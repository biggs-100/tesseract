// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use tesseract_common::error::Result;
use tesseract_core::projection::WeightMask;

use crate::ast::*;

// ---------------------------------------------------------------------------
// QueryPlan — compiled representation of a VQL query
// ---------------------------------------------------------------------------

/// A compiled query plan ready for execution.
#[derive(Debug, Clone)]
pub struct QueryPlan {
    /// What to search for (pre-computed vector or text to embed).
    pub find: FindClause,
    /// Optional weight mask for metadata filtering.
    pub weight_mask: Option<WeightMask>,
    /// Number of results requested.
    pub limit: usize,
    /// Latency budget in milliseconds (if specified).
    pub within_ms: Option<u64>,
    /// Adjusted ef_search parameter to meet latency budget.
    pub ef_search: usize,
    /// Estimated cost in milliseconds.
    pub estimated_cost_ms: f64,
    /// Scoring function name from ORDER BY.
    pub order_by: Option<String>,
}

/// The kind of similarity search to perform.
#[derive(Debug, Clone)]
pub enum FindClause {
    /// Pre-computed vector search.
    Vector {
        /// The embedding field name to search against.
        field: String,
        /// The raw vector values.
        vector: Vec<f64>,
    },
    /// Text query to be embedded before search.
    Text {
        /// The embedding field name to search against.
        field: String,
        /// The query text to embed.
        text: String,
        /// The embedding model name.
        model: String,
    },
}

// ---------------------------------------------------------------------------
// PlannerConfig
// ---------------------------------------------------------------------------

/// Planner configuration.
#[derive(Debug, Clone)]
pub struct PlannerConfig {
    /// Default ef_search when no WITHIN budget is specified.
    pub default_ef_search: usize,
    /// Dimensions of the embedding space.
    pub dim: usize,
    /// Estimated number of vectors in the index.
    pub estimated_vector_count: usize,
    /// Buffer added to cost estimates (0.0 to 1.0, default 0.2 = 20%).
    pub cost_buffer: f64,
    /// Cost per distance computation in ms.
    pub cost_per_distance_ms: f64,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            default_ef_search: 50,
            dim: 384,
            estimated_vector_count: 10_000,
            cost_buffer: 0.2,
            cost_per_distance_ms: 0.001,
        }
    }
}

// ---------------------------------------------------------------------------
// QueryPlanner
// ---------------------------------------------------------------------------

/// Query planner: converts VQL AST to optimized QueryPlan.
pub struct QueryPlanner {
    config: PlannerConfig,
}

impl QueryPlanner {
    pub fn new(config: PlannerConfig) -> Self {
        Self { config }
    }

    /// Plan a query from its AST.
    pub fn plan(&self, query: &Query) -> Result<QueryPlan> {
        let find = self.plan_find(&query.similarity)?;
        let weight_mask = self.derive_weight_mask(&query.metadata_where);
        let limit = query.limit.as_ref().map(|l| l.count as usize).unwrap_or(10);
        let within_ms = query.within.as_ref().map(|w| w.millis);
        let order_by = query.order_by.as_ref().map(|o| o.scoring_fn.clone());
        let ef_search = self.compute_ef_search(within_ms, limit);

        let estimated_cost_ms = self.estimate_cost(ef_search, &weight_mask);

        // Check WITHIN budget constraint
        if let Some(budget) = within_ms {
            let adjusted = estimated_cost_ms * (1.0 + self.config.cost_buffer);
            if adjusted > budget as f64 {
                return Err(tesseract_common::error::Error::ServiceError(format!(
                    "Query would exceed latency budget: estimated {:.1}ms (with buffer), budget {}ms. \
                     Try reducing ef_search, limit, or removing the WITHIN clause.",
                    adjusted, budget
                )));
            }
        }

        Ok(QueryPlan { find, weight_mask, limit, within_ms, ef_search, estimated_cost_ms, order_by })
    }

    fn plan_find(&self, similarity: &Option<SimilarityExpr>) -> Result<FindClause> {
        match similarity {
            Some(expr) => {
                // If a pre-computed vector is provided, use it directly.
                if let Some(vector) = &expr.vector {
                    Ok(FindClause::Vector { field: expr.field.clone(), vector: vector.clone() })
                } else {
                    // Otherwise, treat the query_text as text to be embedded.
                    Ok(FindClause::Text {
                        field: expr.field.clone(),
                        text: expr.query_text.clone(),
                        model: "text-embedding-3-small".to_string(),
                    })
                }
            }
            None => Err(tesseract_common::error::Error::ServiceError("FIND SIMILARITY clause is required".into())),
        }
    }

    /// Derive a WeightMask from metadata WHERE predicates.
    fn derive_weight_mask(&self, metadata_where: &Option<MetadataWhere>) -> Option<WeightMask> {
        metadata_where.as_ref().and_then(|mw| {
            if mw.predicates.is_empty() {
                return None;
            }
            // For Phase 3, we use a simple heuristic:
            // - Equality predicates on categorical fields get weight 1.0 (full filter)
            // - Range predicates get weight 0.5 (partial filter)
            // This is a simplified approach — the full topological projection
            // (learned masks) is a more advanced feature.
            let mut weights = Vec::new();
            for pred in &mw.predicates {
                match pred {
                    Predicate::Comparison { field, operator, .. } => {
                        // Map each predicate to a dimension weight.
                        // Simple hash-based field → dimension mapping.
                        let dim = self.field_to_dim(field);
                        let weight = match operator {
                            ComparisonOp::Eq => 1.0,
                            ComparisonOp::Neq => 0.8,
                            _ => 0.5, // range operators: partial filter
                        };
                        weights.push((dim, weight as f32));
                    }
                    _ => {
                        // IN, BETWEEN: treat as partial filter (skip for Phase 3).
                        continue;
                    }
                }
            }
            if weights.is_empty() { None } else { Some(WeightMask(weights)) }
        })
    }

    /// Map a metadata field name to a dimension index.
    /// This is a simplified approach — a real implementation would
    /// use a learned mapping or a hash function.
    fn field_to_dim(&self, field: &str) -> usize {
        let hash: u64 = field.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        (hash as usize) % self.config.dim
    }

    /// Compute ef_search based on WITHIN budget and other constraints.
    fn compute_ef_search(&self, within_ms: Option<u64>, _limit: usize) -> usize {
        match within_ms {
            Some(budget) => {
                // Scale ef_search down for tighter budgets.
                let base = self.config.default_ef_search as f64;
                let scaled = (base * (budget as f64 / 100.0)).clamp(10.0, 200.0);
                scaled as usize
            }
            None => self.config.default_ef_search,
        }
    }

    /// Estimate the cost of executing a query plan in milliseconds.
    fn estimate_cost(&self, ef_search: usize, _mask: &Option<WeightMask>) -> f64 {
        let n = self.config.estimated_vector_count as f64;
        let log_n = n.ln();
        let dim = self.config.dim as f64;
        let ef = ef_search as f64;

        // Simplified cost model:
        // cost = ef_search * dim * 2 * log(n) * cost_per_distance_ms
        ef * dim * 2.0 * log_n * self.config.cost_per_distance_ms * (1.0 + self.config.cost_buffer)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Query with just a SIMILARITY clause (for minimal-query tests).
    fn query_similarity(field: &str, text: &str) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            order_by: None,
            limit: None,
            within: None,
        }
    }

    /// Build a Query with a SIMILARITY + LIMIT clause.
    fn query_with_limit(field: &str, text: &str, count: u64) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            order_by: None,
            limit: Some(Limit { count }),
            within: None,
        }
    }

    /// Build a Query with SIMILARITY + WITHIN.
    fn query_with_within(field: &str, text: &str, millis: u64) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            order_by: None,
            limit: None,
            within: Some(Within { millis }),
        }
    }

    /// Build a Query with SIMILARITY + WHERE.
    fn query_with_where(field: &str, text: &str, pred: Predicate) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: Some(MetadataWhere { predicates: vec![pred] }),
            order_by: None,
            limit: None,
            within: None,
        }
    }

    /// Build a Query with no similarity clause (invalid — for error tests).
    fn query_no_similarity() -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: None,
            metadata_where: None,
            order_by: None,
            limit: None,
            within: None,
        }
    }

    // -----------------------------------------------------------------------
    // 1. Plan a minimal query (FIND SIMILARITY only)
    // -----------------------------------------------------------------------

    #[test]
    fn plan_minimal_query() {
        let config = PlannerConfig::default();
        let planner = QueryPlanner::new(config);
        let query = query_similarity("emb", "quantum computing");

        let plan = planner.plan(&query).unwrap();

        // Defaults
        assert_eq!(plan.ef_search, 50);
        assert_eq!(plan.limit, 10);
        assert!(plan.weight_mask.is_none());
        assert!(plan.within_ms.is_none());
        assert!(plan.order_by.is_none());
        assert!(plan.estimated_cost_ms > 0.0);

        // FindClause
        match &plan.find {
            FindClause::Text { field, text, model } => {
                assert_eq!(field, "emb");
                assert_eq!(text, "quantum computing");
                assert_eq!(model, "text-embedding-3-small");
            }
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // 2. Plan query with LIMIT
    // -----------------------------------------------------------------------

    #[test]
    fn plan_with_limit() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_with_limit("emb", "hello", 25);

        let plan = planner.plan(&query).unwrap();
        assert_eq!(plan.limit, 25);
    }

    // -----------------------------------------------------------------------
    // 3. Plan query WITHIN budget — verify ef_search is scaled
    // -----------------------------------------------------------------------

    #[test]
    fn plan_within_budget_scales_ef() {
        // Use small vector count + fast cost so the plan fits easily within 200ms.
        let config =
            PlannerConfig { estimated_vector_count: 100, cost_per_distance_ms: 0.000_01, ..Default::default() };
        let planner = QueryPlanner::new(config);
        let query = query_with_within("emb", "test", 200);

        let plan = planner.plan(&query).unwrap();

        // WITHIN 200ms: ef_search = 50 * (200/100) = 100
        assert_eq!(plan.ef_search, 100);
        assert_eq!(plan.within_ms, Some(200));
        assert!(plan.estimated_cost_ms > 0.0);
    }

    // -----------------------------------------------------------------------
    // 4. Plan query WITHIN budget too tight → Err
    // -----------------------------------------------------------------------

    #[test]
    fn plan_within_budget_too_tight_returns_err() {
        // Default config with 10k vectors and higher cost: minimum ef=10 still
        // exceeds a 1ms budget.
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_with_within("emb", "test", 1);

        let err = planner.plan(&query).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("latency budget"), "got: {msg}");
        assert!(msg.contains("1ms"), "got: {msg}");
    }

    // -----------------------------------------------------------------------
    // 5. Plan query with WHERE — verify WeightMask is derived
    // -----------------------------------------------------------------------

    #[test]
    fn plan_with_where_produces_weight_mask() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let pred = Predicate::Comparison {
            field: "category".into(),
            operator: ComparisonOp::Eq,
            value: Literal::String("science".into()),
        };
        let query = query_with_where("emb", "test", pred);

        let plan = planner.plan(&query).unwrap();
        assert!(plan.weight_mask.is_some(), "expected a WeightMask");

        let mask = plan.weight_mask.unwrap();
        assert_eq!(mask.0.len(), 1);

        let (dim, weight) = mask.0[0];
        assert!(dim < 384, "dim index {dim} out of range");
        assert!((weight - 1.0_f32).abs() < f32::EPSILON, "expected weight 1.0, got {weight}");
    }

    // -----------------------------------------------------------------------
    // 6. Plan query with empty similarity → Err
    // -----------------------------------------------------------------------

    #[test]
    fn plan_no_similarity_returns_err() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_no_similarity();

        let err = planner.plan(&query).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("FIND SIMILARITY"), "got: {msg}");
    }

    // -----------------------------------------------------------------------
    // 7. Cost estimation with different ef values
    // -----------------------------------------------------------------------

    #[test]
    fn cost_increases_with_ef_search() {
        let config =
            PlannerConfig { estimated_vector_count: 1000, cost_per_distance_ms: 0.000_1, ..Default::default() };
        let planner = QueryPlanner::new(config);

        let cost_low = planner.estimate_cost(10, &None);
        let cost_high = planner.estimate_cost(100, &None);

        assert!(cost_low > 0.0, "cost should be positive, got {cost_low}");
        assert!(cost_high > cost_low, "ef=100 ({cost_high}) should cost more than ef=10 ({cost_low})");
    }

    // -----------------------------------------------------------------------
    // 8. WeightMask derivation: equality → weight 1.0
    // -----------------------------------------------------------------------

    #[test]
    fn derive_weight_mask_equality() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let mw = Some(MetadataWhere {
            predicates: vec![Predicate::Comparison {
                field: "status".into(),
                operator: ComparisonOp::Eq,
                value: Literal::String("active".into()),
            }],
        });

        let mask = planner.derive_weight_mask(&mw);
        assert!(mask.is_some(), "expected a WeightMask for equality");

        let (_, weight) = mask.unwrap().0[0];
        assert!((weight - 1.0_f32).abs() < f32::EPSILON, "equality should map to weight 1.0, got {weight}");
    }

    // -----------------------------------------------------------------------
    // 9. WeightMask derivation: range operator → weight 0.5
    // -----------------------------------------------------------------------

    #[test]
    fn derive_weight_mask_range_operator() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let mw = Some(MetadataWhere {
            predicates: vec![Predicate::Comparison {
                field: "price".into(),
                operator: ComparisonOp::Gt,
                value: Literal::Float(100.0),
            }],
        });

        let mask = planner.derive_weight_mask(&mw);
        assert!(mask.is_some(), "expected a WeightMask for range");

        let (_, weight) = mask.unwrap().0[0];
        assert!((weight - 0.5_f32).abs() < f32::EPSILON, "range should map to weight 0.5, got {weight}");
    }

    // -----------------------------------------------------------------------
    // Extra: WeightMask derivation from empty WHERE → None
    // -----------------------------------------------------------------------

    #[test]
    fn derive_weight_mask_empty_where() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let mask = planner.derive_weight_mask(&None);
        assert!(mask.is_none(), "no WHERE should produce no mask");
    }

    // -----------------------------------------------------------------------
    // Extra: WeightMask with Neq → weight 0.8
    // -----------------------------------------------------------------------

    #[test]
    fn derive_weight_mask_neq() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let mw = Some(MetadataWhere {
            predicates: vec![Predicate::Comparison {
                field: "color".into(),
                operator: ComparisonOp::Neq,
                value: Literal::String("blue".into()),
            }],
        });

        let mask = planner.derive_weight_mask(&mw);
        assert!(mask.is_some());

        let (_, weight) = mask.unwrap().0[0];
        assert!((weight - 0.8_f32).abs() < f32::EPSILON, "neq should map to weight 0.8, got {weight}");
    }

    // -----------------------------------------------------------------------
    // Extra: PlannerConfig default values
    // -----------------------------------------------------------------------

    #[test]
    fn planner_config_defaults() {
        let cfg = PlannerConfig::default();
        assert_eq!(cfg.default_ef_search, 50);
        assert_eq!(cfg.dim, 384);
        assert_eq!(cfg.estimated_vector_count, 10_000);
        assert!((cfg.cost_buffer - 0.2).abs() < f64::EPSILON);
        assert!((cfg.cost_per_distance_ms - 0.001).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Extra: field_to_dim returns stable hashes in range
    // -----------------------------------------------------------------------

    #[test]
    fn field_to_dim_in_range() {
        let config = PlannerConfig { dim: 128, ..Default::default() };
        let planner = QueryPlanner::new(config);

        let dim1 = planner.field_to_dim("category");
        let dim2 = planner.field_to_dim("year");
        let dim3 = planner.field_to_dim("price");

        assert!(dim1 < 128, "category => {dim1}");
        assert!(dim2 < 128, "year => {dim2}");
        assert!(dim3 < 128, "price => {dim3}");
    }

    // -----------------------------------------------------------------------
    // Extra: compute_ef_search behavior
    // -----------------------------------------------------------------------

    #[test]
    fn compute_ef_search_default_when_no_within() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        assert_eq!(planner.compute_ef_search(None, 10), 50);
    }

    #[test]
    fn compute_ef_search_clamps_minimum() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        // WITHIN 1ms: 50 * (1/100) = 0.5, clamped to 10
        assert_eq!(planner.compute_ef_search(Some(1), 10), 10);
    }

    #[test]
    fn compute_ef_search_clamps_maximum() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        // WITHIN 500ms: 50 * (500/100) = 250, clamped to 200
        assert_eq!(planner.compute_ef_search(Some(500), 10), 200);
    }

    // -----------------------------------------------------------------------
    // Extra: With ORDER BY, plan preserves scoring_fn
    // -----------------------------------------------------------------------

    #[test]
    fn plan_with_order_by() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            order_by: Some(OrderBy {
                scoring_fn: "relevance_clicks".into(),
                args: vec!["current_user".into()],
                descending: true,
            }),
            limit: None,
            within: None,
        };

        let plan = planner.plan(&query).unwrap();
        assert_eq!(plan.order_by, Some("relevance_clicks".to_string()));
    }
}
