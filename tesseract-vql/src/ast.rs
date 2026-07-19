// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::fmt;

/// A parsed VQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The FIND type (e.g., "SIMILARITY" or "SEMANTIC").
    pub find: String,
    /// The similarity expression, always present for FIND queries.
    pub similarity: Option<SimilarityExpr>,
    /// Optional metadata filter clause.
    pub metadata_where: Option<MetadataWhere>,
    /// Optional topological projection clause.
    pub project_on: Option<ProjectOn>,
    /// Optional bias / personalization clause.
    pub bias: Option<Bias>,
    /// Optional ordering clause.
    pub order_by: Option<OrderBy>,
    /// Optional result limit.
    pub limit: Option<Limit>,
    /// Optional pagination offset.
    pub offset: Option<Offset>,
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
    /// A LIKE pattern match: `field LIKE 'pattern'`.
    Like { field: String, pattern: String },
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
    Null,
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

/// A `PROJECT ON` clause with a list of projection expressions.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectOn {
    pub projections: Vec<Projection>,
}

/// A single projection expression inside `PROJECT ON`.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// A plain field reference.
    Field(String),
    /// A field with an alias: `field AS alias`.
    Aliased { field: String, alias: String },
    /// A function applied to a field: `fn(field)`.
    Function { name: String, field: String },
}

/// A `BIAS scoring_fn(args)` personalization clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Bias {
    pub scoring_fn: String,
    pub args: Vec<String>,
}

/// An `OFFSET N` pagination clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Offset {
    pub count: u64,
}

/// A `WITHIN Nms` latency budget clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Within {
    pub millis: u64,
}

// ---------------------------------------------------------------------------
// Display implementations
// ---------------------------------------------------------------------------

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FIND {}", self.find)?;
        if let Some(ref sim) = self.similarity {
            write!(f, "({}, '{}')", sim.field, sim.query_text)?;
        }
        if let Some(ref mw) = self.metadata_where {
            write!(f, "\n  WITH METADATA WHERE {mw}")?;
        }
        if let Some(ref po) = self.project_on {
            write!(f, "\n  PROJECT ON {po}")?;
        }
        if let Some(ref b) = self.bias {
            write!(f, "\n  BIAS {b}")?;
        }
        if let Some(ref ob) = self.order_by {
            write!(f, "\n  ORDER BY {ob}")?;
        }
        if let Some(ref l) = self.limit {
            write!(f, "\n  LIMIT {}", l.count)?;
        }
        if let Some(ref o) = self.offset {
            write!(f, "\n  OFFSET {}", o.count)?;
        }
        if let Some(ref w) = self.within {
            write!(f, "\n  WITHIN {}ms", w.millis)?;
        }
        Ok(())
    }
}

impl fmt::Display for SimilarityExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, '{}')", self.field, self.query_text)
    }
}

impl fmt::Display for MetadataWhere {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, pred) in self.predicates.iter().enumerate() {
            if i > 0 {
                write!(f, " AND ")?;
            }
            write!(f, "{pred}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Predicate::Comparison { field, operator, value } => {
                write!(f, "{field} {operator} {value}")
            }
            Predicate::In { field, values } => {
                write!(f, "{field} IN (")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            Predicate::Between { field, low, high } => {
                write!(f, "{field} BETWEEN {low} AND {high}")
            }
            Predicate::Like { field, pattern } => {
                write!(f, "{field} LIKE '{pattern}'")
            }
            Predicate::And(preds) => {
                for (i, p) in preds.iter().enumerate() {
                    if i > 0 {
                        write!(f, " AND ")?;
                    }
                    write!(f, "{p}")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::String(s) => write!(f, "'{s}'"),
            Literal::Integer(i) => write!(f, "{i}"),
            Literal::Float(fl) => write!(f, "{fl}"),
            Literal::Boolean(b) => write!(f, "{b}"),
            Literal::Null => write!(f, "null"),
        }
    }
}

impl fmt::Display for Bias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.scoring_fn)?;
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{arg}")?;
        }
        write!(f, ")")
    }
}

impl fmt::Display for OrderBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.scoring_fn)?;
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{arg}")?;
        }
        write!(f, ")")?;
        if self.descending {
            write!(f, " DESC")?;
        }
        Ok(())
    }
}

impl fmt::Display for ProjectOn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.projections.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{p}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Projection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Projection::Field(name) => write!(f, "{name}"),
            Projection::Aliased { field, alias } => write!(f, "{field} AS {alias}"),
            Projection::Function { name, field } => write!(f, "{name}({field})"),
        }
    }
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
            project_on: None,
            bias: None,
            order_by: None,
            limit: None,
            offset: None,
            within: None,
        };
        // Clone and compare — if Debug, Clone, PartialEq aren't derived, this won't compile
        let q2 = q.clone();
        assert_eq!(q, q2);
    }
}
