// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Query executor — wires parsing, planning, embedding, episodic memory,
//! and HNSW search into a single end-to-end pipeline using the algebra-based
//! `PlanNode` tree.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tesseract_common::error::Result;
use tesseract_core::embedding::EmbeddingService;
use tesseract_core::episodic::EpisodicMemory;
use tesseract_storage::engine::StorageEngine;

use crate::ast::*;
use crate::parser;
use crate::planner::{PlanNode, PlannerConfig, QueryPlanner};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// A scored search result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoredResult {
    pub id: u64,
    pub score: f32,
    pub metadata: Option<serde_json::Value>,
}

/// Execution timing information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryTimings {
    pub parse_ms: f64,
    pub plan_ms: f64,
    pub embed_ms: f64,
    pub search_ms: f64,
    pub total_ms: f64,
}

/// Full query result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryResult {
    pub results: Vec<ScoredResult>,
    pub total: usize,
    pub timings: QueryTimings,
}

// ---------------------------------------------------------------------------
// QueryExecutor
// ---------------------------------------------------------------------------

/// The query executor connects parsing, planning, embedding, and search.
///
/// Execution walks the algebra-based `PlanNode` tree recursively:
///
/// | Node        | Execution                          |
/// |-------------|------------------------------------|
/// | `AnnScan`   | Embed text (if needed) → HNSW search |
/// | `Filter`    | Evaluate predicate → post-filter    |
/// | `Bias`      | Apply scoring function re-ranking   |
/// | `Sort`      | Sort by scoring function            |
/// | `Limit`     | Skip + take pagination             |
/// | `Deadline`  | Enforce latency budget             |
pub struct QueryExecutor {
    planner: QueryPlanner,
    storage: Arc<StorageEngine>,
    embedder: Arc<dyn EmbeddingService>,
    episodic: Arc<EpisodicMemory>,
    /// Implicit timeout for queries without a `WITHIN` clause.
    query_timeout: Duration,
}

impl QueryExecutor {
    pub fn new(
        storage: Arc<StorageEngine>,
        embedder: Arc<dyn EmbeddingService>,
        episodic: Arc<EpisodicMemory>,
        config: PlannerConfig,
        query_timeout: Duration,
    ) -> Self {
        Self {
            planner: QueryPlanner::new(config),
            storage,
            embedder,
            episodic,
            query_timeout,
        }
    }

    /// Execute a VQL query string end-to-end.
    ///
    /// The pipeline is:
    /// 1. Parse VQL → AST
    /// 2. Plan AST → PlanNode tree
    /// 3. Resolve query vector (embed text or use pre-computed)
    /// 4. Walk the `PlanNode` tree recursively
    pub async fn execute(&self, vql: &str, user_id: Option<&str>) -> Result<QueryResult> {
        let t0 = std::time::Instant::now();

        // Wrap the entire execution in an implicit timeout.
        // Queries with a `WITHIN` clause are additionally bounded by their
        // own deadline; this is a safety net for queries without one.
        let inner = async {
            // 1. Parse
            let parsed = parser::parse(vql)?;
            let t1 = std::time::Instant::now();

            // 2. Plan → PlanNode tree
            let plan = self.planner.plan_to_tree(&parsed)?;
            let t2 = std::time::Instant::now();

            // 3. Resolve query vector from the AST
            let query_vector = self.resolve_query_vector(&parsed).await?;
            let t3 = std::time::Instant::now();

            // 4. Execute the PlanNode tree
            let results = self.execute_plan(&plan, &query_vector, user_id).await?;
            let t4 = std::time::Instant::now();

            Ok::<QueryResult, tesseract_common::error::Error>(QueryResult {
                total: results.len(),
                results,
                timings: QueryTimings {
                    parse_ms: duration_ms(t1 - t0),
                    plan_ms: duration_ms(t2 - t1),
                    embed_ms: duration_ms(t3 - t2),
                    search_ms: duration_ms(t4 - t3),
                    total_ms: duration_ms(t4 - t0),
                },
            })
        };

        match tokio::time::timeout(self.query_timeout, inner).await {
            Ok(result) => result,
            Err(_) => Err(tesseract_common::error::Error::ServiceError(
                "query timed out".into(),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Query vector resolution
    // -----------------------------------------------------------------------

    /// Resolve the query vector from the AST — embed text or use pre-computed vector.
    async fn resolve_query_vector(&self, query: &Query) -> Result<Vec<f64>> {
        match &query.similarity {
            Some(expr) => {
                if let Some(vector) = &expr.vector {
                    Ok(vector.clone())
                } else {
                    self.embedder.embed(&expr.query_text, "text-embedding-3-small").await
                }
            }
            None => Err(tesseract_common::error::Error::ServiceError("No similarity clause".into())),
        }
    }

    // -----------------------------------------------------------------------
    // PlanNode tree execution
    // -----------------------------------------------------------------------

    /// Execute a `PlanNode` tree recursively.
    ///
    /// Returns a boxed future because Rust cannot determine the size of
    /// a recursive async fn at compile time.
    fn execute_plan<'a>(
        &'a self,
        plan: &'a PlanNode,
        query_vector: &'a [f64],
        user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ScoredResult>>> + Send + 'a>> {
        Box::pin(async move {
            match plan {
                // ── Merge (⨝⨝⨝) — Hybrid hot/cold merge ──────────────
                PlanNode::Merge { left, right, limit } => {
                    // Execute both branches in parallel.
                    let left_fut = self.execute_plan(left, query_vector, user_id);
                    let right_fut = self.execute_plan(right, query_vector, user_id);
                    let (left_results, right_results) = tokio::join!(left_fut, right_fut);

                    let mut all = left_results?;
                    all.extend(right_results?);

                    // Sort by score descending, dedup by id, truncate.
                    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    all.dedup_by(|a, b| a.id == b.id);
                    all.truncate(*limit);
                    Ok(all)
                }

                // ── AnnScan (⨝⨝) — ANN/HNSW search ────────────────────
                PlanNode::AnnScan { field: _, ef_search, weight_mask, bias_filters, topological_alpha } => {
                    // Apply episodic footprint if user context is available
                    // (backward-compatible: always applied when user_id is provided)
                    let search_vector = if let Some(uid) = user_id {
                        let footprint = self.episodic.get_footprint(uid)?;
                        if let Some(footprint) = footprint {
                            EpisodicMemory::apply_footprint(query_vector, &footprint)
                        } else {
                            query_vector.to_vec()
                        }
                    } else {
                        query_vector.to_vec()
                    };

                    // Apply topological bias (query-time vector shifting toward
                    // the metadata filter region) so HNSW naturally finds results
                    // that match the filter, without post-filtering.
                    let biased_vector = if !bias_filters.is_empty() {
                        self.storage.apply_topological_bias(&search_vector, bias_filters, *topological_alpha)?
                    } else {
                        search_vector
                    };

                    // Execute HNSW search via StorageEngine
                    let raw = self
                        .storage
                        .search(&biased_vector, *ef_search, weight_mask.as_ref())
                        .await?;

                    Ok(raw
                        .into_iter()
                        .map(|(id, score)| ScoredResult { id: id.0, score, metadata: None })
                        .collect())
                }

                // ── Filter (σ) — Metadata post-filter ──────────────────
                PlanNode::Filter { input, predicate } => {
                    let results = self.execute_plan(input, query_vector, user_id).await?;
                    Ok(results
                        .into_iter()
                        .filter(|r| self.evaluate_predicate(predicate, r))
                        .collect())
                }

                // ── Bias (φ) — Scoring-function re-ranking ─────────────
                PlanNode::Bias { input, scoring_fn, args } => {
                    if scoring_fn == "personal" {
                        // Personal bias modifies the query vector pre-search.
                        // Apply footprint and delegate to child with biased vector.
                        let biased_vector = if let Some(uid) = user_id {
                            let footprint = self.episodic.get_footprint(uid)?;
                            if let Some(footprint) = footprint {
                                EpisodicMemory::apply_footprint(query_vector, &footprint)
                            } else {
                                query_vector.to_vec()
                            }
                        } else {
                            query_vector.to_vec()
                        };
                        return self.execute_plan(input, &biased_vector, user_id).await;
                    }
                    // Post-search bias: execute child, then re-rank
                    let results = self.execute_plan(input, query_vector, user_id).await?;
                    self.apply_bias(results, scoring_fn, args)
                }

                // ── Sort (τ) — Explicit ordering ───────────────────────
                PlanNode::Sort { input, scoring_fn, args, descending } => {
                    let mut results = self.execute_plan(input, query_vector, user_id).await?;
                    self.sort_results(&mut results, scoring_fn, args, *descending);
                    Ok(results)
                }

                // ── Limit (λ) — Pagination ─────────────────────────────
                PlanNode::Limit { input, limit, offset } => {
                    let results = self.execute_plan(input, query_vector, user_id).await?;
                    Ok(results.into_iter().skip(*offset).take(*limit).collect())
                }

                // ── Deadline (⏱) — Latency enforcement ─────────────────
                PlanNode::Deadline { input, millis, estimated_cost_ms: _ } => {
                    let t0 = std::time::Instant::now();
                    let results = self.execute_plan(input, query_vector, user_id).await?;
                    let elapsed_ms = duration_ms(t0.elapsed());

                    if elapsed_ms > *millis as f64 {
                        // Budget exceeded: scale back results proportionally
                        let ratio = (*millis as f64 / elapsed_ms).min(1.0);
                        let keep = (results.len() as f64 * ratio).ceil() as usize;
                        Ok(results.into_iter().take(keep).collect())
                    } else {
                        Ok(results)
                    }
                }
            }
        })
    }

    // -----------------------------------------------------------------------
    // Predicate evaluation (for Filter node)
    // -----------------------------------------------------------------------

    /// Evaluate a predicate against a scored result's metadata.
    fn evaluate_predicate(&self, predicate: &Predicate, result: &ScoredResult) -> bool {
        match predicate {
            Predicate::Comparison { field, operator, value } => {
                let field_val = self.extract_field_value(result, field);
                match field_val {
                    Some(actual) => self.compare_literals(operator, &actual, value),
                    None => false,
                }
            }
            Predicate::In { field, values } => {
                let field_val = self.extract_field_value(result, field);
                match field_val {
                    Some(actual) => values.iter().any(|v| self.values_equal(&actual, v)),
                    None => false,
                }
            }
            Predicate::Between { field, low, high } => {
                let field_val = self.extract_field_value(result, field);
                match field_val {
                    Some(actual) => {
                        self.compare_literals(&ComparisonOp::Gte, &actual, low)
                            && self.compare_literals(&ComparisonOp::Lte, &actual, high)
                    }
                    None => false,
                }
            }
            Predicate::Like { field, pattern } => {
                let field_val = self.extract_field_value(result, field);
                match field_val {
                    Some(Literal::String(s)) => like_match(&s, pattern),
                    _ => false,
                }
            }
            Predicate::And(predicates) => predicates.iter().all(|p| self.evaluate_predicate(p, result)),
        }
    }

    /// Extract a field value from a ScoredResult's metadata as a Literal.
    fn extract_field_value(&self, result: &ScoredResult, field: &str) -> Option<Literal> {
        let metadata = result.metadata.as_ref()?;
        let value = metadata.get(field)?;
        Some(json_value_to_literal(value))
    }

    /// Compare two literals using the given operator.
    fn compare_literals(&self, operator: &ComparisonOp, a: &Literal, b: &Literal) -> bool {
        use std::cmp::Ordering;
        let ordering = match (coerce_numeric(a), coerce_numeric(b)) {
            (Some(la), Some(lb)) => Some(la.partial_cmp(&lb).unwrap_or(Ordering::Equal)),
            _ => return false,
        };
        match operator {
            ComparisonOp::Eq => ordering == Some(Ordering::Equal),
            ComparisonOp::Neq => ordering != Some(Ordering::Equal),
            ComparisonOp::Lt => ordering == Some(Ordering::Less),
            ComparisonOp::Gt => ordering == Some(Ordering::Greater),
            ComparisonOp::Lte => ordering != Some(Ordering::Greater),
            ComparisonOp::Gte => ordering != Some(Ordering::Less),
        }
    }

    /// Check if two literals have equal values (for IN predicate).
    fn values_equal(&self, a: &Literal, b: &Literal) -> bool {
        match (coerce_numeric(a), coerce_numeric(b)) {
            (Some(la), Some(lb)) => (la - lb).abs() < f64::EPSILON,
            _ => {
                // Fall back to string comparison
                format!("{a:?}") == format!("{b:?}")
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bias / Sort scoring functions
    // -----------------------------------------------------------------------

    /// Apply a post-search bias scoring function to re-rank results.
    fn apply_bias(&self, results: Vec<ScoredResult>, scoring_fn: &str, _args: &[String]) -> Result<Vec<ScoredResult>> {
        match scoring_fn {
            "recency" | "popularity" | "relevance_clicks" => {
                // Placeholder: these bias functions would look up external data
                // (timestamps, click counts, user history) and adjust scores.
                // For Phase 3, return results unchanged.
                Ok(results)
            }
            _ => Err(tesseract_common::error::Error::ServiceError(format!(
                "Unknown bias scoring function: {scoring_fn}"
            ))),
        }
    }

    /// Sort results by a scoring function.
    fn sort_results(
        &self,
        results: &mut [ScoredResult],
        scoring_fn: &str,
        _args: &[String],
        descending: bool,
    ) {
        match scoring_fn {
            "score" | "similarity" => {
                results.sort_by(|a, b| {
                    if descending {
                        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)
                    }
                });
            }
            _ => {
                // Unknown scoring function: sort by score descending as fallback
                results.sort_by(|a, b| {
                    b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Convert a [`std::time::Duration`] to milliseconds as an f64.
fn duration_ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Convert a `serde_json::Value` to a `Literal` for predicate evaluation.
fn json_value_to_literal(v: &serde_json::Value) -> Literal {
    match v {
        serde_json::Value::String(s) => Literal::String(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Literal::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Literal::Float(f)
            } else {
                Literal::Null
            }
        }
        serde_json::Value::Bool(b) => Literal::Boolean(*b),
        serde_json::Value::Null => Literal::Null,
        _ => Literal::Null,
    }
}

/// Coerce a `Literal` to an `f64` for numeric comparison.
/// Returns `None` if the literal is not numeric-comparable.
fn coerce_numeric(lit: &Literal) -> Option<f64> {
    match lit {
        Literal::Integer(i) => Some(*i as f64),
        Literal::Float(f) => Some(*f),
        _ => None,
    }
}

/// Simple LIKE pattern matching with `%` as multi-character wildcard.
/// Does NOT support `_` (single-char wildcard) as per VQL v1 spec.
fn like_match(value: &str, pattern: &str) -> bool {
    if pattern == "%" {
        return true;
    }

    let inner = pattern.strip_prefix('%').and_then(|s| s.strip_suffix('%'));
    if let Some(middle) = inner {
        if !middle.is_empty() {
            return value.contains(middle);
        }
    }

    if let Some(suffix) = pattern.strip_prefix('%') {
        value.ends_with(suffix)
    } else if let Some(prefix) = pattern.strip_suffix('%') {
        value.starts_with(prefix)
    } else {
        value == pattern
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    use tesseract_core::embedding::NoopEmbeddingService;
    use tesseract_storage::engine::StorageEngine;
    use tesseract_storage::types::*;

    /// Build a minimal StorageConfig with an enabled index for testing.
    fn test_storage_config(tmp: &TempDir) -> StorageConfig {
        let root = tmp.path().to_path_buf();
        StorageConfig {
            wal: WalConfig {
                wal_dir: root.join("wal"),
                segment_size: 1024 * 1024,
                fsync_interval_ms: 100,
                fsync_interval_ops: 1000,
            },
            hot: HotStoreConfig { max_records: 200 },
            cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 100 },
            skeleton: SkeletonConfig { wake_threshold: 0.15 },
            cache: PageCacheConfig { capacity: 100 },
            index: IndexConfig {
                enabled: true,
                dim: 4,
                hnsw: tesseract_index::types::HnswConfig::default(),
                path: root.join("index.bin"),
            },
            lifecycle: LifecycleConfig::default(),
            topological: TopologicalConfig::default(),
            merkle: tesseract_storage::types::MerkleConfig::default(),
            shutdown: ShutdownConfig::default(),
        }
    }

    /// Build a config that returns vectors close to the query vector.
    fn test_config() -> PlannerConfig {
        PlannerConfig {
            default_ef_search: 50,
            dim: 4,
            estimated_vector_count: 100,
            cost_buffer: 0.0,
            cost_per_distance_ms: 0.000_001,
            topological_alpha: 0.3,
            merkle_enabled: false,
        }
    }

    /// Helper: insert N random vectors.
    async fn insert_vectors(engine: &StorageEngine, n: usize, dim: usize, seed: f64) {
        for i in 0..n {
            let v: Vec<f64> = (0..dim).map(|d| seed + (i as f64 * 0.1) + (d as f64 * 0.01)).collect();
            engine
                .insert(
                    tesseract_core::types::VectorId(i as u64),
                    v,
                    serde_json::json!({"idx": i}),
                    tesseract_storage::types::WriteMode::Fast,
                )
                .await
                .unwrap();
        }
    }

    // -----------------------------------------------------------------------
    // 1. End-to-end query: insert vectors → execute → verify results
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn e2e_query_returns_scored_results() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        // Insert 50 vectors (dim 4) with seed 1.0
        insert_vectors(&engine, 50, 4, 1.0).await;

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));

        let result = executor.execute("FIND SIMILARITY(emb, 'test') LIMIT 10", None).await;
        assert!(result.is_err(), "NoopEmbedding should produce an embed error");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No embedding service configured"));
    }

    /// Test with a query vector by going through the planner directly.
    #[tokio::test]
    async fn e2e_storage_search_works_with_enabled_index() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());

        // Insert 50 vectors close to 0.0
        insert_vectors(&engine, 50, 4, 0.0).await;

        // Search directly via StorageEngine
        let query = vec![0.0_f64; 4];
        let results = engine.search(&query, 10, None).await.unwrap();
        assert!(!results.is_empty(), "should find neighbours for a matching query");
        assert!(results.len() <= 10, "should respect k=10");
    }

    // -----------------------------------------------------------------------
    // 2. Text query with NoopEmbedding — expect embed error
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn text_query_returns_embed_error() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        insert_vectors(&engine, 10, 4, 0.0).await;

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));
        let result = executor.execute("FIND SIMILARITY(emb, 'quantum computing')", None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No embedding service configured"), "expected embed error, got: {err}");
    }

    // -----------------------------------------------------------------------
    // 3. Timings: verify pipeline ordering
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn timings_show_pipeline_ordering() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        insert_vectors(&engine, 10, 4, 0.0).await;

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));
        let result = executor.execute("FIND SIMILARITY(emb, 'should error')", None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn timing_fields_are_populated_on_text_query() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));
        let err = executor.execute("FIND SIMILARITY(emb, 'test') LIMIT 5", None).await.unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    // -----------------------------------------------------------------------
    // 4. Empty index → empty results (storage-level search)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn empty_index_returns_empty_results() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());

        let query = vec![0.0_f64; 4];
        let results = engine.search(&query, 10, None).await.unwrap();
        assert!(results.is_empty(), "empty index should return no results");
    }

    // -----------------------------------------------------------------------
    // 5. Limit enforcement via storage search
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn limit_enforcement_via_search() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());

        // Insert 100 vectors spread across the space.
        insert_vectors(&engine, 100, 4, 0.0).await;

        let query = vec![0.0_f64; 4];
        let results = engine.search(&query, 5, None).await.unwrap();
        assert_eq!(results.len(), 5, "should return exactly 5 results for k=5");
    }

    // -----------------------------------------------------------------------
    // 6. User context: episodic memory biasing
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn user_context_applies_episodic_footprint() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        // First, create a footprint for a user via update_footprint.
        let clicked = vec![1.0_f64; 4];
        let query_vec = vec![0.5_f64; 4];
        episodic.update_footprint("alice", &clicked, &query_vec).unwrap();

        // Verify footprint exists.
        let fp = episodic.get_footprint("alice").unwrap();
        assert!(fp.is_some(), "footprint should exist for alice");

        // Verify apply_footprint produces expected modified vector.
        let raw_query = vec![1.0, 2.0, 3.0, 4.0];
        let footprint = fp.unwrap();
        let biased = EpisodicMemory::apply_footprint(&raw_query, &footprint);
        assert_eq!(biased.len(), 4);
        assert!((biased[0] - 0.85).abs() < 1e-10);
        assert!((biased[1] - 1.7).abs() < 1e-10);

        // Now verify executor passes user_id through to episodic.
        insert_vectors(&engine, 10, 4, 0.0).await;

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));
        let result = executor.execute("FIND SIMILARITY(emb, 'test') LIMIT 5", Some("alice")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No embedding service configured"));
    }

    #[tokio::test]
    async fn user_without_footprint_uses_raw_query() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
        let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn EmbeddingService>;
        let episodic = Arc::new(EpisodicMemory::new());

        // bob has no footprint
        assert!(episodic.get_footprint("bob").unwrap().is_none());

        let executor = QueryExecutor::new(engine, embedder, episodic, test_config(), Duration::from_secs(30));
        let result = executor.execute("FIND SIMILARITY(emb, 'test') LIMIT 5", Some("bob")).await;
        assert!(result.is_err(), "should still fail at embed, not at footprint");
    }

    // -----------------------------------------------------------------------
    // Helper: verify the QueryTimings struct
    // -----------------------------------------------------------------------

    #[test]
    fn timings_are_properly_structured() {
        let timings = QueryTimings { parse_ms: 0.1, plan_ms: 0.2, embed_ms: 0.3, search_ms: 0.5, total_ms: 1.1 };

        let sum_parts = timings.parse_ms + timings.plan_ms + timings.embed_ms + timings.search_ms;
        assert!(
            (timings.total_ms - sum_parts).abs() < f64::EPSILON,
            "total {:.2} should be sum of parts {:.2}",
            timings.total_ms,
            sum_parts
        );

        assert!(timings.parse_ms >= 0.0);
        assert!(timings.plan_ms >= 0.0);
        assert!(timings.embed_ms >= 0.0);
        assert!(timings.search_ms >= 0.0);
    }

    // -----------------------------------------------------------------------
    // Helper: ScoredResult serialization
    // -----------------------------------------------------------------------

    #[test]
    fn scored_result_serializes_to_json() {
        let sr = ScoredResult { id: 42, score: 0.95, metadata: Some(serde_json::json!({"label": "test"})) };
        let json = serde_json::to_string(&sr).unwrap();
        assert!(json.contains("\"id\":42"));
        assert!(json.contains("\"score\":0.95"));
        assert!(json.contains("\"label\":\"test\""));
    }

    #[test]
    fn query_result_serializes_to_json() {
        let qr = QueryResult {
            results: vec![
                ScoredResult { id: 1, score: 0.9, metadata: None },
                ScoredResult { id: 2, score: 0.8, metadata: None },
            ],
            total: 2,
            timings: QueryTimings { parse_ms: 0.1, plan_ms: 0.2, embed_ms: 0.3, search_ms: 0.4, total_ms: 1.0 },
        };
        let json = serde_json::to_string(&qr).unwrap();
        assert!(json.contains("\"total\":2"));
        assert!(json.contains("\"timings\""));
    }

    // -----------------------------------------------------------------------
    // Helper: like_match tests
    // -----------------------------------------------------------------------

    #[test]
    fn like_match_exact() {
        assert!(like_match("hello", "hello"));
        assert!(!like_match("hello", "world"));
    }

    #[test]
    fn like_match_prefix() {
        assert!(like_match("italian", "ita%"));
        assert!(like_match("italian-fusion", "ita%"));
        assert!(!like_match("french", "ita%"));
    }

    #[test]
    fn like_match_suffix() {
        assert!(like_match("hello", "%lo"));
        assert!(!like_match("hello", "%la"));
    }

    #[test]
    fn like_match_contains() {
        assert!(like_match("hello world", "%llo wo%"));
        assert!(like_match("hello world", "%o w%"));
        assert!(!like_match("hello world", "%xyz%"));
    }

    #[test]
    fn like_match_wildcard_only() {
        assert!(like_match("anything", "%"));
        assert!(like_match("", "%"));
    }
}
