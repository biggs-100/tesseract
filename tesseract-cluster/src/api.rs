// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Cluster management HTTP API.
//!
//! Provides Axum-based endpoints for cluster observability and control:
//! node listing, shard assignment view, health summary, promotion candidates,
//! and insert forwarding. Also exposes internal node-to-node endpoints for
//! search, insert, and health checks.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tesseract_core::types::VectorId;
use tesseract_storage::{StorageEngine, types::WriteMode};

use crate::cluster::ClusterState;
use crate::discovery::NodeInfo;
use crate::query_coordinator::{RemoteSearchRequest, RemoteSearchResponse, handle_remote_search};
use crate::shard_manager::ShardAssignment;

// ── Shared API state ─────────────────────────────────────────────────────────

/// Shared API state injected into Axum handlers.
pub struct ClusterApiState {
    pub cluster: Arc<ClusterState>,
    /// Optional storage engine — `Some` when running as a data node,
    /// `None` when running as a coordinator-only node.
    pub storage: Option<Arc<StorageEngine>>,
}

// ── Response / Request types ─────────────────────────────────────────────────

/// Health summary for the local node.
#[derive(Debug, Serialize)]
pub struct ClusterHealth {
    pub node_id: String,
    pub status: String,
    pub active_nodes: usize,
    pub total_shards: usize,
    pub leaderships: usize,
    pub followerships: usize,
}

/// Promotion candidate — a follower that could be promoted to leader.
#[derive(Debug, Serialize)]
pub struct PromotionCandidate {
    pub shard_id: u64,
    pub current_leader: String,
    pub candidate_node: String,
    pub eligible: bool,
    pub replication_lag: u64,
}

/// Insert request forwarded from a client to the correct shard leader.
#[derive(Debug, Deserialize)]
pub struct InsertRequest {
    pub id: u64,
    pub vector: Vec<f64>,
    pub metadata: Option<serde_json::Value>,
}

/// Insert response returned by the shard leader.
#[derive(Debug, Serialize)]
pub struct InsertResponse {
    pub success: bool,
    pub id: u64,
    pub error: Option<String>,
}

/// Internal insert request payload (shard leader → local storage).
#[derive(Debug, Serialize, Deserialize)]
pub struct InternalInsertRequest {
    pub id: u64,
    pub vector: Vec<f64>,
    pub metadata: Option<serde_json::Value>,
}

/// Internal insert response.
#[derive(Debug, Serialize, Deserialize)]
pub struct InternalInsertResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// Internal health response.
#[derive(Debug, Serialize)]
pub struct InternalHealthResponse {
    pub node_id: String,
    pub is_leader: bool,
    pub shards: Vec<u64>,
}

// ── Router ───────────────────────────────────────────────────────────────────

/// Build the cluster management router (external `/cluster/*` endpoints).
///
/// Internal node-to-node endpoints (`/internal/*`) are NOT included — they
/// are added by [`ClusterNode::build_router`](crate::cluster_node::ClusterNode::build_router).
pub fn cluster_router(state: ClusterApiState) -> Router {
    Router::new()
        .route("/cluster/nodes", get(list_nodes))
        .route("/cluster/shard-assignment", get(get_shard_assignment))
        .route("/cluster/health", get(cluster_health))
        .route("/cluster/promotion-candidates", get(promotion_candidates))
        .route("/cluster/insert", post(handle_forward_insert))
        .with_state(Arc::new(state))
}

// ── External handlers ───────────────────────────────────────────────────────

/// `GET /cluster/nodes` — list all registered nodes with their status.
pub(crate) async fn list_nodes(State(state): State<Arc<ClusterApiState>>) -> Json<Vec<NodeInfo>> {
    let mut nodes = state.cluster.registry().all_nodes();
    nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Json(nodes)
}

/// `GET /cluster/shard-assignment` — show current shard-to-node mapping.
pub(crate) async fn get_shard_assignment(State(state): State<Arc<ClusterApiState>>) -> Json<Vec<ShardAssignment>> {
    let shards = state.cluster.shards().read().expect("shards lock poisoned");
    let mut assignments: Vec<ShardAssignment> = shards
        .assigned_leaders()
        .into_iter()
        .map(|(shard_id, leader)| ShardAssignment { shard_id, leader, replicas: vec![] })
        .collect();
    assignments.sort_by_key(|a| a.shard_id);
    Json(assignments)
}

/// `GET /cluster/health` — per-node health summary.
pub(crate) async fn cluster_health(State(state): State<Arc<ClusterApiState>>) -> Json<ClusterHealth> {
    let identity = state.cluster.identity();
    let active_nodes = state.cluster.registry().active_nodes().len();
    let total_shards = {
        let shards = state.cluster.shards().read().expect("shards lock poisoned");
        shards.len()
    };
    let leaderships = state.cluster.leader_election.my_leaderships().len();
    let followerships = state.cluster.leader_election.my_followerships().len();

    Json(ClusterHealth {
        node_id: identity.node_id.clone(),
        status: "healthy".to_string(),
        active_nodes,
        total_shards,
        leaderships,
        followerships,
    })
}

/// `GET /cluster/promotion-candidates` — which followers are eligible.
pub(crate) async fn promotion_candidates(State(state): State<Arc<ClusterApiState>>) -> Json<Vec<PromotionCandidate>> {
    let followerships = state.cluster.leader_election.my_followerships();
    let candidates = state.cluster.failover.promotion_candidates();
    let eligibility: std::collections::HashMap<u64, bool> = candidates.into_iter().collect();
    let node_id = state.cluster.identity().node_id.clone();

    let mut result: Vec<PromotionCandidate> = followerships
        .into_iter()
        .map(|(shard_id, current_leader)| PromotionCandidate {
            shard_id,
            current_leader,
            candidate_node: node_id.clone(),
            eligible: eligibility.get(&shard_id).copied().unwrap_or(false),
            replication_lag: 0,
        })
        .collect();
    result.sort_by_key(|c| c.shard_id);
    Json(result)
}

/// `POST /cluster/insert` — forward an insert to the correct shard leader.
pub(crate) async fn handle_forward_insert(
    State(state): State<Arc<ClusterApiState>>,
    Json(req): Json<InsertRequest>,
) -> impl IntoResponse {
    // Compute the shard for this vector ID.
    let shard_id = {
        let shards = state.cluster.shards().read().expect("shards lock poisoned");
        let vid = VectorId(req.id);
        shards.shard_for(&vid)
    };

    // Look up the leader for this shard.
    let leader = {
        let shards = state.cluster.shards().read().expect("shards lock poisoned");
        shards.get_leader(shard_id).map(|s| s.to_string())
    };

    let Some(leader) = leader else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(InsertResponse {
                success: false,
                id: req.id,
                error: Some(format!("shard {shard_id} has no leader assigned")),
            }),
        );
    };

    let local_node_id = state.cluster.identity().node_id.clone();

    if leader == local_node_id {
        // Local insert.
        let storage = match &state.storage {
            Some(s) => s,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(InsertResponse {
                        success: false,
                        id: req.id,
                        error: Some("local storage not available".to_string()),
                    }),
                );
            }
        };

        let metadata = req.metadata.unwrap_or(serde_json::json!({}));
        match storage.insert(VectorId(req.id), req.vector, metadata, WriteMode::Durable).await {
            Ok(_) => (StatusCode::CREATED, Json(InsertResponse { success: true, id: req.id, error: None })),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(InsertResponse { success: false, id: req.id, error: Some(e.to_string()) }),
            ),
        }
    } else {
        // Forward to remote leader via HTTP.
        let addr = match state.cluster.registry().get_node(&leader) {
            Some(n) => n.addr.clone(),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(InsertResponse {
                        success: false,
                        id: req.id,
                        error: Some(format!("leader {leader} for shard {shard_id} not found in registry")),
                    }),
                );
            }
        };

        let url = format!("http://{addr}/internal/insert");
        let client = reqwest::Client::new();
        let internal_req = InternalInsertRequest { id: req.id, vector: req.vector, metadata: req.metadata };

        match client.post(&url).json(&internal_req).send().await {
            Ok(resp) => match resp.json::<InternalInsertResponse>().await {
                Ok(insert_resp) => {
                    let status = if insert_resp.success { StatusCode::CREATED } else { StatusCode::BAD_REQUEST };
                    (
                        status,
                        Json(InsertResponse { success: insert_resp.success, id: req.id, error: insert_resp.error }),
                    )
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(InsertResponse {
                        success: false,
                        id: req.id,
                        error: Some(format!("failed to parse response from leader {leader}: {e}")),
                    }),
                ),
            },
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(InsertResponse {
                    success: false,
                    id: req.id,
                    error: Some(format!("failed to forward insert to leader {leader} at {addr}: {e}")),
                }),
            ),
        }
    }
}

// ── Internal node-to-node handlers ───────────────────────────────────────────

/// `POST /internal/search` — execute a search on the local storage.
pub(crate) async fn internal_search_handler(
    State(state): State<Arc<ClusterApiState>>,
    Json(req): Json<RemoteSearchRequest>,
) -> impl IntoResponse {
    let storage = match &state.storage {
        Some(s) => s,
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(RemoteSearchResponse { results: vec![], took_ms: 0.0 }));
        }
    };

    match handle_remote_search(req, storage.as_ref()).await {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(e) => {
            tracing::warn!("internal search failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(RemoteSearchResponse { results: vec![], took_ms: 0.0 }))
        }
    }
}

/// `POST /internal/insert` — receive a forwarded insert from a coordinator.
pub(crate) async fn internal_insert_handler(
    State(state): State<Arc<ClusterApiState>>,
    Json(req): Json<InternalInsertRequest>,
) -> impl IntoResponse {
    let storage = match &state.storage {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(InternalInsertResponse { success: false, error: Some("local storage not available".to_string()) }),
            );
        }
    };

    let metadata = req.metadata.unwrap_or(serde_json::json!({}));
    match storage.insert(VectorId(req.id), req.vector, metadata, WriteMode::Durable).await {
        Ok(_) => (StatusCode::CREATED, Json(InternalInsertResponse { success: true, error: None })),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(InternalInsertResponse { success: false, error: Some(e.to_string()) }))
        }
    }
}

/// `POST /internal/replicate` — receive replicated WAL entries from the shard leader.
pub(crate) async fn internal_replicate_handler(
    State(state): State<Arc<ClusterApiState>>,
    Json(entries): Json<Vec<crate::replication::ReplicationEntry>>,
) -> impl IntoResponse {
    let storage = match &state.storage {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::replication_client::ReplicationResponse {
                    success: false,
                    last_acked_txn_id: 0,
                    error: Some("local storage not available".to_string()),
                }),
            );
        }
    };

    match crate::replication_handler::handle_replicate(entries, storage.as_ref()).await {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(e) => {
            tracing::warn!("internal replicate failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::replication_client::ReplicationResponse {
                    success: false,
                    last_acked_txn_id: 0,
                    error: Some(e.to_string()),
                }),
            )
        }
    }
}

/// `GET /internal/health` — liveness and leadership info.
pub(crate) async fn internal_health_handler(State(state): State<Arc<ClusterApiState>>) -> Json<InternalHealthResponse> {
    let node_id = state.cluster.identity().node_id.clone();
    let leaderships = state.cluster.leader_election.my_leaderships();
    let is_leader = !leaderships.is_empty();

    Json(InternalHealthResponse { node_id, is_leader, shards: leaderships })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::NodeIdentity;
    use crate::discovery::NodeRegistry;
    use crate::shard_manager::ShardManager;

    /// Helper: create a `ClusterApiState` with storage=`None` for tests
    /// that only test cluster management endpoints.
    fn make_api_state() -> Arc<ClusterApiState> {
        let identity = NodeIdentity::new("test-node", "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new("test-node");
        let cluster = Arc::new(crate::cluster::ClusterState::new(identity, registry, shard_manager));
        Arc::new(ClusterApiState { cluster, storage: None })
    }

    // ── Test 1: list_nodes returns correct info ──────────────────────────

    #[tokio::test]
    async fn list_nodes_returns_all_nodes() {
        let state = make_api_state();

        // Join the local node so the registry has an entry.
        state.cluster.join().unwrap();

        let response = list_nodes(State(state)).await;
        let nodes: Vec<NodeInfo> = response.0;

        assert_eq!(nodes.len(), 1, "should have one registered node");
        assert_eq!(nodes[0].node_id, "test-node");
        assert_eq!(nodes[0].addr, "127.0.0.1:9001");
    }

    #[tokio::test]
    async fn list_nodes_empty_when_no_nodes() {
        let state = make_api_state();
        // Do NOT join — registry should be empty.

        let response = list_nodes(State(state)).await;
        let nodes: Vec<NodeInfo> = response.0;

        assert!(nodes.is_empty(), "should have no nodes before join");
    }

    // ── Test 2: shard_assignment reflects ShardManager state ─────────────

    #[tokio::test]
    async fn shard_assignment_empty_initially() {
        let state = make_api_state();

        let response = get_shard_assignment(State(state)).await;
        let assignments: Vec<ShardAssignment> = response.0;

        assert!(assignments.is_empty(), "no shards assigned initially");
    }

    #[tokio::test]
    async fn shard_assignment_after_assign() {
        let state = make_api_state();

        // Assign a shard via the shard manager.
        {
            let mut shards = state.cluster.shards().write().unwrap();
            shards.assign_shard(3, "node-b").unwrap();
            shards.assign_shard(7, "node-c").unwrap();
        }

        let response = get_shard_assignment(State(state)).await;
        let assignments: Vec<ShardAssignment> = response.0;

        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].shard_id, 3);
        assert_eq!(assignments[0].leader, "node-b");
        assert_eq!(assignments[1].shard_id, 7);
        assert_eq!(assignments[1].leader, "node-c");
        // Replicas are not set in the assigned_leaders() projection.
        assert!(assignments[0].replicas.is_empty());
        assert!(assignments[1].replicas.is_empty());
    }

    // ── Test 3: cluster_health returns summary ───────────────────────────

    #[tokio::test]
    async fn cluster_health_returns_summary() {
        let state = make_api_state();
        state.cluster.join().unwrap();

        let response = cluster_health(State(state)).await;
        let health: ClusterHealth = response.0;

        assert_eq!(health.node_id, "test-node");
        assert_eq!(health.status, "healthy");
        assert_eq!(health.active_nodes, 1);
        assert_eq!(health.total_shards, 0);
        assert_eq!(health.leaderships, 0);
        assert_eq!(health.followerships, 0);
    }

    #[tokio::test]
    async fn cluster_health_counts_leaderships() {
        let identity = NodeIdentity::new("leader-node", "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new("leader-node");
        let cluster = Arc::new(crate::cluster::ClusterState::new(identity, registry, shard_manager));

        // Simulate winning an election for shard 0.
        cluster.leader_election.become_candidate(0);
        cluster.leader_election.win_election(0).unwrap();
        cluster.leader_election.set_leader(1, "other-node");

        let state = Arc::new(ClusterApiState { cluster, storage: None });
        let response = cluster_health(State(state)).await;
        let health: ClusterHealth = response.0;

        assert_eq!(health.leaderships, 1, "should count 1 leadership");
        assert_eq!(health.followerships, 1, "should count 1 followership");
        assert_eq!(health.node_id, "leader-node");
    }

    // ── Test 4: promotion_candidates lists followers ─────────────────────

    #[tokio::test]
    async fn promotion_candidates_empty_when_no_followers() {
        let state = make_api_state();

        let response = promotion_candidates(State(state)).await;
        let candidates: Vec<PromotionCandidate> = response.0;

        assert!(candidates.is_empty(), "no followers → no candidates");
    }

    #[tokio::test]
    async fn promotion_candidates_lists_followers() {
        let identity = NodeIdentity::new("follower-node", "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new("follower-node");
        let cluster = Arc::new(crate::cluster::ClusterState::new(identity, registry, shard_manager));

        // Set up election: this node follows shard 0 with leader "node-a".
        cluster.leader_election.set_leader(0, "node-a");
        cluster.leader_election.set_leader(1, "node-b");

        let state = Arc::new(ClusterApiState { cluster, storage: None });
        let response = promotion_candidates(State(state)).await;
        let candidates: Vec<PromotionCandidate> = response.0;

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].shard_id, 0);
        assert_eq!(candidates[0].current_leader, "node-a");
        assert_eq!(candidates[0].candidate_node, "follower-node");
        assert_eq!(candidates[1].shard_id, 1);
        assert_eq!(candidates[1].current_leader, "node-b");
        assert_eq!(candidates[1].candidate_node, "follower-node");
    }

    // ── Test 5: internal_health returns node info ────────────────────────

    #[tokio::test]
    async fn internal_health_returns_info() {
        let identity = NodeIdentity::new("health-node", "127.0.0.1:9001");
        let registry = NodeRegistry::new(30);
        let shard_manager = ShardManager::new("health-node");
        let cluster = Arc::new(crate::cluster::ClusterState::new(identity, registry, shard_manager));

        // Win an election to become leader.
        cluster.leader_election.become_candidate(0);
        cluster.leader_election.win_election(0).unwrap();

        let state = Arc::new(ClusterApiState { cluster, storage: None });
        let response = internal_health_handler(State(state)).await;
        let health: InternalHealthResponse = response.0;

        assert_eq!(health.node_id, "health-node");
        assert!(health.is_leader);
        assert_eq!(health.shards, vec![0]);
    }
}
