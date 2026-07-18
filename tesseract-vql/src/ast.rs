// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::fmt;

/// A parsed VQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The FIND type (e.g., "SIMILARITY").
    pub find: String,
    /// The similarity expression, always present for FIND SIMILARITY queries.
    pub similarity: Option<SimilarityExpr>,
    /// Optional metadata filter clause.
    pub metadata_where: Option<MetadataWhere>,
    /// Optional ordering clause.
    pub order_by: Option<OrderBy>,
    /// Optional result limit.
    pub limit: Option<Limit>,
    /// Optional latency budget.
    pub within: Option<Within>,
}

/// A `SIMILARITY(field, 'query_text')` or pre-computed vector expression.
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarityExpr {
    /// The embedding field name.
    pub field: String,
    /// The query text string (None if pre-computed vector is used).
    pub query_text: String,
    /// Pre-computed vector (None for text-based queries).
    pub vector: Option<Vec<f64>>,
}

/// A `WITH METADATA WHERE` clause containing a list of predicates.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataWhere {
    pub predicates: Vec<Predicate>,
}

/// A predicate expression in a WHERE clause.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// A comparison: `field <op> value`.
    Comparison { field: String, operator: ComparisonOp, value: Literal },
    /// An IN list: `field IN (val1, val2, ...)`.
    In { field: String, values: Vec<Literal> },
    /// A BETWEEN range: `field BETWEEN low AND high`.
    Between { field: String, low: Literal, high: Literal },
    /// Multiple predicates combined with AND.
    And(Vec<Predicate>),
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComparisonOp {
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
}

impl fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComparisonOp::Eq => write!(f, "="),
            ComparisonOp::Neq => write!(f, "!="),
            ComparisonOp::Lt => write!(f, "<"),
            ComparisonOp::Gt => write!(f, ">"),
            ComparisonOp::Lte => write!(f, "<="),
            ComparisonOp::Gte => write!(f, ">="),
        }
    }
}

/// A literal value used in predicates.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

/// An `ORDER BY` clause with a scoring function and arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    /// The scoring function name.
    pub scoring_fn: String,
    /// Arguments to the scoring function.
    pub args: Vec<String>,
    /// Whether results should be sorted in descending order.
    pub descending: bool,
}

/// A `LIMIT N` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub count: u64,
}

/// A `WITHIN Nms` latency budget clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Within {
    pub millis: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_op_display() {
        assert_eq!(ComparisonOp::Eq.to_string(), "=");
        assert_eq!(ComparisonOp::Neq.to_string(), "!=");
        assert_eq!(ComparisonOp::Lt.to_string(), "<");
        assert_eq!(ComparisonOp::Gt.to_string(), ">");
        assert_eq!(ComparisonOp::Lte.to_string(), "<=");
        assert_eq!(ComparisonOp::Gte.to_string(), ">=");
    }

    #[test]
    fn query_ast_derives_debug_clone_partial_eq() {
        let q = Query {
            find: "SIMILARITY".into(),
            similarity: Some(SimilarityExpr { field: "emb".into(), query_text: "test".into(), vector: None }),
            metadata_where: None,
            order_by: None,
            limit: None,
            within: None,
        };
        // Clone and compare — if Debug, Clone, PartialEq aren't derived, this won't compile
        let q2 = q.clone();
        assert_eq!(q, q2);
    }
}
