// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::sync::{Arc, RwLock};

use tesseract_common::error::Result;

use crate::discovery::{NodeInfo, NodeRegistry};
use crate::failover::{FailoverConfig, FailoverManager};
use crate::leader_election::LeaderElection;
use crate::shard_manager::ShardManager;

/// Local cluster node identity.
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub node_id: String,
    pub addr: String,
}

impl NodeIdentity {
    /// Create a new node identity.
    pub fn new(node_id: &str, addr: &str) -> Self {
        Self { node_id: node_id.to_string(), addr: addr.to_string() }
    }
}

/// Cluster membership and state.
///
/// Combines a [`NodeRegistry`] for member tracking, a [`ShardManager`]
/// for shard assignment, a [`LeaderElection`] for per-shard leader state,
/// and a [`FailoverManager`] for automated leader failure recovery.
/// Each [`ClusterState`] instance represents the local node's view of
/// the cluster.
pub struct ClusterState {
    identity: NodeIdentity,
    registry: Arc<NodeRegistry>,
    shards: Arc<RwLock<ShardManager>>,
    pub leader_election: Arc<LeaderElection>,
    pub failover: Arc<FailoverManager>,
}

impl ClusterState {
    /// Create a new `ClusterState` for the given node identity.
    ///
    /// A [`LeaderElection`] with a 3-second election timeout and a
    /// [`FailoverManager`] with default config are created automatically.
    pub fn new(identity: NodeIdentity, registry: NodeRegistry, shard_manager: ShardManager) -> Self {
        let shards = Arc::new(RwLock::new(shard_manager));
        let leader_election = Arc::new(LeaderElection::new(&identity.node_id, 3000));
        let failover_config = FailoverConfig::default();
        let failover = Arc::new(FailoverManager::new(&identity.node_id, failover_config, shards.clone()));

        Self { identity, registry: Arc::new(registry), shards, leader_election, failover }
    }

    /// Register this node in the cluster.
    ///
    /// Creates a [`NodeInfo`] entry in the registry with the local node's
    /// identity and the set of shards currently assigned to it.
    pub fn join(&self) -> Result<()> {
        let shards = self
            .shards
            .read()
            .map_err(|e| tesseract_common::error::Error::ServiceError(format!("shards lock poisoned: {e}")))?;

        let mut owned_shards: Vec<u64> = shards.my_shards().iter().map(|a| a.shard_id).collect();
        owned_shards.sort_unstable();
        let info = NodeInfo::new(&self.identity.node_id, &self.identity.addr, owned_shards);
        self.registry.register(info)
    }

    /// Signal that this node is leaving the cluster.
    ///
    /// Removes this node from the node registry.
    pub fn leave(&self) -> Result<()> {
        self.registry.remove_node(&self.identity.node_id);
        Ok(())
    }

    /// Get this node's identity.
    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    /// Get the node registry.
    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// Get the shard manager.
    pub fn shards(&self) -> &RwLock<ShardManager> {
        &self.shards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(node_id: &str) -> ClusterState {
        let identity = NodeIdentity::new(node_id, "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new(node_id);
        ClusterState::new(identity, registry, shard_manager)
    }

    // ── Test 7: ClusterState join registers local node ────────────────────

    #[test]
    fn join_registers_local_node() {
        let state = make_state("node-a");
        state.join().unwrap();

        let node = state.registry().get_node("node-a");
        assert!(node.is_some(), "node-a should be registered after join");

        let node = node.unwrap();
        assert_eq!(node.node_id, "node-a");
        assert_eq!(node.addr, "127.0.0.1:9001");
        assert_eq!(node.status, crate::discovery::NodeStatus::Active);
    }

    // ── Test 8: ClusterState leave removes from registry ──────────────────

    #[test]
    fn leave_removes_from_registry() {
        let state = make_state("node-a");
        state.join().unwrap();
        assert!(state.registry().get_node("node-a").is_some());

        state.leave().unwrap();
        assert!(state.registry().get_node("node-a").is_none(), "node should be removed after leave");
    }

    // ── Join with shards assigned ─────────────────────────────────────────

    #[test]
    fn join_includes_assigned_shards() {
        let identity = NodeIdentity::new("node-a", "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let mut shard_manager = ShardManager::new("node-a");
        shard_manager.assign_shard(0, "node-a").unwrap();
        shard_manager.assign_shard(1, "node-a").unwrap();

        let state = ClusterState::new(identity, registry, shard_manager);
        state.join().unwrap();

        let node = state.registry().get_node("node-a").unwrap();
        assert_eq!(node.shards, vec![0, 1], "join should include assigned shards");
    }

    // ── Re-join after leave is allowed ────────────────────────────────────

    #[test]
    fn rejoin_after_leave() {
        let state = make_state("node-a");
        state.join().unwrap();
        state.leave().unwrap();

        // Re-join should succeed
        state.join().unwrap();
        assert!(state.registry().get_node("node-a").is_some(), "re-join should succeed");
    }

    // ── Identity accessor ─────────────────────────────────────────────────

    #[test]
    fn identity_accessor() {
        let state = make_state("node-x");
        assert_eq!(state.identity().node_id, "node-x");
        assert_eq!(state.identity().addr, "127.0.0.1:9001");
    }

    // ── Shard accessor ────────────────────────────────────────────────────

    #[test]
    fn shards_accessor() {
        let state = make_state("node-a");
        let sm = state.shards().read().unwrap();
        assert!(sm.is_empty());
    }

    // ── Leader election is created ────────────────────────────────────────

    #[test]
    fn leader_election_exists() {
        let state = make_state("node-a");
        assert_eq!(state.leader_election.node_id(), "node-a");
        assert_eq!(state.leader_election.elected_count(), 0);
    }

    // ── Failover manager is created with default config ───────────────────

    #[test]
    fn failover_manager_exists() {
        let state = make_state("node-a");
        assert_eq!(state.failover.election().node_id(), "node-a");
    }
}
