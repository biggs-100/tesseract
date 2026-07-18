// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Distributed query coordinator with scatter-gather execution.
//!
//! The [`QueryCoordinator`] fans out a search query to all shard leaders
//! (local or remote), collects results within a per-shard timeout budget,
//! and merges them into a single sorted top-K list with partial-failure
//! reporting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tesseract_common::error::{Error, Result};
use tesseract_core::projection::WeightMask;
use tesseract_storage::StorageEngine;

use crate::cluster::ClusterState;

/// A scored result from a shard search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredShardResult {
    pub id: u64,
    pub score: f32,
}

/// A result from a single shard during scatter-gather.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardResult {
    pub shard_id: u64,
    pub node_id: String,
    pub results: Vec<ScoredShardResult>,
    pub took_ms: f64,
}

/// Merged result across all shards.
#[derive(Debug, Clone, Serialize)]
pub struct DistributedQueryResult {
    pub results: Vec<ScoredShardResult>,
    pub total_shards: usize,
    pub responded_shards: usize,
    /// `true` if some shards timed out or failed.
    pub partial: bool,
    pub took_ms: f64,
}

/// Request payload for `/internal/search` (coordinator → shard).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSearchRequest {
    pub query: Vec<f64>,
    pub ef: usize,
    pub mask: Option<WeightMask>,
}

/// Response payload for `/internal/search` (shard → coordinator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSearchResponse {
    pub results: Vec<ScoredShardResult>,
    pub took_ms: f64,
}

/// Coordinator that performs scatter-gather queries across cluster shards.
///
/// Every node in the cluster runs a `QueryCoordinator`. When a SEARCH query
/// arrives, the coordinator:
///
/// 1. Reads the full shard-to-leader assignment from [`ShardManager`].
/// 2. Fans out the query to all shard leaders concurrently — sends HTTP
///    `POST /internal/search` to remote leaders, calls the local
///    [`StorageEngine::search`] directly for local shards.
/// 3. Applies a per-shard timeout from the prorated `WITHIN` budget, or
///    the default `query_timeout_ms`.
/// 4. Merges the returned results by sorting descending on score,
///    deduplicating by vector ID, and capping at the requested limit.
/// 5. Returns a [`DistributedQueryResult`] with a `partial` flag if any
///    shard failed or timed out. Returns `AllShardsFailed` if every shard
///    failed to respond.
pub struct QueryCoordinator {
    cluster: Arc<ClusterState>,
    query_timeout_ms: u64,
    local_storage: Arc<StorageEngine>,
}

impl QueryCoordinator {
    /// Create a new `QueryCoordinator`.
    pub fn new(cluster: Arc<ClusterState>, storage: Arc<StorageEngine>, query_timeout_ms: u64) -> Self {
        Self { cluster, local_storage: storage, query_timeout_ms }
    }

    /// Execute a distributed search across all shards.
    ///
    /// - `ef`: search breadth (passed to HNSW index on each shard).
    /// - `limit`: global top-K cap applied after merging.
    /// - `within_ms`: optional latency budget — prorated across shards with
    ///   a 50 ms floor per shard. When `None`, the default
    ///   `query_timeout_ms` is used per shard.
    pub async fn search(
        &self,
        query: &[f64],
        ef: usize,
        mask: Option<&WeightMask>,
        limit: usize,
        within_ms: Option<u64>,
    ) -> Result<DistributedQueryResult> {
        let start = Instant::now();

        // ── 1. Get all shard leaders ───────────────────────────────────
        let leaders = {
            let shards =
                self.cluster.shards().read().map_err(|e| Error::ServiceError(format!("shards lock poisoned: {e}")))?;
            let l = shards.assigned_leaders();
            if l.is_empty() {
                return Err(Error::AllShardsFailed);
            }
            l
        };

        let total_shards = leaders.len();

        // ── 2. Compute per-shard timeout ───────────────────────────────
        let per_shard_timeout = if let Some(within_ms) = within_ms {
            let per = std::cmp::max(50u64, within_ms / total_shards as u64);
            Duration::from_millis(per)
        } else {
            Duration::from_millis(self.query_timeout_ms)
        };

        // ── 3. Fan out to all shards concurrently ──────────────────────
        let query_owned = query.to_vec();
        let mask_owned = mask.cloned();
        let base_timeout = self.query_timeout_ms;

        let mut handles = Vec::with_capacity(total_shards);
        for (shard_id, _) in &leaders {
            let cluster = Arc::clone(&self.cluster);
            let storage = Arc::clone(&self.local_storage);
            let q = query_owned.clone();
            let m = mask_owned.clone();
            let sid = *shard_id;
            let tout = per_shard_timeout;
            let btout = base_timeout;

            handles.push(tokio::spawn(async move {
                let coord = QueryCoordinator { cluster, local_storage: storage, query_timeout_ms: btout };
                tokio::time::timeout(tout, coord.search_shard(q, ef, m, sid)).await
            }));
        }

        // ── 4. Collect responses (or timeouts) ─────────────────────────
        let mut shard_results: Vec<ShardResult> = Vec::with_capacity(total_shards);
        let mut responded = 0usize;

        for handle in handles {
            match handle.await {
                Ok(Ok(Ok(result))) => {
                    shard_results.push(result);
                    responded += 1;
                }
                Ok(Ok(Err(e))) => {
                    // Shard returned an error — skip.
                    tracing::debug!("shard search returned error: {e}");
                }
                Ok(Err(_elapsed)) => {
                    // Timeout — shard did not respond in time.
                }
                Err(join_err) => {
                    // Task panicked — log and treat as failure.
                    tracing::warn!("shard search task panicked: {join_err}");
                }
            }
        }

        // ── 5. Handle all-failed case ──────────────────────────────────
        if shard_results.is_empty() {
            return Err(Error::AllShardsFailed);
        }

        // ── 6. Merge and return ────────────────────────────────────────
        let partial = responded < total_shards;
        let merged = Self::merge_results(shard_results, limit);
        let took_ms = start.elapsed().as_secs_f64() * 1000.0;

        Ok(DistributedQueryResult { results: merged, total_shards, responded_shards: responded, partial, took_ms })
    }

    /// Search a single shard (local or remote).
    async fn search_shard(
        &self,
        query: Vec<f64>,
        ef: usize,
        mask: Option<WeightMask>,
        shard_id: u64,
    ) -> Result<ShardResult> {
        let start = Instant::now();

        // Look up the leader for this shard.
        let (leader, is_local) = {
            let shards =
                self.cluster.shards().read().map_err(|e| Error::ServiceError(format!("shards lock poisoned: {e}")))?;
            let leader = shards.get_leader(shard_id).ok_or(Error::ShardNotAssigned(shard_id))?.to_string();
            let is_local = leader == self.cluster.identity().node_id;
            (leader, is_local)
        };

        if is_local {
            self.local_search(&query, ef, mask.as_ref(), shard_id, start).await
        } else {
            self.remote_search(&leader, &query, ef, mask.as_ref(), shard_id).await
        }
    }

    /// Execute a search on the local [`StorageEngine`].
    async fn local_search(
        &self,
        query: &[f64],
        ef: usize,
        mask: Option<&WeightMask>,
        shard_id: u64,
        start: Instant,
    ) -> Result<ShardResult> {
        let results = match mask {
            Some(m) => self.local_storage.search(query, ef, Some(m)).await,
            None => self.local_storage.search(query, ef, None).await,
        }?;

        let took_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok(ShardResult {
            shard_id,
            node_id: self.cluster.identity().node_id.clone(),
            results: results.into_iter().map(|(id, score)| ScoredShardResult { id: id.0, score }).collect(),
            took_ms,
        })
    }

    /// Send a remote search request to another node via HTTP.
    async fn remote_search(
        &self,
        leader: &str,
        query: &[f64],
        ef: usize,
        mask: Option<&WeightMask>,
        shard_id: u64,
    ) -> Result<ShardResult> {
        let start = Instant::now();

        // Resolve leader address from the registry.
        let addr = self
            .cluster
            .registry()
            .get_node(leader)
            .ok_or_else(|| Error::NotFound(format!("leader node {leader}")))?
            .addr;

        let url = format!("http://{addr}/internal/search");
        let req = RemoteSearchRequest { query: query.to_vec(), ef, mask: mask.cloned() };

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&req)
            .timeout(Duration::from_millis(self.query_timeout_ms))
            .send()
            .await
            .map_err(|e| Error::ServiceError(format!("remote search request to {addr} failed: {e}")))?;

        let search_resp: RemoteSearchResponse = resp
            .json()
            .await
            .map_err(|e| Error::ServiceError(format!("failed to parse remote search response from {addr}: {e}")))?;

        let took_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok(ShardResult { shard_id, node_id: leader.to_string(), results: search_resp.results, took_ms })
    }

    /// Merge shard results into a single sorted list, capped at `limit`.
    ///
    /// The merge sorts all scored results descending by score, deduplicates
    /// by ID (keeping the highest score for duplicates), and truncates to
    /// the requested limit.
    fn merge_results(results: Vec<ShardResult>, limit: usize) -> Vec<ScoredShardResult> {
        // Flatten all shard results into a single vector.
        let mut all: Vec<ScoredShardResult> = results.into_iter().flat_map(|r| r.results).collect();

        // Sort descending by score.
        all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Deduplicate by ID — keep the first (highest-scoring) occurrence.
        let mut seen = std::collections::HashSet::new();
        all.retain(|r| seen.insert(r.id));

        // Cap at limit.
        all.truncate(limit);
        all
    }
}

/// Handle a remote search request received from a coordinator.
///
/// This is the handler for `POST /internal/search`. It executes the query
/// against the local [`StorageEngine`] and returns the results.
pub async fn handle_remote_search(body: RemoteSearchRequest, storage: &StorageEngine) -> Result<RemoteSearchResponse> {
    let start = Instant::now();

    let results = storage.search(&body.query, body.ef, body.mask.as_ref()).await?;

    let took_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(RemoteSearchResponse {
        results: results.into_iter().map(|(id, score)| ScoredShardResult { id: id.0, score }).collect(),
        took_ms,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_results: multiple shard results ──────────────────────────

    #[test]
    fn merge_results_with_multiple_shards() {
        let results = vec![
            ShardResult {
                shard_id: 0,
                node_id: "node-a".into(),
                results: vec![ScoredShardResult { id: 1, score: 0.9 }, ScoredShardResult { id: 2, score: 0.7 }],
                took_ms: 10.0,
            },
            ShardResult {
                shard_id: 1,
                node_id: "node-b".into(),
                results: vec![ScoredShardResult { id: 3, score: 0.95 }, ScoredShardResult { id: 4, score: 0.6 }],
                took_ms: 15.0,
            },
        ];

        let merged = QueryCoordinator::merge_results(results, 10);
        assert_eq!(merged.len(), 4);
        // Should be sorted descending by score.
        assert_eq!(merged[0].id, 3);
        assert_eq!(merged[1].id, 1);
        assert_eq!(merged[2].id, 2);
        assert_eq!(merged[3].id, 4);
    }

    // ── merge_results: overlapping IDs (dedup) ────────────────────────

    #[test]
    fn merge_results_deduplicates_by_id() {
        let results = vec![
            ShardResult {
                shard_id: 0,
                node_id: "node-a".into(),
                results: vec![ScoredShardResult { id: 1, score: 0.9 }, ScoredShardResult { id: 2, score: 0.7 }],
                took_ms: 10.0,
            },
            ShardResult {
                shard_id: 1,
                node_id: "node-b".into(),
                results: vec![
                    ScoredShardResult { id: 1, score: 0.85 }, // same ID, lower score
                    ScoredShardResult { id: 3, score: 0.95 },
                ],
                took_ms: 15.0,
            },
        ];

        let merged = QueryCoordinator::merge_results(results, 10);
        // ID 1 should appear only once, keeping the higher score (0.9 from shard 0).
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, 3); // 0.95
        assert_eq!(merged[1].id, 1); // 0.9 (kept from shard 0, not 0.85)
        assert_eq!(merged[1].score, 0.9);
        assert_eq!(merged[2].id, 2); // 0.7
    }

    // ── merge_results: caps at limit ──────────────────────────────────

    #[test]
    fn merge_results_caps_at_limit() {
        let results = vec![ShardResult {
            shard_id: 0,
            node_id: "node-a".into(),
            results: (0..10).map(|i| ScoredShardResult { id: i as u64, score: 1.0 - (i as f32 * 0.1) }).collect(),
            took_ms: 10.0,
        }];

        let merged = QueryCoordinator::merge_results(results, 3);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, 0);
        assert_eq!(merged[1].id, 1);
        assert_eq!(merged[2].id, 2);
    }

    // ── merge_results: empty results ──────────────────────────────────

    #[test]
    fn merge_results_empty() {
        let merged = QueryCoordinator::merge_results(vec![], 10);
        assert!(merged.is_empty());
    }

    #[test]
    fn merge_results_empty_shard_results() {
        let results = vec![ShardResult { shard_id: 0, node_id: "node-a".into(), results: vec![], took_ms: 5.0 }];

        let merged = QueryCoordinator::merge_results(results, 10);
        assert!(merged.is_empty());
    }

    // ── merge_results: sorts descending by score ──────────────────────

    #[test]
    fn merge_results_sorts_descending() {
        let results = vec![ShardResult {
            shard_id: 0,
            node_id: "node-a".into(),
            results: vec![
                ScoredShardResult { id: 1, score: 0.3 },
                ScoredShardResult { id: 2, score: 0.9 },
                ScoredShardResult { id: 3, score: 0.5 },
            ],
            took_ms: 10.0,
        }];

        let merged = QueryCoordinator::merge_results(results, 10);
        assert_eq!(merged.len(), 3);
        assert!(merged[0].score >= merged[1].score);
        assert!(merged[1].score >= merged[2].score);
        assert_eq!(merged[0].id, 2); // 0.9
        assert_eq!(merged[1].id, 3); // 0.5
        assert_eq!(merged[2].id, 1); // 0.3
    }

    // ── Serde roundtrip ───────────────────────────────────────────────

    #[test]
    fn remote_search_request_serde_roundtrip() {
        let req = RemoteSearchRequest {
            query: vec![0.1, 0.2, 0.3],
            ef: 100,
            mask: Some(WeightMask(vec![(0, 2.0), (2, 0.5)])),
        };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: RemoteSearchRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.query, vec![0.1, 0.2, 0.3]);
        assert_eq!(decoded.ef, 100);
        assert_eq!(decoded.mask, Some(WeightMask(vec![(0, 2.0), (2, 0.5)])));
    }

    #[test]
    fn remote_search_request_no_mask_serde_roundtrip() {
        let req = RemoteSearchRequest { query: vec![1.0, 2.0], ef: 50, mask: None };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: RemoteSearchRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.query, vec![1.0, 2.0]);
        assert_eq!(decoded.ef, 50);
        assert!(decoded.mask.is_none());
    }

    #[test]
    fn remote_search_response_serde_roundtrip() {
        let resp = RemoteSearchResponse {
            results: vec![ScoredShardResult { id: 1, score: 0.95 }, ScoredShardResult { id: 2, score: 0.8 }],
            took_ms: 12.5,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: RemoteSearchResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.results.len(), 2);
        assert_eq!(decoded.results[0].id, 1);
        assert!((decoded.results[0].score - 0.95).abs() < 1e-6);
        assert!((decoded.took_ms - 12.5).abs() < 1e-6);
    }

    #[test]
    fn shard_result_serde_roundtrip() {
        let result = ShardResult {
            shard_id: 3,
            node_id: "node-z".into(),
            results: vec![ScoredShardResult { id: 42, score: 0.7 }],
            took_ms: 8.0,
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: ShardResult = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.shard_id, 3);
        assert_eq!(decoded.node_id, "node-z");
        assert_eq!(decoded.results.len(), 1);
        assert_eq!(decoded.results[0].id, 42);
    }

    // ── DistributedQueryResult partial flag ───────────────────────────

    #[test]
    fn distributed_query_result_partial_flag() {
        // Simulate: 5 shards total, only 3 responded.
        let result = DistributedQueryResult {
            results: vec![],
            total_shards: 5,
            responded_shards: 3,
            partial: true,
            took_ms: 100.0,
        };

        assert!(result.partial);
        assert_eq!(result.total_shards, 5);
        assert_eq!(result.responded_shards, 3);
    }

    #[test]
    fn distributed_query_result_not_partial_when_all_respond() {
        let result = DistributedQueryResult {
            results: vec![],
            total_shards: 3,
            responded_shards: 3,
            partial: false,
            took_ms: 50.0,
        };

        assert!(!result.partial);
        assert_eq!(result.total_shards, 3);
        assert_eq!(result.responded_shards, 3);
    }

    // ── AllShardsFailed error path ────────────────────────────────────

    #[test]
    fn all_shards_failed_error_variant() {
        let err = Error::AllShardsFailed;
        assert_eq!(err.to_string(), "All shards failed during distributed query");
    }

    // ── merge_results: dedup keeps highest score ──────────────────────

    #[test]
    fn merge_results_dedup_keeps_highest_score() {
        // Same ID across shards with different scores.
        let results = vec![
            ShardResult {
                shard_id: 0,
                node_id: "node-a".into(),
                results: vec![ScoredShardResult { id: 1, score: 0.5 }],
                took_ms: 5.0,
            },
            ShardResult {
                shard_id: 1,
                node_id: "node-b".into(),
                results: vec![ScoredShardResult { id: 1, score: 0.9 }],
                took_ms: 5.0,
            },
        ];

        let merged = QueryCoordinator::merge_results(results, 10);
        // Sorted desc — ID 1 with 0.9 should come first.
        assert_eq!(merged.len(), 1);
        assert!((merged[0].score - 0.9).abs() < 1e-6, "expected 0.9, got {}", merged[0].score);
    }

    // ── merge_results: limit of zero ──────────────────────────────────

    #[test]
    fn merge_results_limit_zero() {
        let results = vec![ShardResult {
            shard_id: 0,
            node_id: "node-a".into(),
            results: vec![ScoredShardResult { id: 1, score: 0.9 }],
            took_ms: 5.0,
        }];

        let merged = QueryCoordinator::merge_results(results, 0);
        assert!(merged.is_empty());
    }

    // ── Timeout proration computation ─────────────────────────────────

    #[test]
    fn per_shard_timeout_proration() {
        // 2000ms / 4 shards = 500ms per shard.
        let within_ms = 2000u64;
        let total_shards = 4usize;
        let per = std::cmp::max(50u64, within_ms / total_shards as u64);
        assert_eq!(per, 500);
    }

    #[test]
    fn per_shard_timeout_floor() {
        // 100ms / 10 shards = 10ms → should floor to 50ms.
        let within_ms = 100u64;
        let total_shards = 10usize;
        let per = std::cmp::max(50u64, within_ms / total_shards as u64);
        assert_eq!(per, 50);
    }
}
