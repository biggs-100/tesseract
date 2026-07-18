// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tesseract_common::error::Result;

use crate::jump_hash::jump_hash;
use tesseract_core::types::VectorId;

/// Default number of shards for the cluster.
pub const NUM_SHARDS: u64 = 64;

/// Shard assignment: which node is responsible for which shard role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardAssignment {
    pub shard_id: u64,
    pub leader: String,
    pub replicas: Vec<String>,
}

/// Manages shard-to-node assignments.
///
/// Maintains an in-memory map of shard → leader + replicas. The map can be
/// serialized to/from JSON for etcd storage and cluster-wide synchronization.
#[derive(Debug)]
pub struct ShardManager {
    assignments: HashMap<u64, ShardAssignment>,
    node_id: String,
}

impl ShardManager {
    /// Create a new `ShardManager` for the given local node ID.
    pub fn new(node_id: &str) -> Self {
        Self { assignments: HashMap::new(), node_id: node_id.to_string() }
    }

    /// Compute the shard for a given `VectorId` using JumpHash.
    pub fn shard_for(&self, id: &VectorId) -> u64 {
        jump_hash(id.0, NUM_SHARDS)
    }

    /// Get the leader node for a shard, if assigned.
    pub fn get_leader(&self, shard_id: u64) -> Option<&str> {
        self.assignments.get(&shard_id).map(|a| a.leader.as_str())
    }

    /// Assign a shard to a leader node.
    ///
    /// Creates or replaces the assignment for the given shard. Any existing
    /// replicas are preserved if the assignment already exists, otherwise
    /// the replicas list starts empty.
    pub fn assign_shard(&mut self, shard_id: u64, leader: &str) -> Result<()> {
        let replicas = self.assignments.get(&shard_id).map(|a| a.replicas.clone()).unwrap_or_default();

        self.assignments.insert(shard_id, ShardAssignment { shard_id, leader: leader.to_string(), replicas });
        Ok(())
    }

    /// Add a replica node for a shard.
    ///
    /// # Errors
    ///
    /// Returns `ShardNotAssigned` if the shard has no leader assignment.
    pub fn add_replica(&mut self, shard_id: u64, replica: &str) -> Result<()> {
        let assignment =
            self.assignments.get_mut(&shard_id).ok_or(tesseract_common::error::Error::ShardNotAssigned(shard_id))?;

        if !assignment.replicas.contains(&replica.to_string()) {
            assignment.replicas.push(replica.to_string());
        }
        Ok(())
    }

    /// Remove a node from all assignments, both as leader and as replica.
    ///
    /// Returns the list of shard IDs whose assignment was modified.
    pub fn remove_node(&mut self, node_id: &str) -> Vec<u64> {
        let mut affected = Vec::new();

        // Collect shards where this node is leader or replica.
        let leader_shards: Vec<u64> =
            self.assignments.iter().filter(|(_, a)| a.leader == node_id).map(|(&id, _)| id).collect();

        let replica_shards: Vec<u64> = self
            .assignments
            .iter()
            .filter(|(_, a)| a.replicas.contains(&node_id.to_string()))
            .map(|(&id, _)| id)
            .collect();

        // Remove node as leader — drop the entire assignment.
        for shard_id in &leader_shards {
            self.assignments.remove(shard_id);
            affected.push(*shard_id);
        }

        // Remove node as replica from remaining assignments.
        for shard_id in &replica_shards {
            if let Some(assignment) = self.assignments.get_mut(shard_id) {
                assignment.replicas.retain(|r| r != node_id);
                if !affected.contains(shard_id) {
                    affected.push(*shard_id);
                }
            }
        }

        affected
    }

    /// List all shard assignments owned by this node (as leader or replica).
    pub fn my_shards(&self) -> Vec<&ShardAssignment> {
        self.assignments.values().filter(|a| a.leader == self.node_id || a.replicas.contains(&self.node_id)).collect()
    }

    /// Serialize all assignments to a JSON string for etcd storage.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.assignments).unwrap_or_else(|_| "{}".into())
    }

    /// Deserialize assignments from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a `ServiceError` if the JSON is malformed.
    pub fn from_json(json: &str, node_id: &str) -> Result<Self> {
        let assignments: HashMap<u64, ShardAssignment> = serde_json::from_str(json).map_err(|e| {
            tesseract_common::error::Error::ServiceError(format!("failed to parse shard assignments: {e}"))
        })?;

        Ok(Self { assignments, node_id: node_id.to_string() })
    }

    /// Return all assigned (shard_id, leader_node_id) pairs.
    pub fn assigned_leaders(&self) -> Vec<(u64, String)> {
        self.assignments.iter().map(|(&shard_id, a)| (shard_id, a.leader.clone())).collect()
    }

    /// Return the number of assigned shards.
    pub fn len(&self) -> usize {
        self.assignments.len()
    }

    /// Return `true` if no shards are assigned.
    pub fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tesseract_common::error::Error;

    #[test]
    fn assign_and_get_leader() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap();
        sm.assign_shard(1, "node-b").unwrap();

        assert_eq!(sm.get_leader(0), Some("node-a"));
        assert_eq!(sm.get_leader(1), Some("node-b"));
    }

    #[test]
    fn get_leader_unassigned_returns_none() {
        let sm = ShardManager::new("node-a");
        assert_eq!(sm.get_leader(99), None);
    }

    #[test]
    fn remove_node_leader_reassigns_shards() {
        let mut sm = ShardManager::new("node-b");
        sm.assign_shard(0, "node-a").unwrap();
        sm.assign_shard(1, "node-b").unwrap();
        sm.assign_shard(2, "node-a").unwrap();

        let mut affected = sm.remove_node("node-a");
        affected.sort();
        assert_eq!(affected, vec![0, 2], "should report shards 0 and 2");
        assert_eq!(sm.get_leader(0), None, "shard 0 leader should be removed");
        assert_eq!(sm.get_leader(2), None, "shard 2 leader should be removed");
        assert_eq!(sm.get_leader(1), Some("node-b"), "shard 1 should be unchanged");
    }

    #[test]
    fn remove_node_replica_cleans_replicas() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap();
        sm.add_replica(0, "node-b").unwrap();
        sm.add_replica(0, "node-c").unwrap();

        let affected = sm.remove_node("node-b");
        assert_eq!(affected, vec![0], "shard 0 should be affected");

        let assignment = sm.assignments.get(&0).unwrap();
        assert_eq!(assignment.replicas, vec!["node-c"]);
    }

    #[test]
    fn add_replica_unassigned_shard_errors() {
        let mut sm = ShardManager::new("node-a");
        let err = sm.add_replica(99, "node-b").unwrap_err();
        assert!(matches!(err, Error::ShardNotAssigned(99)), "expected ShardNotAssigned(99), got {err:?}");
    }

    #[test]
    fn add_replica_duplicate_is_idempotent() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap();
        sm.add_replica(0, "node-b").unwrap();
        sm.add_replica(0, "node-b").unwrap(); // duplicate

        let assignment = sm.assignments.get(&0).unwrap();
        assert_eq!(assignment.replicas.len(), 1);
    }

    #[test]
    fn my_shards_returns_owned_and_replica_shards() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap(); // leader
        sm.assign_shard(1, "node-b").unwrap();
        sm.add_replica(1, "node-a").unwrap(); // replica
        sm.assign_shard(2, "node-c").unwrap();

        let mine: Vec<u64> = sm.my_shards().iter().map(|a| a.shard_id).collect();
        assert!(mine.contains(&0));
        assert!(mine.contains(&1));
        assert!(!mine.contains(&2));
    }

    #[test]
    fn json_roundtrip() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap();
        sm.assign_shard(1, "node-b").unwrap();
        sm.add_replica(0, "node-b").unwrap();
        sm.add_replica(0, "node-c").unwrap();

        let json = sm.to_json();
        let restored = ShardManager::from_json(&json, "node-a").unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get_leader(0), Some("node-a"));
        assert_eq!(restored.get_leader(1), Some("node-b"));

        let a0 = restored.assignments.get(&0).unwrap();
        assert_eq!(a0.replicas, vec!["node-b", "node-c"]);
    }

    #[test]
    fn empty_shard_manager() {
        let sm = ShardManager::new("node-a");
        assert!(sm.is_empty());
        assert_eq!(sm.len(), 0);
        assert_eq!(sm.to_json(), "{}");
    }

    #[test]
    fn from_json_empty() {
        let sm = ShardManager::from_json("{}", "node-a").unwrap();
        assert!(sm.is_empty());
    }

    #[test]
    fn from_json_invalid() {
        let err = ShardManager::from_json("not-json", "node-a").unwrap_err();
        assert!(matches!(err, Error::ServiceError(_)), "expected ServiceError, got {err:?}");
    }

    #[test]
    fn shard_for_vector_id() {
        let sm = ShardManager::new("node-a");
        let id = VectorId(42);
        let shard = sm.shard_for(&id);
        assert!(shard < NUM_SHARDS, "shard {shard} out of range");

        // Deterministic
        assert_eq!(sm.shard_for(&id), shard);
    }

    #[test]
    fn assign_shard_preserves_replicas() {
        let mut sm = ShardManager::new("node-a");
        sm.assign_shard(0, "node-a").unwrap();
        sm.add_replica(0, "node-b").unwrap();

        // Re-assign with new leader — replicas should be preserved.
        sm.assign_shard(0, "node-c").unwrap();
        assert_eq!(sm.get_leader(0), Some("node-c"));
        let a0 = sm.assignments.get(&0).unwrap();
        assert_eq!(a0.replicas, vec!["node-b"]);
    }
}
