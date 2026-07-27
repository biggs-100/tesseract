// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Top-level cluster node that wires together cluster state, query
//! coordination, and the HTTP server.

use std::sync::Arc;

use axum::Router;
use tesseract_common::error::Result;
use tesseract_storage::StorageEngine;

use crate::api::{self, ClusterApiState};
use crate::cluster::{ClusterState, NodeIdentity};
use crate::discovery::NodeRegistry;
use crate::query_coordinator::QueryCoordinator;
use crate::shard_manager::ShardManager;

/// Configuration for creating a [`ClusterNode`].
pub struct ClusterNodeConfig {
    /// Local node identity.
    pub identity: NodeIdentity,
    /// HTTP address to listen on (e.g. `"127.0.0.1:9001"`).
    pub http_addr: String,
    /// Shared storage engine for local data.
    pub storage: Arc<StorageEngine>,
    /// Default query timeout in milliseconds.
    pub query_timeout_ms: u64,
}

/// A single node in the Tesseract cluster.
///
/// Wraps [`ClusterState`], [`QueryCoordinator`], and the HTTP server
/// into a single runnable unit. Every node in the cluster runs one
/// instance of `ClusterNode`.
pub struct ClusterNode {
    pub identity: NodeIdentity,
    pub cluster_state: Arc<ClusterState>,
    pub coordinator: Arc<QueryCoordinator>,
    pub storage: Arc<StorageEngine>,
    pub http_addr: String,
}

impl ClusterNode {
    /// Create a new [`ClusterNode`].
    ///
    /// Initializes the [`NodeRegistry`], [`ShardManager`], [`ClusterState`],
    /// and [`QueryCoordinator`]. Does **not** join the cluster or bind the
    /// HTTP listener — call [`start`](Self::start) for that.
    pub fn new(config: ClusterNodeConfig) -> Self {
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new(&config.identity.node_id);
        let cluster_state = Arc::new(ClusterState::new(config.identity.clone(), registry, shard_manager));
        let coordinator =
            Arc::new(QueryCoordinator::new(cluster_state.clone(), config.storage.clone(), config.query_timeout_ms));

        Self {
            identity: config.identity,
            cluster_state,
            coordinator,
            storage: config.storage,
            http_addr: config.http_addr,
        }
    }

    /// Start the cluster node.
    ///
    /// 1. Registers this node in the node registry ([`ClusterState::join`]).
    /// 2. Starts the failover monitoring background task.
    /// 3. Builds the combined HTTP router.
    /// 4. Binds to [`http_addr`](ClusterNodeConfig::http_addr) and starts
    ///    serving requests.
    pub async fn start(&self) -> Result<()> {
        self.cluster_state.join()?;
        let _failover_handle = self.cluster_state.failover.start();

        let router = self.build_router();
        let listener =
            tokio::net::TcpListener::bind(&self.http_addr).await.map_err(tesseract_common::error::Error::IoError)?;

        tracing::info!("Cluster node {} listening on {}", self.identity.node_id, self.http_addr);

        axum::serve(listener, router)
            .await
            .map_err(|e| tesseract_common::error::Error::ServiceError(format!("server error: {e}")))?;

        Ok(())
    }

    /// Build the combined HTTP router.
    ///
    /// Includes both the external cluster management API (`/cluster/*`)
    /// and the internal node-to-node API (`/internal/*`).
    pub fn build_router(&self) -> Router {
        let state =
            Arc::new(ClusterApiState { cluster: self.cluster_state.clone(), storage: Some(self.storage.clone()) });

        Router::new()
            // External management API
            .route("/cluster/nodes", axum::routing::get(api::list_nodes))
            .route("/cluster/shard-assignment", axum::routing::get(api::get_shard_assignment))
            .route("/cluster/health", axum::routing::get(api::cluster_health))
            .route("/cluster/promotion-candidates", axum::routing::get(api::promotion_candidates))
            .route("/cluster/insert", axum::routing::post(api::handle_forward_insert))
            // Internal node-to-node API
            .route("/internal/search", axum::routing::post(api::internal_search_handler))
            .route("/internal/insert", axum::routing::post(api::internal_insert_handler))
            .route("/internal/replicate", axum::routing::post(api::internal_replicate_handler))
            .route("/internal/health", axum::routing::get(api::internal_health_handler))
            .with_state(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ClusterNode: new creates with correct identity ──────────────────

    #[tokio::test]
    async fn cluster_node_new_sets_identity() {
        let dir = tempfile::tempdir().unwrap();
        let storage_config = tesseract_storage::types::StorageConfig {
            wal: tesseract_storage::types::WalConfig { wal_dir: dir.path().join("wal"), ..Default::default() },
            index: tesseract_storage::types::IndexConfig {
                path: dir.path().join("index.hnsw"),
                enabled: false,
                ..Default::default()
            },
            hot: Default::default(),
            cold: Default::default(),
            cache: Default::default(),
            skeleton: Default::default(),
            lifecycle: Default::default(),
            topological: Default::default(),
            merkle: Default::default(),
            shutdown: tesseract_storage::types::ShutdownConfig::default(),
        };
        let storage = Arc::new(tesseract_storage::StorageEngine::open(storage_config).await.unwrap());

        let identity = NodeIdentity::new("test-node", "127.0.0.1:9999");
        let config = ClusterNodeConfig {
            identity: identity.clone(),
            http_addr: "127.0.0.1:9999".to_string(),
            storage,
            query_timeout_ms: 5000,
        };

        let node = ClusterNode::new(config);

        assert_eq!(node.identity.node_id, "test-node");
        assert_eq!(node.identity.addr, "127.0.0.1:9999");
        assert_eq!(node.http_addr, "127.0.0.1:9999");
        assert_eq!(node.cluster_state.identity().node_id, "test-node");
    }
}
