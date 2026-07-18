// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tesseract_common::error::{Error, Result};

/// Node status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Active,
    /// Heartbeat missed, not yet confirmed dead.
    Suspect,
    Dead,
}

/// Cluster node information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub addr: String,
    pub status: NodeStatus,
    /// Epoch seconds timestamp (ISO 8601 compatible numeric representation).
    pub last_heartbeat: String,
    pub shards: Vec<u64>,
}

impl NodeInfo {
    /// Create a new `NodeInfo` with the current time as heartbeat.
    pub fn new(node_id: &str, addr: &str, shards: Vec<u64>) -> Self {
        Self {
            node_id: node_id.to_string(),
            addr: addr.to_string(),
            status: NodeStatus::Active,
            last_heartbeat: now_epoch_millis(),
            shards,
        }
    }
}

/// Returns the current time as a decimal epoch-milliseconds string.
///
/// Millisecond precision is used so that sub-second timing in tests
/// (e.g. heartbeat timeout detection) works reliably.
fn now_epoch_millis() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().to_string()
}

/// Parse an epoch-milliseconds string, returning `None` on failure.
fn parse_epoch_millis(s: &str) -> Option<u128> {
    s.parse::<u128>().ok()
}

/// In-memory node registry for cluster membership.
///
/// Tracks node state including heartbeats, status transitions, and
/// automatic suspect marking on heartbeat timeout. This registry works
/// without an external etcd dependency and is suitable for testing and
/// small-scale deployments.
pub struct NodeRegistry {
    nodes: RwLock<HashMap<String, NodeInfo>>,
    heartbeat_timeout: Duration,
}

impl NodeRegistry {
    /// Create a new `NodeRegistry` with the given heartbeat timeout in seconds.
    pub fn new(heartbeat_timeout_secs: u64) -> Self {
        Self { nodes: RwLock::new(HashMap::new()), heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs) }
    }

    /// Register or update a node.
    ///
    /// Returns `Err(NodeConflict)` if a node with the same ID is already
    /// registered with a different address. If the same node re-registers
    /// (matching node_id AND addr), the registration is updated.
    pub fn register(&self, info: NodeInfo) -> Result<()> {
        let mut nodes = self.nodes.write().map_err(|e| Error::ServiceError(format!("lock poisoned: {e}")))?;

        if let Some(existing) = nodes.get(&info.node_id) {
            if existing.addr != info.addr {
                return Err(Error::NodeConflict(format!(
                    "node {} already registered at {}",
                    info.node_id, existing.addr
                )));
            }
            // Same node re-registering — update fields.
        }

        nodes.insert(info.node_id.clone(), info);
        Ok(())
    }

    /// Record a heartbeat from a node.
    ///
    /// Updates the node's timestamp and resets its status to `Active`.
    /// Returns `Err(NotFound)` if the node is not registered.
    pub fn heartbeat(&self, node_id: &str) -> Result<()> {
        let mut nodes = self.nodes.write().map_err(|e| Error::ServiceError(format!("lock poisoned: {e}")))?;

        let info = nodes.get_mut(node_id).ok_or_else(|| Error::NotFound(format!("node {node_id}")))?;
        info.last_heartbeat = now_epoch_millis();
        info.status = NodeStatus::Active;
        Ok(())
    }

    /// Mark a node as dead.
    ///
    /// Returns `Err(NotFound)` if the node is not registered.
    pub fn mark_dead(&self, node_id: &str) -> Result<()> {
        let mut nodes = self.nodes.write().map_err(|e| Error::ServiceError(format!("lock poisoned: {e}")))?;

        let info = nodes.get_mut(node_id).ok_or_else(|| Error::NotFound(format!("node {node_id}")))?;
        info.status = NodeStatus::Dead;
        Ok(())
    }

    /// List all active nodes.
    pub fn active_nodes(&self) -> Vec<NodeInfo> {
        let nodes = self.nodes.read().map_err(|e| Error::ServiceError(format!("lock poisoned: {e}")));
        match nodes {
            Ok(nodes) => nodes.values().filter(|n| n.status == NodeStatus::Active).cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Get info for a specific node.
    pub fn get_node(&self, node_id: &str) -> Option<NodeInfo> {
        let nodes = self.nodes.read().ok()?;
        nodes.get(node_id).cloned()
    }

    /// Remove a node from the registry.
    pub fn remove_node(&self, node_id: &str) {
        if let Ok(mut nodes) = self.nodes.write() {
            nodes.remove(node_id);
        }
    }

    /// Check which nodes have timed out and mark them suspect.
    ///
    /// A node is considered timed out when its last heartbeat is older
    /// than `heartbeat_timeout`. Nodes already in `Dead` status are
    /// skipped. Returns the list of node IDs that were newly marked as
    /// suspect.
    pub fn check_heartbeats(&self) -> Vec<String> {
        let mut timed_out = Vec::new();

        let Ok(mut nodes) = self.nodes.write() else {
            return timed_out;
        };

        let now_millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();

        for info in nodes.values_mut() {
            if info.status == NodeStatus::Dead {
                continue;
            }

            let last = parse_epoch_millis(&info.last_heartbeat).unwrap_or(0);
            let elapsed_millis = now_millis.saturating_sub(last);

            if Duration::from_millis(elapsed_millis as u64) > self.heartbeat_timeout
                && info.status == NodeStatus::Active
            {
                info.status = NodeStatus::Suspect;
                timed_out.push(info.node_id.clone());
            }
        }

        timed_out
    }

    /// Number of registered nodes.
    pub fn len(&self) -> usize {
        self.nodes.read().map(|n| n.len()).unwrap_or(0)
    }

    /// Return all registered nodes regardless of status.
    pub fn all_nodes(&self) -> Vec<NodeInfo> {
        let nodes = self.nodes.read().ok();
        match nodes {
            Some(nodes) => nodes.values().cloned().collect(),
            None => Vec::new(),
        }
    }

    /// Return `true` if no nodes are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration as StdDuration;

    fn test_node(node_id: &str, addr: &str) -> NodeInfo {
        NodeInfo::new(node_id, addr, vec![])
    }

    // ── Test 1: register node, verify active ──────────────────────────────

    #[test]
    fn register_node_is_active() {
        let registry = NodeRegistry::new(30);
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        let node = registry.get_node("node-a").expect("node should exist");
        assert_eq!(node.status, NodeStatus::Active);
        assert_eq!(node.addr, "10.0.0.1:9001");
    }

    // ── Test 2: duplicate node ID detection ───────────────────────────────

    #[test]
    fn register_duplicate_node_id_returns_conflict() {
        let registry = NodeRegistry::new(30);
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        let err = registry.register(test_node("node-a", "10.0.0.2:9001")).unwrap_err();
        assert!(
            matches!(&err, Error::NodeConflict(msg) if msg.contains("node-a")),
            "expected NodeConflict for duplicate ID, got {err:?}"
        );
    }

    // ── Test 3: re-register same node (same addr) is allowed ──────────────

    #[test]
    fn register_same_node_same_addr_is_update() {
        let registry = NodeRegistry::new(30);
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        let mut updated = test_node("node-a", "10.0.0.1:9001");
        updated.shards = vec![1, 2, 3];
        registry.register(updated).unwrap();

        let node = registry.get_node("node-a").expect("node should exist");
        assert_eq!(node.shards, vec![1, 2, 3]);
    }

    // ── Test 4: heartbeat refreshes timestamp ─────────────────────────────

    #[test]
    fn heartbeat_refreshes_timeout() {
        let registry = NodeRegistry::new(1); // 1-second timeout
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        // Wait past the timeout
        thread::sleep(StdDuration::from_millis(1100));

        // Without heartbeat, node should be suspect
        let timed_out = registry.check_heartbeats();
        assert!(timed_out.contains(&"node-a".to_string()), "node-a should time out without heartbeat");

        // Now heartbeat and verify it's active again
        registry.heartbeat("node-a").unwrap();
        let timed_out = registry.check_heartbeats();
        assert!(!timed_out.contains(&"node-a".to_string()), "node-a should not time out after heartbeat");
    }

    // ── Test 5: timeout detection with short timeout ──────────────────────

    #[test]
    fn timeout_detection_with_short_timeout() {
        let registry = NodeRegistry::new(0); // 0-second timeout = immediate
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        // Short sleep ensures at least some time has passed
        thread::sleep(StdDuration::from_millis(10));

        let timed_out = registry.check_heartbeats();
        assert!(timed_out.contains(&"node-a".to_string()), "node with 0s timeout should be suspect");
    }

    // ── Test 6: mark_dead transitions to Dead ─────────────────────────────

    #[test]
    fn mark_dead_transitions_to_dead() {
        let registry = NodeRegistry::new(30);
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();

        registry.mark_dead("node-a").unwrap();
        let node = registry.get_node("node-a").expect("node should exist");
        assert_eq!(node.status, NodeStatus::Dead);
    }

    // ── Test 7: active_nodes filters dead/suspect ─────────────────────────

    #[test]
    fn active_nodes_filters_dead_and_suspect() {
        let registry = NodeRegistry::new(30);

        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap(); // stays active
        registry.register(test_node("node-b", "10.0.0.2:9001")).unwrap(); // will be marked suspect
        registry.register(test_node("node-c", "10.0.0.3:9001")).unwrap(); // will be marked dead

        // Manually manipulate status via internal methods
        // Mark node-b suspect by setting a very old heartbeat
        {
            let mut nodes = registry.nodes.write().unwrap();
            if let Some(b) = nodes.get_mut("node-b") {
                b.last_heartbeat = "42".to_string(); // epoch + 42ms — ancient
            }
        }
        registry.check_heartbeats(); // should mark node-b suspect

        registry.mark_dead("node-c").unwrap();

        let active = registry.active_nodes();
        let active_ids: Vec<&str> = active.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(active_ids, vec!["node-a"], "only node-a should be active, got: {active_ids:?}");
    }

    // ── Test 8: remove_node cleans up ─────────────────────────────────────

    #[test]
    fn remove_node_cleans_up() {
        let registry = NodeRegistry::new(30);
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();
        assert_eq!(registry.len(), 1);

        registry.remove_node("node-a");
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    // ── Test 9: multiple nodes, verify all active ─────────────────────────

    #[test]
    fn multiple_nodes_all_active() {
        let registry = NodeRegistry::new(30);

        for i in 0..3 {
            registry.register(test_node(&format!("node-{i}"), &format!("10.0.0.{i}:9001"))).unwrap();
        }

        assert_eq!(registry.len(), 3);

        let active = registry.active_nodes();
        assert_eq!(active.len(), 3, "all 3 nodes should be active");

        let ids: std::collections::BTreeSet<&str> = active.iter().map(|n| n.node_id.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> = ["node-0", "node-1", "node-2"].into_iter().collect();
        assert_eq!(ids, expected);
    }

    // ── Edge: heartbeat on unknown node returns NotFound ──────────────────

    #[test]
    fn heartbeat_unknown_node_errors() {
        let registry = NodeRegistry::new(30);
        let err = registry.heartbeat("nonexistent").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    // ── Edge: mark_dead on unknown node returns NotFound ──────────────────

    #[test]
    fn mark_dead_unknown_node_errors() {
        let registry = NodeRegistry::new(30);
        let err = registry.mark_dead("nonexistent").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    // ── Edge: check_heartbeats skips dead nodes ───────────────────────────

    #[test]
    fn check_heartbeats_skips_dead_nodes() {
        let registry = NodeRegistry::new(0); // immediate timeout
        registry.register(test_node("node-a", "10.0.0.1:9001")).unwrap();
        registry.mark_dead("node-a").unwrap();

        // Even with 0s timeout, dead node should not be flagged
        thread::sleep(StdDuration::from_millis(10));
        let timed_out = registry.check_heartbeats();
        assert!(!timed_out.contains(&"node-a".to_string()), "dead node should not appear in timeouts");
    }

    // ── Integration: register + heartbeat + multiple nodes ────────────────

    #[test]
    fn register_heartbeat_multiple_nodes() {
        let registry = NodeRegistry::new(5);

        registry.register(test_node("alpha", "10.0.0.1:9001")).unwrap();
        registry.register(test_node("beta", "10.0.0.2:9001")).unwrap();
        registry.register(test_node("gamma", "10.0.0.3:9001")).unwrap();

        assert_eq!(registry.len(), 3);

        // All should be active initially
        assert_eq!(registry.active_nodes().len(), 3);

        // Heartbeat all
        for id in &["alpha", "beta", "gamma"] {
            registry.heartbeat(id).unwrap();
        }

        // Remove one
        registry.remove_node("gamma");
        assert_eq!(registry.len(), 2);
        assert!(registry.get_node("gamma").is_none());
    }
}
