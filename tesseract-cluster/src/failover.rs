// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Leader failover management for shard-based clusters.
//!
//! The [`FailoverManager`] coordinates leader failure detection, candidate
//! eligibility checks, and follower promotion. It works in conjunction with
//! [`LeaderElection`](crate::leader_election::LeaderElection) for per-shard
//! election state and [`ReplicationEngine`](crate::replication::ReplicationEngine)
//! for replication lag tracking.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tesseract_common::error::{Error, Result};

use crate::leader_election::LeaderElection;
use crate::replication::ReplicationEngine;
use crate::shard_manager::ShardManager;

/// Configuration for failover behavior.
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// How long without leader heartbeat before triggering election (ms).
    pub election_timeout_ms: u64,
    /// How often to check for leader failures (ms).
    pub check_interval_ms: u64,
    /// Max replication lag for a follower to be eligible for promotion.
    pub max_promotion_lag: u64,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self { election_timeout_ms: 3000, check_interval_ms: 500, max_promotion_lag: 100 }
    }
}

/// Manages leader failure detection and follower promotion.
///
/// Thread-safe: all mutable state is behind `Arc` or `RwLock`. The
/// [`start`](FailoverManager::start) method spawns a background task
/// that periodically invokes [`check_and_failover`](FailoverManager::check_and_failover).
pub struct FailoverManager {
    /// Shared leader election state.
    election: Arc<LeaderElection>,
    /// Failover configuration.
    config: FailoverConfig,
    /// This node's ID.
    node_id: String,
    /// Shard manager — updated when this node is promoted to leader.
    shard_manager: Arc<RwLock<ShardManager>>,
    /// Per-shard replication engines for eligibility checks.
    /// (Not in the public API — used internally by `check_and_failover`.)
    replications: Arc<RwLock<HashMap<u64, ReplicationEngine>>>,
}

impl FailoverManager {
    /// Create a new `FailoverManager`.
    ///
    /// A [`LeaderElection`] is created internally with the same
    /// `election_timeout_ms` from `config`.
    pub fn new(node_id: &str, config: FailoverConfig, shard_manager: Arc<RwLock<ShardManager>>) -> Self {
        let election = Arc::new(LeaderElection::new(node_id, config.election_timeout_ms));
        Self {
            election,
            config,
            node_id: node_id.to_string(),
            shard_manager,
            replications: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a reference to the shared [`LeaderElection`].
    pub fn election(&self) -> &Arc<LeaderElection> {
        &self.election
    }

    /// Register a [`ReplicationEngine`] for a shard.
    ///
    /// This is used by [`check_and_failover`](FailoverManager::check_and_failover)
    /// to determine promotion eligibility. Per-shard replication engines
    /// should be registered when this node starts following a shard.
    pub fn set_replication_engine(&self, shard_id: u64, replication: ReplicationEngine) {
        let mut replications = self.replications.write().expect("replications lock poisoned");
        replications.insert(shard_id, replication);
    }

    /// Remove the [`ReplicationEngine`] for a shard.
    pub fn remove_replication_engine(&self, shard_id: u64) {
        let mut replications = self.replications.write().expect("replications lock poisoned");
        replications.remove(&shard_id);
    }

    /// Start the failover monitoring loop.
    ///
    /// Spawns a background task that periodically calls
    /// [`check_and_failover`](FailoverManager::check_and_failover).
    /// Returns a `JoinHandle` that can be aborted to stop monitoring.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let election = self.election.clone();
        let config = self.config.clone();
        let node_id = self.node_id.clone();
        let sm = self.shard_manager.clone();
        let replications = self.replications.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(config.check_interval_ms));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                let mut promoted = Vec::new();
                let now = Instant::now();
                let followerships = election.my_followerships();

                for (shard_id, _leader_id) in followerships {
                    if !election.has_leader_timed_out(shard_id, now) {
                        continue;
                    }

                    // Check promotion eligibility via replication lag.
                    let eligible = match replications.read() {
                        Ok(r) => r.get(&shard_id).is_some_and(|rep| {
                            rep.replication_lag(&node_id).unwrap_or(u64::MAX) <= config.max_promotion_lag
                        }),
                        Err(_) => false,
                    };

                    if !eligible {
                        continue;
                    }

                    // Start election and promote.
                    election.become_candidate(shard_id);
                    if election.win_election(shard_id).is_ok() {
                        if let Ok(mut sm) = sm.write() {
                            let _ = sm.assign_shard(shard_id, &node_id);
                        }
                        promoted.push(shard_id);
                    }
                }

                if !promoted.is_empty() {
                    tracing::info!("failover: node {node_id} promoted for shards {promoted:?}");
                }
            }
        })
    }

    /// Check all shards for leader failures and promote eligible followers.
    ///
    /// For each shard where this node is a follower and the leader has
    /// timed out:
    /// 1. Check eligibility via replication lag
    /// 2. Become a candidate
    /// 3. Win the election
    /// 4. Update the shard manager
    ///
    /// Returns the list of shard IDs where this node was promoted.
    pub async fn check_and_failover(&self) -> Result<Vec<u64>> {
        let mut promoted = Vec::new();
        let now = Instant::now();
        let followerships = self.election.my_followerships();

        for (shard_id, _leader_id) in followerships {
            if !self.election.has_leader_timed_out(shard_id, now) {
                continue;
            }

            // Check promotion eligibility.
            let eligible = {
                let replications = self
                    .replications
                    .read()
                    .map_err(|e| Error::ServiceError(format!("replications lock poisoned: {e}")))?;
                replications.get(&shard_id).is_some_and(|rep| {
                    rep.replication_lag(&self.node_id).unwrap_or(u64::MAX) <= self.config.max_promotion_lag
                })
            };

            if !eligible {
                continue;
            }

            // Start election and promote.
            self.election.become_candidate(shard_id);
            self.election.win_election(shard_id)?;

            {
                let mut sm = self
                    .shard_manager
                    .write()
                    .map_err(|e| Error::ServiceError(format!("shard_manager lock poisoned: {e}")))?;
                sm.assign_shard(shard_id, &self.node_id)?;
            }

            promoted.push(shard_id);
            tracing::info!("failover: node {} promoted for shard {shard_id}", self.node_id);
        }

        Ok(promoted)
    }

    /// Promote this node to leader for a shard.
    ///
    /// Checks replication lag eligibility first, then updates the election
    /// state and shard manager.
    ///
    /// Returns `true` if promotion was successful.
    pub async fn promote_to_leader(&self, shard_id: u64) -> Result<bool> {
        // Check eligibility.
        let eligible = {
            let replications = self
                .replications
                .read()
                .map_err(|e| Error::ServiceError(format!("replications lock poisoned: {e}")))?;
            replications.get(&shard_id).is_some_and(|rep| {
                rep.replication_lag(&self.node_id).unwrap_or(u64::MAX) <= self.config.max_promotion_lag
            })
        };

        if !eligible {
            return Ok(false);
        }

        // Become candidate and win.
        self.election.become_candidate(shard_id);
        self.election.win_election(shard_id)?;

        {
            let mut sm = self
                .shard_manager
                .write()
                .map_err(|e| Error::ServiceError(format!("shard_manager lock poisoned: {e}")))?;
            sm.assign_shard(shard_id, &self.node_id)?;
        }

        tracing::info!("failover: node {} promoted to leader for shard {shard_id}", self.node_id);
        Ok(true)
    }

    /// Check if a follower is eligible to become leader for a shard.
    ///
    /// A follower is eligible if its replication lag is within
    /// `max_promotion_lag`.
    pub fn is_eligible_for_promotion(&self, _shard_id: u64, replication: &ReplicationEngine) -> bool {
        replication.replication_lag(&self.node_id).unwrap_or(u64::MAX) <= self.config.max_promotion_lag
    }

    /// Get promotion eligibility summary for all shards this node follows.
    ///
    /// Returns a list of `(shard_id, is_eligible)` tuples.
    pub fn promotion_candidates(&self) -> Vec<(u64, bool)> {
        let followerships = self.election.my_followerships();
        let replications = self.replications.read().ok();

        followerships
            .into_iter()
            .map(|(shard_id, _)| {
                let eligible = replications.as_ref().is_some_and(|r| {
                    r.get(&shard_id).is_some_and(|rep| {
                        rep.replication_lag(&self.node_id).unwrap_or(u64::MAX) <= self.config.max_promotion_lag
                    })
                });
                (shard_id, eligible)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leader_election::ElectionState;
    use crate::replication::{ReplicationConfig, ReplicationEntry};

    /// Helper: create a `FailoverManager` for tests.
    fn make_failover_manager(node_id: &str, config: FailoverConfig) -> FailoverManager {
        let sm = Arc::new(RwLock::new(ShardManager::new(node_id)));
        FailoverManager::new(node_id, config, sm)
    }

    /// Helper: create a [`ReplicationEngine`] that tracks a specific replica
    /// with a given lag.
    fn replication_with_lag(leader_id: &str, follower_id: &str, total_entries: u64, acked: u64) -> ReplicationEngine {
        let engine = ReplicationEngine::new(0, leader_id, ReplicationConfig::default());
        engine.add_replica(follower_id, "127.0.0.1:9002");

        for i in 1..=total_entries {
            engine.record_entry(ReplicationEntry { txn_id: i, shard_id: 0, op_code: 0x01, payload: vec![] }).unwrap();
        }
        engine.ack(follower_id, acked);
        engine
    }

    // ── Test 8: is_eligible_for_promotion with low lag → true ────────

    #[test]
    fn eligible_with_low_lag() {
        let config = FailoverConfig { max_promotion_lag: 100, ..Default::default() };
        let fm = make_failover_manager("node-b", config);

        // ReplicationEngine on the leader tracks node-b with lag=1.
        let replication = replication_with_lag("node-a", "node-b", 10, 9);

        assert!(fm.is_eligible_for_promotion(0, &replication));
    }

    // ── Test 9: is_eligible_for_promotion with high lag → false ──────

    #[test]
    fn not_eligible_with_high_lag() {
        let config = FailoverConfig { max_promotion_lag: 3, ..Default::default() };
        let fm = make_failover_manager("node-b", config);

        // ReplicationEngine tracks node-b with lag=7 (only acked 3 of 10).
        let replication = replication_with_lag("node-a", "node-b", 10, 3);

        assert!(!fm.is_eligible_for_promotion(0, &replication));
    }

    // ── Test 10: check_and_failover promotes eligible leader ─────────

    #[tokio::test]
    async fn check_and_failover_promotes_eligible_leader() {
        let config = FailoverConfig {
            election_timeout_ms: 50, // short timeout for test
            max_promotion_lag: 100,
            ..Default::default()
        };

        let sm = Arc::new(RwLock::new(ShardManager::new("node-b")));
        let fm = FailoverManager::new("node-b", config, sm.clone());

        // Set up election: node-b is follower for shard 0, leader is node-a.
        fm.election().set_leader(0, "node-a");

        // Register replication engine with low lag.
        let replication = replication_with_lag("node-a", "node-b", 5, 5);
        fm.set_replication_engine(0, replication);

        // Since we never heartbeated, and election_timeout_ms=50,
        // has_leader_timed_out should return true with a "now" past 50ms.
        // Use the current time directly.

        // Run failover.
        let promoted = fm.check_and_failover().await.unwrap();
        assert!(promoted.contains(&0), "node-b should be promoted for shard 0");

        // Verify election state.
        assert_eq!(fm.election().state(0), ElectionState::Leader);

        // Verify shard manager updated.
        let sm_read = sm.read().unwrap();
        assert_eq!(sm_read.get_leader(0), Some("node-b"));
    }

    // ── Edge: check_and_failover skips non-timed-out leaders ────────

    #[tokio::test]
    async fn check_and_failover_skips_healthy_leaders() {
        let config = FailoverConfig {
            election_timeout_ms: 5000, // long timeout
            max_promotion_lag: 100,
            ..Default::default()
        };

        let sm = Arc::new(RwLock::new(ShardManager::new("node-b")));
        let fm = FailoverManager::new("node-b", config, sm.clone());

        // Set up election with an active leader (recent heartbeat by set_leader).
        fm.election().set_leader(0, "node-a");

        // Run failover — should not promote because leader is still alive.
        let promoted = fm.check_and_failover().await.unwrap();
        assert!(promoted.is_empty(), "should not promote while leader is healthy");
        assert_eq!(fm.election().state(0), ElectionState::Follower { leader_id: "node-a".into() });
    }

    // ── Edge: check_and_failover skips ineligible followers ──────────

    #[tokio::test]
    async fn check_and_failover_skips_ineligible_followers() {
        let config = FailoverConfig {
            election_timeout_ms: 50,
            max_promotion_lag: 3, // strict limit
            ..Default::default()
        };

        let sm = Arc::new(RwLock::new(ShardManager::new("node-b")));
        let fm = FailoverManager::new("node-b", config, sm.clone());

        // Set up election: node-b is follower for shard 0, leader is node-a.
        fm.election().set_leader(0, "node-a");

        // Register replication engine with HIGH lag.
        let replication = replication_with_lag("node-a", "node-b", 10, 1); // lag = 9 > 3
        fm.set_replication_engine(0, replication);

        // Run failover — should skip because lag exceeds max_promotion_lag.
        let promoted = fm.check_and_failover().await.unwrap();
        assert!(promoted.is_empty(), "should not promote ineligible follower");
        assert_eq!(fm.election().state(0), ElectionState::Follower { leader_id: "node-a".into() });
    }

    // ── Edge: promotion_candidates returns correct eligibility ───────

    #[test]
    fn promotion_candidates_returns_eligibility() {
        let config = FailoverConfig { max_promotion_lag: 5, ..Default::default() };
        let fm = make_failover_manager("node-b", config);

        fm.election().set_leader(0, "node-a");
        fm.election().set_leader(1, "node-c");

        // Shard 0: low lag → eligible. Shard 1: high lag → ineligible.
        let rep0 = replication_with_lag("node-a", "node-b", 10, 10); // lag = 0 ≤ 5
        let rep1 = replication_with_lag("node-c", "node-b", 10, 1); // lag = 9 > 5

        fm.set_replication_engine(0, rep0);
        fm.set_replication_engine(1, rep1);

        let candidates = fm.promotion_candidates();
        assert_eq!(candidates.len(), 2);

        let map: HashMap<u64, bool> = candidates.into_iter().collect();
        assert!(map.get(&0).unwrap(), "shard 0 should be eligible");
        assert!(!map.get(&1).unwrap(), "shard 1 should not be eligible");
    }

    // ── Edge: promote_to_leader with eligible follower ───────────────

    #[tokio::test]
    async fn promote_to_leader_with_eligible_follower() {
        let config = FailoverConfig { max_promotion_lag: 100, ..Default::default() };
        let sm = Arc::new(RwLock::new(ShardManager::new("node-b")));
        let fm = FailoverManager::new("node-b", config, sm.clone());

        // Register replication with low lag.
        let replication = replication_with_lag("node-a", "node-b", 10, 10);
        fm.set_replication_engine(0, replication);

        let result = fm.promote_to_leader(0).await.unwrap();
        assert!(result, "promotion should succeed");

        assert_eq!(fm.election().state(0), ElectionState::Leader);
        assert_eq!(sm.read().unwrap().get_leader(0), Some("node-b"));
    }

    // ── Edge: promote_to_leader with ineligible follower → false ─────

    #[tokio::test]
    async fn promote_to_leader_with_ineligible_follower() {
        let config = FailoverConfig { max_promotion_lag: 3, ..Default::default() };
        let sm = Arc::new(RwLock::new(ShardManager::new("node-b")));
        let fm = FailoverManager::new("node-b", config, sm.clone());

        // Register replication with high lag.
        let replication = replication_with_lag("node-a", "node-b", 10, 1); // lag = 9
        fm.set_replication_engine(0, replication);

        let result = fm.promote_to_leader(0).await.unwrap();
        assert!(!result, "promotion should be declined for ineligible follower");
    }
}
