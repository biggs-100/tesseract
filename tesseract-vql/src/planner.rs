// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use tesseract_common::error::Result;
use tesseract_core::projection::WeightMask;
use tesseract_core::topological::{BiasFilter, BiasKind, RangeOp};

use crate::ast::*;

// ---------------------------------------------------------------------------
// PlanNode — composable algebra-based query plan
// ---------------------------------------------------------------------------

/// A compiled query plan represented as a tree of algebra operators.
///
/// Each node wraps its child (except `AnnScan` and `Merge` which are leaves),
/// forming a pipeline that the executor walks bottom-up.
///
/// | Operator | Node        | Symbol | What it does                |
/// |----------|-------------|--------|----------------------------|
/// | AnnScan  | `AnnScan`   | ⨝⨝     | ANN/HNSW search            |
/// | Merge    | `Merge`     | ⨝⨝⨝    | Hybrid hot/cold merge      |
/// | Filter   | `Filter`    | σ      | Metadata post-filter       |
/// | Bias     | `Bias`      | φ      | Scoring-function re-rank   |
/// | Sort     | `Sort`      | τ      | Explicit ordering          |
/// | Limit    | `Limit`     | λ      | Pagination (limit+offset)  |
/// | Deadline | `Deadline`  | ⏱      | Latency enforcement        |
#[derive(Debug, Clone)]
pub enum PlanNode {
    /// ANN search against an embedding field (Semantic Join, ⨝⨝).
    AnnScan {
        /// The embedding field name to search against.
        field: String,
        /// HNSW ef_search parameter (recall/speed trade-off).
        ef_search: usize,
        /// Optional weight mask for metadata filtering via projection.
        weight_mask: Option<WeightMask>,
        /// Topological bias filters for query vector biasing.
        /// Populated from WHERE predicates during planning.
        bias_filters: Vec<BiasFilter>,
        /// Bias strength multiplier (0.0 = no bias, 1.0 = full centroid shift).
        /// Default 0.3; reduced when WITHIN budget is tight.
        topological_alpha: f64,
    },
    /// Hybrid search merge: search both hot buffer and cold tree, merge results.
    ///
    /// The left branch typically searches the HNSW index (merged data), while
    /// the right branch searches the Merkle tree (centroid-guided). Both
    /// branches are executed in parallel and their results are merged.
    Merge {
        /// Left search branch (typically HNSW/AnnScan).
        left: Box<PlanNode>,
        /// Right search branch (typically Merkle tree / centroid scan).
        right: Box<PlanNode>,
        /// Maximum results to return after merge.
        limit: usize,
    },
    /// Metadata post-filter (Selection, σ).
    ///
    /// Evaluates the predicate against each result's metadata.
    /// For projected fields this would ideally be a geometric constraint
    /// pushed into the HNSW traversal, but for non-projected fields
    /// (or when the index doesn't support topological projection),
    /// this is an O(n) post-filter.
    Filter {
        input: Box<PlanNode>,
        predicate: Predicate,
    },
    /// Bias / personalization (Bias, φ).
    ///
    /// Applies a scoring function to adjust result ranking.
    /// - `personal` bias modifies the query vector pre-search.
    /// - `recency`, `popularity`, `relevance_clicks` re-rank post-search.
    Bias {
        input: Box<PlanNode>,
        scoring_fn: String,
        args: Vec<String>,
    },
    /// Re-ranking with a scoring function (Sort, τ).
    Sort {
        input: Box<PlanNode>,
        scoring_fn: String,
        args: Vec<String>,
        descending: bool,
    },
    /// Limit with optional offset (Limit, λ).
    Limit {
        input: Box<PlanNode>,
        limit: usize,
        offset: usize,
    },
    /// Deadline / latency enforcement (Deadline, ⏱).
    ///
    /// If execution exceeds the budget, results are proportionally truncated.
    /// The planner also rejects queries where the cost model predicts
    /// the budget would be exceeded.
    Deadline {
        input: Box<PlanNode>,
        millis: u64,
        estimated_cost_ms: f64,
    },
}

// ---------------------------------------------------------------------------
// Display — indented tree format
// ---------------------------------------------------------------------------

use std::fmt;

impl fmt::Display for PlanNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_indented(f, 0)
    }
}

impl PlanNode {
    /// Recursive indented display: print this node, then print child with
    /// deeper indentation. The deepest (innermost) node has no trailing newline.
    fn fmt_indented(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        let indent = "  ".repeat(depth);
        match self {
            PlanNode::AnnScan { field, ef_search, weight_mask, bias_filters, topological_alpha } => {
                write!(f, "{indent}AnnScan {{ {field}, ef: {ef_search} }}")?;
                if let Some(mask) = weight_mask {
                    write!(f, " [mask: {} dims]", mask.0.len())?;
                }
                if !bias_filters.is_empty() {
                    write!(f, " [bias: α={topological_alpha}, {} filter(s)]", bias_filters.len())?;
                }
                Ok(())
            }
            PlanNode::Merge { left, right, limit } => {
                write!(f, "{indent}Merge {{ limit: {limit} }}")?;
                writeln!(f)?;
                left.fmt_indented(f, depth + 1)?;
                writeln!(f)?;
                right.fmt_indented(f, depth + 1)
            }
            PlanNode::Filter { input, predicate } => {
                write!(f, "{indent}Filter {{ {predicate} }}")?;
                writeln!(f)?;
                input.fmt_indented(f, depth + 1)
            }
            PlanNode::Bias { input, scoring_fn, args } => {
                write!(f, "{indent}Bias {{ {scoring_fn}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{a}")?;
                }
                write!(f, ") }}")?;
                writeln!(f)?;
                input.fmt_indented(f, depth + 1)
            }
            PlanNode::Sort { input, scoring_fn, args, descending } => {
                let dir = if *descending { "desc" } else { "asc" };
                write!(f, "{indent}Sort {{ {scoring_fn}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{a}")?;
                }
                write!(f, "), {dir} }}")?;
                writeln!(f)?;
                input.fmt_indented(f, depth + 1)
            }
            PlanNode::Limit { input, limit, offset } => {
                write!(f, "{indent}Limit {{ {limit}, offset: {offset} }}")?;
                writeln!(f)?;
                input.fmt_indented(f, depth + 1)
            }
            PlanNode::Deadline { input, millis, estimated_cost_ms } => {
                write!(f, "{indent}Deadline {{ {millis}ms, est: {estimated_cost_ms:.1}ms }}")?;
                writeln!(f)?;
                input.fmt_indented(f, depth + 1)
            }
        }
    }
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
    /// Topological bias strength (0.0 to 1.0, default 0.3).
    /// Tighter WITHIN budgets reduce this proportionally.
    pub topological_alpha: f64,
    /// Whether the progressive Merkle tree is enabled.
    /// When true, the planner wraps the AnnScan in a MergeScan to search
    /// both the hot buffer and the Merkle tree concurrently.
    pub merkle_enabled: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            default_ef_search: 50,
            dim: 384,
            estimated_vector_count: 10_000,
            cost_buffer: 0.2,
            cost_per_distance_ms: 0.001,
            topological_alpha: 0.3,
            merkle_enabled: false,
        }
    }
}

// ---------------------------------------------------------------------------
// QueryPlanner
// ---------------------------------------------------------------------------

/// Query planner: converts VQL AST to an algebra-based `PlanNode` tree.
pub struct QueryPlanner {
    config: PlannerConfig,
}

impl QueryPlanner {
    pub fn new(config: PlannerConfig) -> Self {
        Self { config }
    }

    /// Plan a query from its AST into a composable `PlanNode` tree.
    ///
    /// The tree is built inside-out: `AnnScan` is the innermost node
    /// (the data source), with filtering, bias, sorting, pagination,
    /// and deadline enforcement layered around it.
    ///
    /// The canonical execution order is:
    ///   AnnScan → Filter → Bias → Sort → Limit → Deadline
    pub fn plan_to_tree(&self, query: &Query) -> Result<PlanNode> {
        // Validate BIAS and ORDER BY mutual exclusion
        if query.bias.is_some() && query.order_by.is_some() {
            return Err(tesseract_common::error::Error::ServiceError(
                "BIAS and ORDER BY are mutually exclusive".into(),
            ));
        }

        let similarity = query
            .similarity
            .as_ref()
            .ok_or_else(|| tesseract_common::error::Error::ServiceError("FIND SIMILARITY clause is required".into()))?;

        let limit = query.limit.as_ref().map(|l| l.count as usize).unwrap_or(10);
        let offset = query.offset.as_ref().map(|o| o.count as usize).unwrap_or(0);
        let within_ms = query.within.as_ref().map(|w| w.millis);
        let weight_mask = self.derive_weight_mask(&query.metadata_where);
        let ef_search = self.compute_ef_search(within_ms, limit);
        let estimated_cost_ms = self.estimate_cost(ef_search, &weight_mask);

        // Extract topological bias filters from WITH METADATA WHERE predicates.
        let bias_filters = Self::extract_bias_filters(&query.metadata_where);

        // Adjust alpha for tight WITHIN budgets: tighter budget = lower alpha.
        let topological_alpha = match within_ms {
            Some(budget) => {
                self.config.topological_alpha * (budget as f64 / 100.0).clamp(0.0, 1.0)
            }
            None => self.config.topological_alpha,
        };

        // --- Build the tree inside-out ---

        // 1. Base: AnnScan (data source, ⨝⨝)
        let ann_scan = PlanNode::AnnScan {
            field: similarity.field.clone(),
            ef_search,
            weight_mask,
            bias_filters: bias_filters.clone(),
            topological_alpha,
        };

        // If Merkle tree is enabled, wrap the AnnScan in a MergeScan so both
        // the HNSW index and the Merkle tree / hot buffer are searched.
        let mut node: PlanNode = if self.config.merkle_enabled {
            PlanNode::Merge {
                left: Box::new(ann_scan.clone()),
                right: Box::new(ann_scan),
                limit,
            }
        } else {
            ann_scan
        };

        // 2. Optional Filter (σ) — from WITH METADATA WHERE
        //
        // Skip the Filter node when topological bias already covers the
        // predicate (centroid shifting handles the filter geometrically).
        // This avoids the post-filter issue where AnnScan drops metadata.
        let topological_predicate: Option<Predicate> = if !bias_filters.is_empty() {
            // Build a predicate from the bias_filters so we can check coverage.
            let covered = bias_filters.iter().map(|bf| {
                match &bf.kind {
                    BiasKind::Category(v) => Predicate::Comparison {
                        field: bf.field.clone(),
                        operator: ComparisonOp::Eq,
                        value: Literal::String(v.clone()),
                    },
                    BiasKind::Numerical { value, op } => {
                        let cmp = match op {
                            RangeOp::Eq => ComparisonOp::Eq,
                            RangeOp::Gt => ComparisonOp::Gt,
                            RangeOp::Gte => ComparisonOp::Gte,
                            RangeOp::Lt => ComparisonOp::Lt,
                            RangeOp::Lte => ComparisonOp::Lte,
                            _ => ComparisonOp::Eq,
                        };
                        Predicate::Comparison {
                            field: bf.field.clone(),
                            operator: cmp,
                            value: Literal::Float(*value),
                        }
                    }
                }
            }).collect::<Vec<_>>();
            if let [single] = &covered[..] {
                Some(single.clone())
            } else {
                Some(Predicate::And(covered))
            }
        } else {
            None
        };

        if let Some(mw) = &query.metadata_where {
            if !mw.predicates.is_empty() {
                // Check if the predicate is already covered by topological bias.
                let already_covered = topological_predicate.as_ref().map_or(false, |tp| {
                    mw.predicates.iter().all(|p| p == tp)
                });
                if !already_covered {
                    let predicate = if mw.predicates.len() == 1 {
                        mw.predicates[0].clone()
                    } else {
                        Predicate::And(mw.predicates.clone())
                    };
                    node = PlanNode::Filter { input: Box::new(node), predicate };
                }
            }
        }

        // 3. Optional Bias (φ) — from BIAS clause or implicit FIND SEMANTIC
        if let Some(bias) = &query.bias {
            node = PlanNode::Bias {
                input: Box::new(node),
                scoring_fn: bias.scoring_fn.clone(),
                args: bias.args.clone(),
            };
        }

        // 4. Optional Sort (τ) — from ORDER BY
        if let Some(ob) = &query.order_by {
            node = PlanNode::Sort {
                input: Box::new(node),
                scoring_fn: ob.scoring_fn.clone(),
                args: ob.args.clone(),
                descending: ob.descending,
            };
        }

        // 5. Limit (λ) — always present with default
        node = PlanNode::Limit {
            input: Box::new(node),
            limit,
            offset,
        };

        // 6. Optional Deadline (⏱) — from WITHIN / EN
        if let Some(millis) = within_ms {
            // Check budget constraint: reject if predicted cost exceeds budget
            let adjusted = estimated_cost_ms * (1.0 + self.config.cost_buffer);
            if adjusted > millis as f64 {
                return Err(tesseract_common::error::Error::ServiceError(format!(
                    "Query would exceed latency budget: estimated {:.1}ms (with buffer), budget {}ms. \
                     Try reducing ef_search, limit, or removing the WITHIN clause.",
                    adjusted, millis
                )));
            }
            node = PlanNode::Deadline {
                input: Box::new(node),
                millis,
                estimated_cost_ms,
            };
        }

        Ok(node)
    }

    /// Derive a WeightMask from metadata WHERE predicates.
    fn derive_weight_mask(&self, metadata_where: &Option<MetadataWhere>) -> Option<WeightMask> {
        metadata_where.as_ref().and_then(|mw| {
            if mw.predicates.is_empty() {
                return None;
            }
            let mut weights = Vec::new();
            for pred in &mw.predicates {
                match pred {
                    Predicate::Comparison { field, operator, .. } => {
                        let dim = self.field_to_dim(field);
                        let weight = match operator {
                            ComparisonOp::Eq => 1.0,
                            ComparisonOp::Neq => 0.8,
                            _ => 0.5, // range operators: partial filter
                        };
                        weights.push((dim, weight as f32));
                    }
                    _ => {
                        // IN, BETWEEN, LIKE, AND: skip for Phase 3
                        continue;
                    }
                }
            }
            if weights.is_empty() { None } else { Some(WeightMask(weights)) }
        })
    }

    /// Map a metadata field name to a dimension index.
    fn field_to_dim(&self, field: &str) -> usize {
        let hash: u64 = field.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        (hash as usize) % self.config.dim
    }

    /// Compute ef_search based on WITHIN budget and other constraints.
    fn compute_ef_search(&self, within_ms: Option<u64>, _limit: usize) -> usize {
        match within_ms {
            Some(budget) => {
                let base = self.config.default_ef_search as f64;
                let scaled = (base * (budget as f64 / 100.0)).clamp(10.0, 200.0);
                scaled as usize
            }
            None => self.config.default_ef_search,
        }
    }

    /// Extract topological bias filters from the optional WITH METADATA WHERE
    /// clause.
    ///
    /// Converts simple predicates into `BiasFilter` entries:
    /// - `field = 'string_value'` → categorical bias
    /// - `field <op> <numeric>` → numerical bias (Eq, Gt, Ge, Lt, Le)
    /// - Complex predicates (IN, BETWEEN, LIKE, AND, Neq) are skipped.
    fn extract_bias_filters(metadata_where: &Option<MetadataWhere>) -> Vec<BiasFilter> {
        match metadata_where {
            Some(mw) => mw
                .predicates
                .iter()
                .filter_map(|p| match p {
                    Predicate::Comparison {
                        field,
                        operator: ComparisonOp::Eq,
                        value: Literal::String(v),
                    } => Some(BiasFilter {
                        field: field.clone(),
                        kind: BiasKind::Category(v.clone()),
                    }),
                    Predicate::Comparison { field, operator, value } => {
                        // Numerical: Eq, Gt, Ge, Lt, Le with numeric value
                        let range_op = match operator {
                            ComparisonOp::Eq => RangeOp::Eq,
                            ComparisonOp::Gt => RangeOp::Gt,
                            ComparisonOp::Gte => RangeOp::Gte,
                            ComparisonOp::Lt => RangeOp::Lt,
                            ComparisonOp::Lte => RangeOp::Lte,
                            _ => return None,
                        };
                        match *value {
                            Literal::Integer(i) => Some(BiasFilter {
                                field: field.clone(),
                                kind: BiasKind::Numerical { value: i as f64, op: range_op },
                            }),
                            Literal::Float(f) => Some(BiasFilter {
                                field: field.clone(),
                                kind: BiasKind::Numerical { value: f, op: range_op },
                            }),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect(),
            None => vec![],
        }
    }

    /// Estimate the cost of executing a query plan in milliseconds.
    fn estimate_cost(&self, ef_search: usize, _mask: &Option<WeightMask>) -> f64 {
        let n = self.config.estimated_vector_count as f64;
        let log_n = n.ln();
        let dim = self.config.dim as f64;
        let ef = ef_search as f64;

        ef * dim * 2.0 * log_n * self.config.cost_per_distance_ms * (1.0 + self.config.cost_buffer)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Navigate to the innermost AnnScan node by unwrapping wrappers.
    fn find_ann_scan<'a>(node: &'a PlanNode) -> Option<&'a PlanNode> {
        match node {
            PlanNode::AnnScan { .. } => Some(node),
            PlanNode::Merge { left, .. } => find_ann_scan(left),
            PlanNode::Filter { input, .. }
            | PlanNode::Bias { input, .. }
            | PlanNode::Sort { input, .. }
            | PlanNode::Limit { input, .. }
            | PlanNode::Deadline { input, .. } => find_ann_scan(input),
        }
    }

    /// Extract limit/offset from the outermost Limit node.
    fn find_limit(node: &PlanNode) -> Option<(usize, usize)> {
        match node {
            PlanNode::Limit { limit, offset, .. } => Some((*limit, *offset)),
            PlanNode::Deadline { input, .. } => find_limit(input),
            _ => None,
        }
    }

    /// Build a Query with just a SIMILARITY clause (for minimal-query tests).
    fn query_similarity(field: &str, text: &str) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        }
    }

    /// Build a Query with a SIMILARITY + LIMIT clause.
    fn query_with_limit(field: &str, text: &str, count: u64) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: Some(Limit { count }),
            offset: None,
            within: None,
        }
    }

    /// Build a Query with SIMILARITY + WITHIN.
    fn query_with_within(field: &str, text: &str, millis: u64) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: Some(Within { millis }),
        }
    }

    /// Build a Query with SIMILARITY + WHERE.
    fn query_with_where(field: &str, text: &str, pred: Predicate) -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: field.into(), query_text: text.into(), vector: None }),
            metadata_where: Some(MetadataWhere { predicates: vec![pred] }),
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        }
    }

    /// Build a Query with no similarity clause (invalid — for error tests).
    fn query_no_similarity() -> Query {
        Query {
            find: "SIMILARITY".into(),
            similarity: None,
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
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

        let plan = planner.plan_to_tree(&query).unwrap();

        // Top-level: Limit
        let (limit, offset) = find_limit(&plan).expect("expected Limit node");
        assert_eq!(limit, 10);
        assert_eq!(offset, 0);

        // Innermost: AnnScan
        let scan = find_ann_scan(&plan).expect("expected AnnScan");
        match scan {
            PlanNode::AnnScan { field, ef_search, weight_mask, .. } => {
                assert_eq!(field, "emb");
                assert_eq!(*ef_search, 50);
                assert!(weight_mask.is_none());
            }
            _ => unreachable!(),
        }

        // No Deadline (no WITHIN)
        assert!(matches!(&plan, PlanNode::Limit { .. }));
    }

    // -----------------------------------------------------------------------
    // 2. Plan query with LIMIT
    // -----------------------------------------------------------------------

    #[test]
    fn plan_with_limit() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_with_limit("emb", "hello", 25);

        let plan = planner.plan_to_tree(&query).unwrap();
        let (limit, _) = find_limit(&plan).expect("expected Limit node");
        assert_eq!(limit, 25);
    }

    // -----------------------------------------------------------------------
    // 3. Plan query WITHIN budget — verify ef_search is scaled
    // -----------------------------------------------------------------------

    #[test]
    fn plan_within_budget_scales_ef() {
        let config =
            PlannerConfig { estimated_vector_count: 100, cost_per_distance_ms: 0.000_01, ..Default::default() };
        let planner = QueryPlanner::new(config);
        let query = query_with_within("emb", "test", 200);

        let plan = planner.plan_to_tree(&query).unwrap();

        // Top-level should be Deadline
        match &plan {
            PlanNode::Deadline { millis, estimated_cost_ms, .. } => {
                assert_eq!(*millis, 200);
                assert!(*estimated_cost_ms > 0.0);
            }
            _ => panic!("Expected Deadline as top-level node"),
        }

        // AnnScan should have scaled ef_search
        let scan = find_ann_scan(&plan).expect("expected AnnScan");
        match scan {
            PlanNode::AnnScan { ef_search, .. } => {
                assert_eq!(*ef_search, 100);
            }
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------------
    // 4. Plan query WITHIN budget too tight → Err
    // -----------------------------------------------------------------------

    #[test]
    fn plan_within_budget_too_tight_returns_err() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_with_within("emb", "test", 1);

        let err = planner.plan_to_tree(&query).unwrap_err();
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

        let plan = planner.plan_to_tree(&query).unwrap();

        // Structure should be Limit(Filter(AnnScan))
        let scan = find_ann_scan(&plan).expect("expected AnnScan");
        match scan {
            PlanNode::AnnScan { weight_mask, .. } => {
                assert!(weight_mask.is_some(), "expected a WeightMask");
                let mask = weight_mask.as_ref().unwrap();
                assert_eq!(mask.0.len(), 1);
                let (dim, weight) = mask.0[0];
                assert!(dim < 384, "dim index {dim} out of range");
                assert!((weight - 1.0_f32).abs() < f32::EPSILON, "expected weight 1.0, got {weight}");
            }
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------------
    // 6. Plan query with empty similarity → Err
    // -----------------------------------------------------------------------

    #[test]
    fn plan_no_similarity_returns_err() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_no_similarity();

        let err = planner.plan_to_tree(&query).unwrap_err();
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
        assert_eq!(planner.compute_ef_search(Some(1), 10), 10);
    }

    #[test]
    fn compute_ef_search_clamps_maximum() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        assert_eq!(planner.compute_ef_search(Some(500), 10), 200);
    }

    // -----------------------------------------------------------------------
    // Extra: With ORDER BY, plan preserves scoring_fn in Sort node
    // -----------------------------------------------------------------------

    #[test]
    fn plan_with_order_by() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: Some(OrderBy {
                scoring_fn: "relevance_clicks".into(),
                args: vec!["current_user".into()],
                descending: true,
            }),
            limit: None,
            offset: None,
            within: None,
        };

        let plan = planner.plan_to_tree(&query).unwrap();

        // Structure: Limit(Sort(AnnScan))
        let sort = match &plan {
            PlanNode::Limit { input, .. } => match input.as_ref() {
                PlanNode::Sort { scoring_fn, args, descending, .. } => {
                    assert_eq!(scoring_fn, "relevance_clicks");
                    assert_eq!(args[0], "current_user");
                    assert!(*descending);
                    true
                }
                _ => false,
            },
            _ => false,
        };
        assert!(sort, "expected Limit > Sort structure");
    }

    // -----------------------------------------------------------------------
    // New tests: FIND SEMANTIC, BIAS, OFFSET, PROJECT ON, validation
    // -----------------------------------------------------------------------

    #[test]
    fn plan_find_semantic() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SEMANTIC".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        };

        // SEMANTIC is accepted and produces a valid plan (same as SIMILARITY for now)
        let plan = planner.plan_to_tree(&query).unwrap();
        let (limit, _) = find_limit(&plan).expect("expected Limit");
        assert_eq!(limit, 10);
    }

    #[test]
    fn plan_with_offset() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: None,
            limit: Some(Limit { count: 10 }),
            offset: Some(Offset { count: 5 }),
            within: None,
        };

        let plan = planner.plan_to_tree(&query).unwrap();
        let (limit, offset) = find_limit(&plan).expect("expected Limit node");
        assert_eq!(limit, 10);
        assert_eq!(offset, 5);
    }

    #[test]
    fn plan_with_bias() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: Some(Bias { scoring_fn: "recency".into(), args: vec![] }),
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        };

        let plan = planner.plan_to_tree(&query).unwrap();

        // Structure: Limit(Bias(AnnScan))
        match &plan {
            PlanNode::Limit { input, .. } => match input.as_ref() {
                PlanNode::Bias { scoring_fn, args, .. } => {
                    assert_eq!(scoring_fn, "recency");
                    assert!(args.is_empty());
                }
                _ => panic!("Expected Bias node"),
            },
            _ => panic!("Expected Limit node"),
        }
    }

    #[test]
    fn plan_with_project_on() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: Some(ProjectOn {
                projections: vec![Projection::Field("year".into())],
            }),
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        };

        // PROJECT ON is a topological hint, not a query operator.
        // It doesn't affect the plan tree structure — verify the plan is valid.
        let plan = planner.plan_to_tree(&query).unwrap();
        let (limit, _) = find_limit(&plan).expect("expected Limit");
        assert_eq!(limit, 10);
    }

    #[test]
    fn plan_rejects_bias_and_order_by() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: Some(Bias { scoring_fn: "recency".into(), args: vec![] }),
            order_by: Some(OrderBy {
                scoring_fn: "score".into(),
                args: vec![],
                descending: false,
            }),
            limit: None,
            offset: None,
            within: None,
        };

        let err = planner.plan_to_tree(&query).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mutually exclusive"), "got: {msg}");
    }

    #[test]
    fn plan_accepts_bias_without_order_by() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: Some(Bias { scoring_fn: "recency".into(), args: vec![] }),
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        };

        assert!(planner.plan_to_tree(&query).is_ok());
    }

    #[test]
    fn plan_accepts_order_by_without_bias() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            project_on: None,
            bias: None,
            order_by: Some(OrderBy {
                scoring_fn: "score".into(),
                args: vec![],
                descending: false,
            }),
            limit: None,
            offset: None,
            within: None,
        };

        assert!(planner.plan_to_tree(&query).is_ok());
    }

    #[test]
    fn plan_default_offset_is_zero() {
        let planner = QueryPlanner::new(PlannerConfig::default());
        let query = query_similarity("emb", "test");

        let plan = planner.plan_to_tree(&query).unwrap();
        let (_, offset) = find_limit(&plan).expect("expected Limit node");
        assert_eq!(offset, 0);
    }
}
