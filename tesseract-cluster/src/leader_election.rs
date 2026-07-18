// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! In-memory leader election for shard failover.
//!
//! Each shard in the cluster has a single leader at any time. Followers
//! monitor the leader via heartbeats. When a leader fails (heartbeat
//! timeout), followers trigger an election and one is promoted.
//!
//! This is an in-memory implementation suitable for testing and
//! single-process deployments. Production deployments should use an
//! etcd-based election backed by leases and campaigns.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use tesseract_common::error::{Error, Result};

/// Leader election state for a shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElectionState {
    /// This node is the leader for this shard.
    Leader,
    /// This node is a candidate (trying to become leader).
    Candidate,
    /// This node is a follower of the given leader.
    Follower { leader_id: String },
    /// No leader elected yet.
    NoLeader,
}

/// In-memory leader election manager.
///
/// Each shard has one leader. Followers detect leader failure via heartbeat
/// timeouts and trigger elections. Thread-safe: all mutation goes through
/// internal `RwLock`s.
pub struct LeaderElection {
    /// Per-shard election state.
    shard_states: RwLock<HashMap<u64, ElectionState>>,
    /// This node's ID.
    node_id: String,
    /// Election timeout in milliseconds.
    election_timeout_ms: u64,
    /// Time of last leader heartbeat per shard.
    last_leader_heartbeat: RwLock<HashMap<u64, Instant>>,
}

impl LeaderElection {
    /// Create a new `LeaderElection` for the given node.
    ///
    /// `election_timeout_ms` is the duration after which a leader is
    /// considered dead if no heartbeat is received.
    pub fn new(node_id: &str, election_timeout_ms: u64) -> Self {
        Self {
            shard_states: RwLock::new(HashMap::new()),
            node_id: node_id.to_string(),
            election_timeout_ms,
            last_leader_heartbeat: RwLock::new(HashMap::new()),
        }
    }

    /// Return this node's ID.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Return the election timeout in milliseconds.
    pub fn election_timeout_ms(&self) -> u64 {
        self.election_timeout_ms
    }

    /// Get the election state for a shard.
    ///
    /// Returns [`ElectionState::NoLeader`] if the shard has no state.
    pub fn state(&self, shard_id: u64) -> ElectionState {
        let states = self.shard_states.read().expect("shard_states lock poisoned");
        states.get(&shard_id).cloned().unwrap_or(ElectionState::NoLeader)
    }

    /// Set a shard's leader (called when this node learns of a leader).
    ///
    /// Transition: any state → [`Follower`](ElectionState::Follower).
    /// Does NOT record a heartbeat — call
    /// [`record_leader_heartbeat`](LeaderElection::record_leader_heartbeat)
    /// separately when a heartbeat is received from the leader.
    pub fn set_leader(&self, shard_id: u64, leader_id: &str) {
        let mut states = self.shard_states.write().expect("shard_states lock poisoned");
        states.insert(shard_id, ElectionState::Follower { leader_id: leader_id.to_string() });
    }

    /// Become a candidate for a shard (start an election).
    ///
    /// Transition: any state → [`Candidate`](ElectionState::Candidate).
    pub fn become_candidate(&self, shard_id: u64) {
        let mut states = self.shard_states.write().expect("shard_states lock poisoned");
        states.insert(shard_id, ElectionState::Candidate);
    }

    /// Win the election for a shard.
    ///
    /// Transition: [`Candidate`](ElectionState::Candidate) → [`Leader`](ElectionState::Leader).
    ///
    /// # Errors
    ///
    /// Returns `ServiceError` if the shard is not in [`Candidate`](ElectionState::Candidate)
    /// or [`Leader`](ElectionState::Leader) state.
    pub fn win_election(&self, shard_id: u64) -> Result<()> {
        let mut states =
            self.shard_states.write().map_err(|e| Error::ServiceError(format!("shard_states lock poisoned: {e}")))?;

        match states.get(&shard_id) {
            Some(ElectionState::Candidate) => {
                states.insert(shard_id, ElectionState::Leader);
                let mut heartbeats = self
                    .last_leader_heartbeat
                    .write()
                    .map_err(|e| Error::ServiceError(format!("heartbeat lock poisoned: {e}")))?;
                heartbeats.insert(shard_id, Instant::now());
                Ok(())
            }
            Some(ElectionState::Leader) => {
                // Already leader — idempotent.
                Ok(())
            }
            Some(state) => Err(Error::ServiceError(format!(
                "Cannot win election for shard {shard_id}: current state is {state:?}, expected Candidate"
            ))),
            None => {
                Err(Error::ServiceError(format!("Cannot win election for shard {shard_id}: no election state exists")))
            }
        }
    }

    /// Record a heartbeat from the leader of a shard.
    ///
    /// Resets the timeout timer for the given shard. This should be called
    /// whenever this node (as a follower) receives a health check or
    /// replication entry from the shard leader.
    pub fn record_leader_heartbeat(&self, shard_id: u64) {
        let mut heartbeats = self.last_leader_heartbeat.write().expect("heartbeat lock poisoned");
        heartbeats.insert(shard_id, Instant::now());
    }

    /// Check if a shard leader has timed out.
    ///
    /// Returns `true` if no heartbeat was received within
    /// `election_timeout_ms`. If no heartbeat has ever been recorded
    /// for the shard, returns `true` (timed out by default).
    pub fn has_leader_timed_out(&self, shard_id: u64, now: Instant) -> bool {
        let heartbeats = self.last_leader_heartbeat.read().expect("heartbeat lock poisoned");
        match heartbeats.get(&shard_id) {
            Some(last) => now.duration_since(*last) > Duration::from_millis(self.election_timeout_ms),
            None => true, // No heartbeat ever recorded — timed out.
        }
    }

    /// List all shards where this node is the leader.
    pub fn my_leaderships(&self) -> Vec<u64> {
        let states = self.shard_states.read().expect("shard_states lock poisoned");
        states.iter().filter_map(|(&id, state)| matches!(state, ElectionState::Leader).then_some(id)).collect()
    }

    /// List all shards where this node is a follower, with the leader ID.
    pub fn my_followerships(&self) -> Vec<(u64, String)> {
        let states = self.shard_states.read().expect("shard_states lock poisoned");
        states
            .iter()
            .filter_map(|(&id, state)| {
                if let ElectionState::Follower { leader_id } = state { Some((id, leader_id.clone())) } else { None }
            })
            .collect()
    }

    /// Number of shards with a leader elected (either this node or another).
    pub fn elected_count(&self) -> usize {
        let states = self.shard_states.read().expect("shard_states lock poisoned");
        states.values().filter(|s| matches!(s, ElectionState::Leader | ElectionState::Follower { .. })).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: become_candidate → state is Candidate ─────────────────

    #[test]
    fn become_candidate_changes_state_to_candidate() {
        let election = LeaderElection::new("node-a", 3000);
        assert_eq!(election.state(0), ElectionState::NoLeader);

        election.become_candidate(0);
        assert_eq!(election.state(0), ElectionState::Candidate);
    }

    // ── Test 2: win_election → state is Leader ─────────────────────────

    #[test]
    fn win_election_changes_state_to_leader() {
        let election = LeaderElection::new("node-a", 3000);
        election.become_candidate(0);
        election.win_election(0).unwrap();
        assert_eq!(election.state(0), ElectionState::Leader);
    }

    // ── Test 3: set_leader → state is Follower ─────────────────────────

    #[test]
    fn set_leader_changes_state_to_follower() {
        let election = LeaderElection::new("node-b", 3000);
        election.set_leader(0, "node-a");
        assert_eq!(election.state(0), ElectionState::Follower { leader_id: "node-a".into() });
    }

    // ── Test 4: record_heartbeat prevents timeout ──────────────────────

    #[test]
    fn record_heartbeat_prevents_timeout() {
        let election = LeaderElection::new("node-b", 200);
        election.set_leader(0, "node-a");

        // Record heartbeat, then check with a "now" within the timeout.
        election.record_leader_heartbeat(0);
        let now = Instant::now() + Duration::from_millis(100);
        assert!(!election.has_leader_timed_out(0, now), "should not time out within election_timeout");
    }

    // ── Test 5: timeout detection works ─────────────────────────────────

    #[test]
    fn timeout_detection_works() {
        let election = LeaderElection::new("node-b", 50);
        election.set_leader(0, "node-a");

        // Record heartbeat, then check with a "now" past the timeout.
        election.record_leader_heartbeat(0);
        let now = Instant::now() + Duration::from_millis(100);
        assert!(election.has_leader_timed_out(0, now), "should time out after election_timeout");
    }

    // ── Test 6: my_leaderships returns correct shards ──────────────────

    #[test]
    fn my_leaderships_returns_correct_shards() {
        let election = LeaderElection::new("node-a", 3000);

        election.become_candidate(0);
        election.win_election(0).unwrap();
        election.become_candidate(1);
        election.win_election(1).unwrap();

        let mut shards = election.my_leaderships();
        shards.sort_unstable();
        assert_eq!(shards, vec![0, 1], "should list both shards where node-a is leader");
    }

    // ── Test 7: my_followerships returns correct shards ────────────────

    #[test]
    fn my_followerships_returns_correct_shards() {
        let election = LeaderElection::new("node-b", 3000);

        election.set_leader(0, "node-a");
        election.set_leader(1, "node-c");

        let mut followerships = election.my_followerships();
        followerships.sort_by_key(|(id, _)| *id);
        assert_eq!(followerships, vec![(0, "node-a".into()), (1, "node-c".into())]);
    }

    // ── Edge: win_election on non-candidate errors ─────────────────────

    #[test]
    fn win_election_on_non_candidate_errors() {
        let election = LeaderElection::new("node-a", 3000);

        // Shard 0 has no state.
        let err = election.win_election(0).unwrap_err();
        assert!(matches!(err, Error::ServiceError(_)), "expected ServiceError, got {err:?}");

        // Shard 1 is a follower.
        election.set_leader(1, "node-b");
        let err = election.win_election(1).unwrap_err();
        assert!(matches!(err, Error::ServiceError(_)), "expected ServiceError for follower, got {err:?}");
    }

    // ── Edge: win_election on leader is idempotent ─────────────────────

    #[test]
    fn win_election_on_leader_is_idempotent() {
        let election = LeaderElection::new("node-a", 3000);
        election.become_candidate(0);
        election.win_election(0).unwrap();
        // Second win should succeed (idempotent).
        assert!(election.win_election(0).is_ok());
        assert_eq!(election.state(0), ElectionState::Leader);
    }

    // ── Edge: no leader heartbeat → timed out ──────────────────────────

    #[test]
    fn no_heartbeat_is_timed_out() {
        let election = LeaderElection::new("node-b", 3000);

        // Set leader for shard 0 but never record a heartbeat.
        election.set_leader(0, "node-a");

        let now = Instant::now() + Duration::from_millis(5000);
        assert!(election.has_leader_timed_out(0, now), "shard with no heartbeat should be timed out");
        assert!(election.has_leader_timed_out(1, now), "unregistered shard should be timed out");
    }

    // ── Edge: elected_count counts leaders and followers ───────────────

    #[test]
    fn elected_count_counts_leaders_and_followers() {
        let election = LeaderElection::new("node-a", 3000);

        assert_eq!(election.elected_count(), 0);

        election.become_candidate(0);
        election.win_election(0).unwrap(); // leader
        election.set_leader(1, "node-b"); // follower
        election.become_candidate(2); // candidate — not counted

        assert_eq!(election.elected_count(), 2, "should count leader and follower, not candidate");
    }
}
