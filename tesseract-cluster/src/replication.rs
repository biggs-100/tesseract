// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Async WAL replication engine.
//!
//! Each shard leader runs a [`ReplicationEngine`] that tracks replicas and
//! their acknowledgment state. New WAL entries are queued and streamed to
//! followers asynchronously. Followers acknowledge receipt, and the leader
//! updates the replication watermark accordingly.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tesseract_common::error::Result;

// ─── Replica State ───────────────────────────────────────

/// Replication state for a single replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicaState {
    /// Fully synced with leader — no pending entries.
    Synced,
    /// Catching up — has missed some entries.
    Lagging(u64),
    /// Disconnected — replica is unreachable or too far behind.
    Disconnected,
}

// ─── Replication Entry ───────────────────────────────────

/// A single replication entry (WAL entry wrapper for wire transfer).
///
/// Mirrors the on-disk WAL format but adds a `shard_id` so the follower
/// can route entries to the correct shard local state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationEntry {
    pub txn_id: u64,
    pub shard_id: u64,
    pub op_code: u8,
    pub payload: Vec<u8>,
}

// ─── Replica Info ────────────────────────────────────────

/// Replica information tracked by the leader.
#[derive(Debug, Clone)]
pub struct ReplicaInfo {
    pub node_id: String,
    pub addr: String,
    pub state: ReplicaState,
    pub last_acked_txn_id: u64,
    pub last_heartbeat: Instant,
}

// ─── Configuration ───────────────────────────────────────

/// Configuration for the replication engine.
#[derive(Debug, Clone)]
pub struct ReplicationConfig {
    /// Max entries to batch in one replication request.
    pub batch_size: usize,
    /// Interval between replication attempts (ms).
    pub replication_interval_ms: u64,
    /// Timeout for follower ack (ms).
    pub ack_timeout_ms: u64,
    /// Maximum lag before marking replica as Disconnected.
    pub max_lag_entries: u64,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self { batch_size: 100, replication_interval_ms: 50, ack_timeout_ms: 1000, max_lag_entries: 10000 }
    }
}

// ─── Replication Engine ──────────────────────────────────

/// Replication manager running on each shard leader.
///
/// Thread-safe: all mutation goes through internal `RwLock`s so callers
/// can share a single `ReplicationEngine` behind an `Arc`.
pub struct ReplicationEngine {
    shard_id: u64,
    replicas: RwLock<HashMap<String, ReplicaInfo>>,
    pending_entries: RwLock<Vec<ReplicationEntry>>,
    config: ReplicationConfig,
}

impl ReplicationEngine {
    /// Create a new `ReplicationEngine` for the given shard.
    pub fn new(shard_id: u64, _node_id: &str, config: ReplicationConfig) -> Self {
        Self {
            shard_id,
            replicas: RwLock::new(HashMap::new()),
            pending_entries: RwLock::new(Vec::new()),
            config,
        }
    }

    /// Return the shard ID this engine manages.
    pub fn shard_id(&self) -> u64 {
        self.shard_id
    }

    /// Return the replication config.
    pub fn config(&self) -> &ReplicationConfig {
        &self.config
    }

    // ── Replica lifecycle ────────────────────────────────

    /// Add a replica for this shard.
    pub fn add_replica(&self, node_id: &str, addr: &str) {
        let mut replicas = self.replicas.write().expect("replicas lock poisoned");
        replicas.insert(
            node_id.to_string(),
            ReplicaInfo {
                node_id: node_id.to_string(),
                addr: addr.to_string(),
                state: ReplicaState::Synced,
                last_acked_txn_id: 0,
                last_heartbeat: Instant::now(),
            },
        );
    }

    /// Remove a replica.
    pub fn remove_replica(&self, node_id: &str) {
        let mut replicas = self.replicas.write().expect("replicas lock poisoned");
        replicas.remove(node_id);
    }

    /// Check if a replica is registered.
    pub fn has_replica(&self, node_id: &str) -> bool {
        let replicas = self.replicas.read().expect("replicas lock poisoned");
        replicas.contains_key(node_id)
    }

    /// Return the number of tracked replicas.
    pub fn replica_count(&self) -> usize {
        let replicas = self.replicas.read().expect("replicas lock poisoned");
        replicas.len()
    }

    /// Return a copy of the replica info for a given node, if registered.
    pub fn replica_info(&self, node_id: &str) -> Option<ReplicaInfo> {
        let replicas = self.replicas.read().expect("replicas lock poisoned");
        replicas.get(node_id).cloned()
    }

    // ── Entry tracking ───────────────────────────────────

    /// Record a new WAL entry for replication.
    ///
    /// Appends the entry to the pending queue and updates replica states
    /// based on current lag. This is the non-blocking hot path — callers
    /// should invoke this immediately after the local WAL append completes.
    pub fn record_entry(&self, entry: ReplicationEntry) -> Result<()> {
        let entry_txn_id = entry.txn_id;

        {
            let mut entries = self.pending_entries.write().expect("pending_entries lock poisoned");
            entries.push(entry);
        }

        // Determine the highest known txn_id for lag calculation.
        let max_txn_id = {
            let entries = self.pending_entries.read().expect("pending_entries lock poisoned");
            entries.last().map(|e| e.txn_id).unwrap_or(entry_txn_id)
        };

        // Update each replica's state based on current lag.
        let mut replicas = self.replicas.write().expect("replicas lock poisoned");
        for replica in replicas.values_mut() {
            let lag = max_txn_id.saturating_sub(replica.last_acked_txn_id);
            replica.state = if lag > self.config.max_lag_entries {
                ReplicaState::Disconnected
            } else if lag > 0 {
                ReplicaState::Lagging(lag)
            } else {
                // Lag is zero — replica is fully up to date.
                // Keep the previous state (likely Synced).
                if replica.state == ReplicaState::Disconnected && lag == 0 {
                    // Reconnected and caught up.
                    replica.state = ReplicaState::Synced;
                }
                replica.state.clone()
            };
        }

        Ok(())
    }

    /// Get pending entries that need to be sent to a specific replica.
    ///
    /// Returns entries with `txn_id > last_acked_txn_id` for the given
    /// replica. Returns an empty vec if the replica is not registered.
    pub fn pending_for_replica(&self, replica_id: &str) -> Vec<ReplicationEntry> {
        let last_acked = {
            let replicas = self.replicas.read().expect("replicas lock poisoned");
            match replicas.get(replica_id) {
                Some(info) => info.last_acked_txn_id,
                None => return Vec::new(),
            }
        };

        let entries = self.pending_entries.read().expect("pending_entries lock poisoned");
        entries.iter().filter(|e| e.txn_id > last_acked).cloned().collect()
    }

    /// Return the total number of pending (unacked) entries across all replicas.
    pub fn pending_count(&self) -> usize {
        let entries = self.pending_entries.read().expect("pending_entries lock poisoned");
        entries.len()
    }

    // ── Acknowledgment ───────────────────────────────────

    /// Mark a txn_id as acknowledged by a replica.
    ///
    /// Updates the replica's `last_acked_txn_id`, resets its heartbeat,
    /// and recalculates its state based on remaining lag.
    pub fn ack(&self, replica_id: &str, txn_id: u64) {
        let max_txn_id = {
            let entries = self.pending_entries.read().expect("pending_entries lock poisoned");
            entries.last().map(|e| e.txn_id).unwrap_or(0)
        };

        let mut replicas = self.replicas.write().expect("replicas lock poisoned");
        if let Some(replica) = replicas.get_mut(replica_id) {
            if txn_id > replica.last_acked_txn_id {
                replica.last_acked_txn_id = txn_id;
            }
            replica.last_heartbeat = Instant::now();

            let lag = max_txn_id.saturating_sub(replica.last_acked_txn_id);
            if lag == 0 {
                replica.state = ReplicaState::Synced;
            } else if lag > self.config.max_lag_entries {
                replica.state = ReplicaState::Disconnected;
            } else {
                replica.state = ReplicaState::Lagging(lag);
            }
        }
    }

    // ── Monitoring ───────────────────────────────────────

    /// Get replication lag for a replica.
    ///
    /// Returns `None` if the replica is not registered. Lag is computed as
    /// `(highest known txn_id) - (replica's last_acked_txn_id)`.
    pub fn replication_lag(&self, replica_id: &str) -> Option<u64> {
        let replicas = self.replicas.read().expect("replicas lock poisoned");
        let info = replicas.get(replica_id)?;

        let max_txn_id = {
            let entries = self.pending_entries.read().expect("pending_entries lock poisoned");
            entries.last().map(|e| e.txn_id).unwrap_or(0)
        };

        Some(max_txn_id.saturating_sub(info.last_acked_txn_id))
    }

    /// Get state of all replicas.
    pub fn replica_states(&self) -> Vec<(String, ReplicaState)> {
        let replicas = self.replicas.read().expect("replicas lock poisoned");
        replicas.values().map(|r| (r.node_id.clone(), r.state.clone())).collect()
    }

    // ── Maintenance ──────────────────────────────────────

    /// Clean up acknowledged entries (called periodically).
    ///
    /// Removes all pending entries whose `txn_id <= min(all_replicas.last_acked_txn_id)`.
    /// If no replicas exist, this is a no-op.
    pub fn trim_acked(&self) {
        let min_acked = {
            let replicas = self.replicas.read().expect("replicas lock poisoned");
            replicas.values().map(|r| r.last_acked_txn_id).min().unwrap_or(0)
        };

        let mut entries = self.pending_entries.write().expect("pending_entries lock poisoned");
        entries.retain(|e| e.txn_id > min_acked);
    }
}

// ─── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a `ReplicationEntry` with the given txn_id.
    fn entry(txn_id: u64) -> ReplicationEntry {
        ReplicationEntry { txn_id, shard_id: 0, op_code: 0x01, payload: vec![] }
    }

    /// Helper: create a default engine with two followers.
    fn engine_with_followers() -> ReplicationEngine {
        let engine = ReplicationEngine::new(0, "leader-1", ReplicationConfig::default());
        engine.add_replica("follower-1", "127.0.0.1:9002");
        engine.add_replica("follower-2", "127.0.0.1:9003");
        engine
    }

    /// Helper: record N entries in sequence.
    fn record_n(engine: &ReplicationEngine, n: u64) {
        for i in 1..=n {
            engine.record_entry(entry(i)).unwrap();
        }
    }

    // ── 1. Add replica ───────────────────────────────────

    #[test]
    fn add_replica_exists() {
        let engine = ReplicationEngine::new(0, "leader-1", ReplicationConfig::default());
        engine.add_replica("follower-1", "127.0.0.1:9002");

        assert!(engine.has_replica("follower-1"));
        assert_eq!(engine.replica_count(), 1);
    }

    // ── 2. Record entry → pending returns it ─────────────

    #[test]
    fn record_entry_pending() {
        let engine = engine_with_followers();
        engine.record_entry(entry(1)).unwrap();

        let pending = engine.pending_for_replica("follower-1");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].txn_id, 1);
    }

    // ── 3. Ack updates last_acked_txn_id ─────────────────

    #[test]
    fn ack_updates_last_acked() {
        let engine = engine_with_followers();
        record_n(&engine, 5);

        // Ack up to txn_id 3.
        engine.ack("follower-1", 3);

        let pending = engine.pending_for_replica("follower-1");
        assert_eq!(pending.len(), 2); // entries 4, 5 remain
        assert_eq!(pending[0].txn_id, 4);
        assert_eq!(pending[1].txn_id, 5);

        // Ack the rest.
        engine.ack("follower-1", 5);
        let pending = engine.pending_for_replica("follower-1");
        assert!(pending.is_empty());
    }

    // ── 4. Replication lag calculates correctly ──────────

    #[test]
    fn replication_lag_calculates() {
        let engine = engine_with_followers();
        record_n(&engine, 5);

        // Ack only up to 3.
        engine.ack("follower-1", 3);

        let lag = engine.replication_lag("follower-1").unwrap();
        assert_eq!(lag, 2); // 5 - 3 = 2

        // Follower-2 hasn't acked anything.
        let lag_f2 = engine.replication_lag("follower-2").unwrap();
        assert_eq!(lag_f2, 5);
    }

    // ── 5. Trim acked removes entries below all acks ─────

    #[test]
    fn trim_acked_removes_below_min() {
        let engine = engine_with_followers();
        record_n(&engine, 5);

        engine.ack("follower-1", 5);
        engine.ack("follower-2", 3);

        engine.trim_acked();

        // After trim, entries 1-3 should be removed (min ack = 3).
        let pending_f2 = engine.pending_for_replica("follower-2");
        assert_eq!(pending_f2.len(), 2);
        assert_eq!(pending_f2[0].txn_id, 4);
        assert_eq!(pending_f2[1].txn_id, 5);

        // Follower-1 acked everything, so its pending should be empty.
        let pending_f1 = engine.pending_for_replica("follower-1");
        assert!(pending_f1.is_empty());
    }

    // ── 6. Pending for replica filters by last ack ───────

    #[test]
    fn pending_for_replica_filters_by_ack() {
        let engine = engine_with_followers();
        record_n(&engine, 10);

        engine.ack("follower-1", 7);
        let pending = engine.pending_for_replica("follower-1");
        assert_eq!(pending.len(), 3); // 8, 9, 10
        assert_eq!(pending[0].txn_id, 8);
        assert_eq!(pending[1].txn_id, 9);
        assert_eq!(pending[2].txn_id, 10);
    }

    // ── 7. Max lag detection (mark Disconnected) ─────────

    #[test]
    fn max_lag_detection() {
        let config = ReplicationConfig { max_lag_entries: 5, ..Default::default() };
        let engine = ReplicationEngine::new(0, "leader-1", config);
        engine.add_replica("follower-1", "127.0.0.1:9002");

        // Record 6 entries without acking → lag = 6 > 5.
        record_n(&engine, 6);

        let states = engine.replica_states();
        let state = states.iter().find(|(id, _)| id == "follower-1").map(|(_, s)| s);
        assert_eq!(state, Some(&ReplicaState::Disconnected));
    }

    // ── 8. Reconnection recovers from Disconnected ───────

    #[test]
    fn reconnect_after_disconnect() {
        let config = ReplicationConfig { max_lag_entries: 3, ..Default::default() };
        let engine = ReplicationEngine::new(0, "leader-1", config);
        engine.add_replica("follower-1", "127.0.0.1:9002");

        // Lag exceeds max → Disconnected.
        record_n(&engine, 5);
        let state = engine.replica_states();
        assert_eq!(state.iter().find(|(id, _)| id == "follower-1").map(|(_, s)| s), Some(&ReplicaState::Disconnected));

        // Follower acks everything → should go back to Synced.
        engine.ack("follower-1", 5);
        let state = engine.replica_states();
        assert_eq!(state.iter().find(|(id, _)| id == "follower-1").map(|(_, s)| s), Some(&ReplicaState::Synced));
    }

    // ── 9. Remove replica ────────────────────────────────

    #[test]
    fn remove_replica_cleans_up() {
        let engine = engine_with_followers();
        assert!(engine.has_replica("follower-1"));

        engine.remove_replica("follower-1");
        assert!(!engine.has_replica("follower-1"));
        assert_eq!(engine.replica_count(), 1);
    }

    // ── 10. Replication state transitions ─────────────────

    #[test]
    fn state_transitions_to_lagging() {
        let engine = engine_with_followers();
        record_n(&engine, 3);

        let states = engine.replica_states();
        let state = states.iter().find(|(id, _)| id == "follower-1").map(|(_, s)| s);
        assert_eq!(state, Some(&ReplicaState::Lagging(3)));

        // After acking everything, back to Synced.
        engine.ack("follower-1", 3);
        let states = engine.replica_states();
        let state = states.iter().find(|(id, _)| id == "follower-1").map(|(_, s)| s);
        assert_eq!(state, Some(&ReplicaState::Synced));
    }

    // ── 11. Pending count ────────────────────────────────

    #[test]
    fn pending_count_after_records() {
        let engine = engine_with_followers();
        assert_eq!(engine.pending_count(), 0);

        record_n(&engine, 7);
        assert_eq!(engine.pending_count(), 7);

        engine.trim_acked();
        assert_eq!(engine.pending_count(), 7); // no acks yet
    }

    // ── 12. Empty engine state ───────────────────────────

    #[test]
    fn empty_engine_returns_defaults() {
        let engine = ReplicationEngine::new(5, "leader-1", ReplicationConfig::default());
        assert_eq!(engine.shard_id(), 5);
        assert_eq!(engine.replica_count(), 0);
        assert!(engine.replica_states().is_empty());
        assert_eq!(engine.pending_count(), 0);
    }

    // ── 13. ReplicationEntry + ReplicaState serde ────────

    #[test]
    fn replication_entry_serde_roundtrip() {
        let entry = ReplicationEntry { txn_id: 42, shard_id: 7, op_code: 0x01, payload: vec![1, 2, 3, 4, 5] };

        let json = serde_json::to_string(&entry).unwrap();
        let decoded: ReplicationEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.txn_id, entry.txn_id);
        assert_eq!(decoded.shard_id, entry.shard_id);
        assert_eq!(decoded.op_code, entry.op_code);
        assert_eq!(decoded.payload, entry.payload);
    }

    #[test]
    fn replica_state_serde_roundtrip() {
        let states = vec![ReplicaState::Synced, ReplicaState::Lagging(42), ReplicaState::Disconnected];

        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let decoded: ReplicaState = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, *state);
        }
    }

    // ── 14. Multi-replica independent ack ──────────────────

    #[test]
    fn multi_replica_independent_ack() {
        let engine = engine_with_followers();
        record_n(&engine, 10);

        // Follower-1 acks up to 10, follower-2 acks nothing.
        engine.ack("follower-1", 10);

        // Follower-1 should have no pending.
        assert!(engine.pending_for_replica("follower-1").is_empty());
        // Follower-2 should have all 10.
        assert_eq!(engine.pending_for_replica("follower-2").len(), 10);
    }

    // ── 15. No replicas → pending returns empty ──────────

    #[test]
    fn pending_for_unknown_replica_returns_empty() {
        let engine = ReplicationEngine::new(0, "leader-1", ReplicationConfig::default());
        record_n(&engine, 5);

        let pending = engine.pending_for_replica("nonexistent");
        assert!(pending.is_empty());
    }
}
