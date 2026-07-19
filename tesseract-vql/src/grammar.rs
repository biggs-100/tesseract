// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use nom::{
    IResult,
    branch::alt,
    bytes::complete::{tag, take_while},
    character::complete::{alpha1, alphanumeric1, digit1, multispace0},
    combinator::{map, map_res, opt, recognize, value},
    multi::{many0, separated_list0, separated_list1},
    sequence::{delimited, preceded, tuple},
};
use nom_locate::LocatedSpan;

use crate::ast::*;

/// A located span type for tracking source positions.
pub type Span<'a> = LocatedSpan<&'a str>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Consumes zero or more whitespace characters (spaces, tabs, newlines).
fn ws<'a>(input: Span<'a>) -> IResult<Span<'a>, Span<'a>> {
    multispace0(input)
}

/// Parses a valid VQL identifier: starts with `_` or an alphabetic character,
/// followed by zero or more alphanumeric or `_` characters.
fn identifier(input: Span) -> IResult<Span, String> {
    map(recognize(tuple((alt((alpha1, tag("_"))), many0(alt((alphanumeric1, tag("_"))))))), |s: Span| {
        s.fragment().to_string()
    })(input)
}

/// Parses a single-quoted string literal: `'content'`.
fn string_literal(input: Span) -> IResult<Span, String> {
    let (input, _) = tag("'")(input)?;
    let (input, content) = take_while(|c: char| c != '\'')(input)?;
    let (input, _) = tag("'")(input)?;
    Ok((input, content.fragment().to_string()))
}

/// Parses an unsigned integer literal.
fn integer_literal(input: Span) -> IResult<Span, i64> {
    map_res(recognize(digit1), |s: Span| s.fragment().parse::<i64>())(input)
}

/// Parses a floating-point literal (requires a `.`).
fn float_literal(input: Span) -> IResult<Span, f64> {
    map_res(recognize(tuple((digit1, tag("."), digit1))), |s: Span| s.fragment().parse::<f64>())(input)
}

/// Parses a boolean literal: `true` or `false`.
fn boolean_literal(input: Span) -> IResult<Span, bool> {
    alt((value(true, tag("true")), value(false, tag("false"))))(input)
}

/// Parses a `null` literal value.
fn null_literal(input: Span) -> IResult<Span, Literal> {
    value(Literal::Null, tag("null"))(input)
}

/// Parses any literal value: string, boolean, float, integer, or null (in that order).
fn literal(input: Span) -> IResult<Span, Literal> {
    alt((
        map(string_literal, Literal::String),
        map(boolean_literal, Literal::Boolean),
        map(float_literal, Literal::Float),
        map(integer_literal, Literal::Integer),
        null_literal,
    ))(input)
}

/// Parses an ORDER BY function argument (identifier or string literal).
fn order_by_arg(input: Span) -> IResult<Span, String> {
    alt((string_literal, identifier))(input)
}

// ---------------------------------------------------------------------------
// Comparison operators
// ---------------------------------------------------------------------------

fn comparison_op(input: Span) -> IResult<Span, ComparisonOp> {
    alt((
        value(ComparisonOp::Neq, tag("!=")),
        value(ComparisonOp::Lte, tag("<=")),
        value(ComparisonOp::Gte, tag(">=")),
        value(ComparisonOp::Eq, tag("=")),
        value(ComparisonOp::Lt, tag("<")),
        value(ComparisonOp::Gt, tag(">")),
    ))(input)
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// Parses a comparison predicate: `field <op> value`.
fn comparison_predicate(input: Span) -> IResult<Span, Predicate> {
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, operator) = comparison_op(input)?;
    let (input, _) = ws(input)?;
    let (input, value) = literal(input)?;
    Ok((input, Predicate::Comparison { field, operator, value }))
}

/// Parses an IN predicate: `field IN (val1, val2, ...)`.
fn in_predicate(input: Span) -> IResult<Span, Predicate> {
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("IN")(input)?;
    let (input, _) = ws(input)?;
    let (input, values) = delimited(tag("("), separated_list0(tuple((ws, tag(","), ws)), literal), tag(")"))(input)?;
    Ok((input, Predicate::In { field, values }))
}

/// Parses a BETWEEN predicate: `field BETWEEN low AND high`.
fn between_predicate(input: Span) -> IResult<Span, Predicate> {
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("BETWEEN")(input)?;
    let (input, _) = ws(input)?;
    let (input, low) = literal(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("AND")(input)?;
    let (input, _) = ws(input)?;
    let (input, high) = literal(input)?;
    Ok((input, Predicate::Between { field, low, high }))
}

/// Parses a LIKE predicate: `field LIKE 'pattern'`.
fn like_predicate(input: Span) -> IResult<Span, Predicate> {
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("LIKE")(input)?;
    let (input, _) = ws(input)?;
    let (input, pattern) = string_literal(input)?;
    Ok((input, Predicate::Like { field, pattern }))
}

/// Parses one or more predicates joined by AND. A single predicate is returned
/// as-is; multiple predicates are wrapped in `Predicate::And`.
fn and_expression(input: Span) -> IResult<Span, Predicate> {
    let (input, predicates) = separated_list1(
        tuple((ws, tag("AND"), ws)),
        alt((in_predicate, between_predicate, like_predicate, comparison_predicate)),
    )(input)?;

    if predicates.len() == 1 {
        Ok((input, predicates.into_iter().next().unwrap()))
    } else {
        Ok((input, Predicate::And(predicates)))
    }
}

// ---------------------------------------------------------------------------
// Clause combinators
// ---------------------------------------------------------------------------

/// Parses a `VECTOR(f1, f2, ...)` literal.
fn vector_literal(input: Span) -> IResult<Span, Vec<f64>> {
    let (input, _) = tag("VECTOR")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("(")(input)?;
    let (input, _) = ws(input)?;
    let (input, values) = separated_list0(tuple((ws, tag(","), ws)), float_literal)(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag(")")(input)?;
    Ok((input, values))
}

/// Parses the FIND expression including the find type (SIMILARITY or SEMANTIC),
/// the field, and the query source (text string or VECTOR literal).
///
/// Returns `(find_type, SimilarityExpr)`.
fn find_expr(input: Span) -> IResult<Span, (String, SimilarityExpr)> {
    let (input, find_type) = alt((
        value("SIMILARITY", tag("SIMILARITY")),
        value("SEMANTIC", tag("SEMANTIC")),
    ))(input)?;
    let (input, _) = tag("(")(input)?;
    let (input, _) = ws(input)?;
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag(",")(input)?;
    let (input, _) = ws(input)?;
    // Try VECTOR(...) first, then string literal
    let (input, (query_text, vector)) = alt((
        map(vector_literal, |v| (String::new(), Some(v))),
        map(string_literal, |s| (s, None)),
    ))(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag(")")(input)?;
    Ok((input, (find_type.to_string(), SimilarityExpr { field, query_text, vector })))
}

/// Parses `WITH METADATA WHERE <predicate>`.
fn metadata_where_clause(input: Span) -> IResult<Span, MetadataWhere> {
    let (input, _) = tag("WITH")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("METADATA")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("WHERE")(input)?;
    let (input, _) = ws(input)?;
    let (input, predicate) = and_expression(input)?;
    Ok((input, MetadataWhere { predicates: vec![predicate] }))
}

/// Parses `ORDER BY <scoring_fn>(<args>) [DESC | ASC]`.
fn order_by_clause(input: Span) -> IResult<Span, OrderBy> {
    let (input, _) = tag("ORDER")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("BY")(input)?;
    let (input, _) = ws(input)?;
    let (input, scoring_fn) = identifier(input)?;
    let (input, args) = delimited(tag("("), separated_list0(tuple((ws, tag(","), ws)), order_by_arg), tag(")"))(input)?;
    let (input, direction) = opt(preceded(ws, alt((
        value(true, tag("DESC")),
        value(false, tag("ASC")),
    ))))(input)?;
    Ok((input, OrderBy { scoring_fn, args, descending: direction.unwrap_or(false) }))
}

/// Parses `LIMIT <count>`.
fn limit_clause(input: Span) -> IResult<Span, Limit> {
    let (input, _) = tag("LIMIT")(input)?;
    let (input, _) = ws(input)?;
    let (input, count) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    Ok((input, Limit { count }))
}

/// Parses `OFFSET <count>`.
fn offset_clause(input: Span) -> IResult<Span, Offset> {
    let (input, _) = tag("OFFSET")(input)?;
    let (input, _) = ws(input)?;
    let (input, count) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    Ok((input, Offset { count }))
}

/// Parses `WITHIN <millis>ms`.
fn within_clause(input: Span) -> IResult<Span, Within> {
    let (input, _) = tag("WITHIN")(input)?;
    let (input, _) = ws(input)?;
    let (input, millis) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    let (input, _) = tag("ms")(input)?;
    Ok((input, Within { millis }))
}

/// Parses `EN <millis>ms` (alias for WITHIN).
fn en_clause(input: Span) -> IResult<Span, Within> {
    let (input, _) = tag("EN")(input)?;
    let (input, _) = ws(input)?;
    let (input, millis) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    let (input, _) = tag("ms")(input)?;
    Ok((input, Within { millis }))
}

/// Parses `PROJECT ON field1, field2, ...`.
fn project_on_clause(input: Span) -> IResult<Span, ProjectOn> {
    let (input, _) = tag("PROJECT")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("ON")(input)?;
    let (input, _) = ws(input)?;
    let (input, projections) = separated_list1(tuple((ws, tag(","), ws)), projection)(input)?;
    Ok((input, ProjectOn { projections }))
}

/// Parses a single projection expression.
fn projection(input: Span) -> IResult<Span, Projection> {
    preceded(ws, alt((
        // function(field)
        map(
            tuple((identifier, preceded(ws, tag("(")), preceded(ws, identifier), preceded(ws, tag(")")))),
            |(name, _, field, _)| Projection::Function { name, field },
        ),
        // field AS alias
        map(
            tuple((identifier, preceded(ws, tag("AS")), preceded(ws, identifier))),
            |(field, _, alias)| Projection::Aliased { field, alias },
        ),
        // field
        map(identifier, Projection::Field),
    )))(input)
}

/// Parses `BIAS <scoring_fn>(<args>)`.
fn bias_clause(input: Span) -> IResult<Span, Bias> {
    let (input, _) = tag("BIAS")(input)?;
    let (input, _) = ws(input)?;
    let (input, scoring_fn) = identifier(input)?;
    let (input, args) = delimited(tag("("), separated_list0(tuple((ws, tag(","), ws)), identifier), tag(")"))(input)?;
    Ok((input, Bias { scoring_fn, args }))
}

// ---------------------------------------------------------------------------
// ParsedClause — internal enum for order-independent clause parsing
// ---------------------------------------------------------------------------

/// Internal enum representing any optional clause after FIND.
#[derive(Debug, Clone, PartialEq)]
enum ParsedClause {
    MetadataWhere(MetadataWhere),
    ProjectOn(ProjectOn),
    Bias(Bias),
    OrderBy(OrderBy),
    Limit(Limit),
    Offset(Offset),
    Within(Within),
}

/// Tries to parse any one clause from the remaining input.
/// Leading whitespace has already been consumed before this call.
fn parse_any_clause(input: Span) -> IResult<Span, ParsedClause> {
    alt((
        map(metadata_where_clause, ParsedClause::MetadataWhere),
        map(project_on_clause, ParsedClause::ProjectOn),
        map(bias_clause, ParsedClause::Bias),
        map(order_by_clause, ParsedClause::OrderBy),
        map(limit_clause, ParsedClause::Limit),
        map(offset_clause, ParsedClause::Offset),
        map(within_clause, ParsedClause::Within),
        map(en_clause, ParsedClause::Within),
    ))(input)
}

// ---------------------------------------------------------------------------
// Top-level query parser
// ---------------------------------------------------------------------------

/// Parses a complete VQL query. Expects `FIND SIMILARITY(...)` or
/// `FIND SEMANTIC(...)` followed by zero or more optional clauses in any
/// order, with no trailing content.
pub fn query(input: Span) -> IResult<Span, Query> {
    let (input, _) = tag("FIND")(input)?;
    let (input, _) = ws(input)?;
    let (input, (find_type, similarity)) = find_expr(input)?;

    let mut metadata_where: Option<MetadataWhere> = None;
    let mut project_on: Option<ProjectOn> = None;
    let mut bias: Option<Bias> = None;
    let mut order_by: Option<OrderBy> = None;
    let mut limit: Option<Limit> = None;
    let mut offset: Option<Offset> = None;
    let mut within: Option<Within> = None;
    let mut remaining = input;

    loop {
        // Consume whitespace before attempting a clause
        let (rest, _) = ws(remaining)?;
        if rest.fragment().is_empty() {
            break;
        }

        match parse_any_clause(rest) {
            Ok((rest2, ParsedClause::MetadataWhere(v))) => {
                if metadata_where.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                metadata_where = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::ProjectOn(v))) => {
                if project_on.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                project_on = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::Bias(v))) => {
                if bias.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                bias = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::OrderBy(v))) => {
                if order_by.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                order_by = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::Limit(v))) => {
                if limit.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                limit = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::Offset(v))) => {
                if offset.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                offset = Some(v);
                remaining = rest2;
            }
            Ok((rest2, ParsedClause::Within(v))) => {
                if within.is_some() {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Alt,
                    )));
                }
                within = Some(v);
                remaining = rest2;
            }
            Err(nom::Err::Error(_)) => break,
            Err(e) => return Err(e),
        }
    }

    // Consume trailing whitespace
    let (remaining, _) = ws(remaining)?;

    // Reject trailing content after all clauses are consumed
    if !remaining.fragment().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(
            remaining,
            nom::error::ErrorKind::Eof,
        )));
    }

    // Validate: OFFSET requires LIMIT
    if offset.is_some() && limit.is_none() {
        return Err(nom::Err::Error(nom::error::Error::new(
            remaining,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((
        remaining,
        Query {
            find: find_type,
            similarity: Some(similarity),
            metadata_where,
            project_on,
            bias,
            order_by,
            limit,
            offset,
            within,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: parse a full query from a string
    // -----------------------------------------------------------------------
    fn parse_vql(input: &str) -> Result<Query, nom::Err<nom::error::Error<Span<'_>>>> {
        let span = Span::new(input);
        match query(span) {
            Ok((_, q)) => Ok(q),
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // FIND SIMILARITY — mandatory clause
    // -----------------------------------------------------------------------

    #[test]
    fn parse_minimal_query() {
        let q = parse_vql("FIND SIMILARITY(emb, 'hello world')").unwrap();
        assert_eq!(q.find, "SIMILARITY");
        let sim = q.similarity.unwrap();
        assert_eq!(sim.field, "emb");
        assert_eq!(sim.query_text, "hello world");
        assert!(q.metadata_where.is_none());
        assert!(q.project_on.is_none());
        assert!(q.bias.is_none());
        assert!(q.order_by.is_none());
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
        assert!(q.within.is_none());
    }

    #[test]
    fn parse_similarity_with_multiple_underscores() {
        let q = parse_vql("FIND SIMILARITY(my_embedding_field, 'test')").unwrap();
        let sim = q.similarity.unwrap();
        assert_eq!(sim.field, "my_embedding_field");
        assert_eq!(sim.query_text, "test");
    }

    #[test]
    fn parse_similarity_empty_query_text() {
        let q = parse_vql("FIND SIMILARITY(emb, '')").unwrap();
        assert_eq!(q.similarity.unwrap().query_text, "");
    }

    #[test]
    fn reject_no_find_keyword() {
        let err = parse_vql("SIMILARITY(emb, 'text')");
        assert!(err.is_err());
    }

    #[test]
    fn reject_missing_closing_paren_similarity() {
        // Missing closing paren after 'text'
        let result = parse_vql("FIND SIMILARITY(emb, 'text'");
        assert!(result.is_err());
    }

    #[test]
    fn reject_missing_opening_paren_similarity() {
        let result = parse_vql("FIND SIMILARITY emb, 'text')");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // FIND SEMANTIC — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_semantic_query() {
        let q = parse_vql("FIND SEMANTIC(emb, 'hello world')").unwrap();
        assert_eq!(q.find, "SEMANTIC");
        let sim = q.similarity.unwrap();
        assert_eq!(sim.field, "emb");
        assert_eq!(sim.query_text, "hello world");
        assert!(q.metadata_where.is_none());
        assert!(q.project_on.is_none());
        assert!(q.bias.is_none());
        assert!(q.order_by.is_none());
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
        assert!(q.within.is_none());
    }

    // -----------------------------------------------------------------------
    // VECTOR source — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_vector_source() {
        let q = parse_vql("FIND SIMILARITY(emb, VECTOR(0.1, 0.2, 0.3))").unwrap();
        let sim = q.similarity.unwrap();
        assert_eq!(sim.field, "emb");
        assert!(sim.query_text.is_empty());
        let vec = sim.vector.unwrap();
        assert_eq!(vec, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn parse_vector_source_single_element() {
        let q = parse_vql("FIND SIMILARITY(emb, VECTOR(42.0))").unwrap();
        let sim = q.similarity.unwrap();
        let vec = sim.vector.unwrap();
        assert_eq!(vec, vec![42.0]);
    }

    #[test]
    fn parse_vector_source_empty() {
        let q = parse_vql("FIND SIMILARITY(emb, VECTOR())").unwrap();
        let sim = q.similarity.unwrap();
        let vec = sim.vector.unwrap();
        assert!(vec.is_empty());
    }

    #[test]
    fn parse_semantic_with_vector_source() {
        let q = parse_vql("FIND SEMANTIC(emb, VECTOR(0.5, 0.6))").unwrap();
        assert_eq!(q.find, "SEMANTIC");
        let sim = q.similarity.unwrap();
        assert_eq!(sim.field, "emb");
        let vec = sim.vector.unwrap();
        assert_eq!(vec, vec![0.5, 0.6]);
    }

    // -----------------------------------------------------------------------
    // WITH METADATA WHERE
    // -----------------------------------------------------------------------

    #[test]
    fn parse_metadata_where_single_eq() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE color = 'red'").unwrap();
        let mw = q.metadata_where.unwrap();
        assert_eq!(mw.predicates.len(), 1);
        match &mw.predicates[0] {
            Predicate::Comparison { field, operator, value } => {
                assert_eq!(field, "color");
                assert_eq!(*operator, ComparisonOp::Eq);
                assert_eq!(*value, Literal::String("red".to_string()));
            }
            other => panic!("Expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn parse_metadata_where_neq() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE color != 'blue'").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Comparison { field, operator, .. } => {
                assert_eq!(field, "color");
                assert_eq!(*operator, ComparisonOp::Neq);
            }
            other => panic!("Expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn parse_metadata_where_lt_gt_lte_gte() {
        let lt = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE age < 30").unwrap();
        assert_eq!(
            lt.metadata_where.unwrap().predicates[0],
            Predicate::Comparison { field: "age".into(), operator: ComparisonOp::Lt, value: Literal::Integer(30) }
        );

        let gt = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE age > 30").unwrap();
        assert_eq!(
            gt.metadata_where.unwrap().predicates[0],
            Predicate::Comparison { field: "age".into(), operator: ComparisonOp::Gt, value: Literal::Integer(30) }
        );

        let lte = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE age <= 30").unwrap();
        assert_eq!(
            lte.metadata_where.unwrap().predicates[0],
            Predicate::Comparison { field: "age".into(), operator: ComparisonOp::Lte, value: Literal::Integer(30) }
        );

        let gte = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE age >= 30").unwrap();
        assert_eq!(
            gte.metadata_where.unwrap().predicates[0],
            Predicate::Comparison { field: "age".into(), operator: ComparisonOp::Gte, value: Literal::Integer(30) }
        );
    }

    #[test]
    fn parse_metadata_where_integer_literal() {
        let q = parse_vql("FIND SIMILARITY(emb, 'ml') WITH METADATA WHERE date >= 2024").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Comparison { field, operator, value } => {
                assert_eq!(field, "date");
                assert_eq!(*operator, ComparisonOp::Gte);
                assert_eq!(*value, Literal::Integer(2024));
            }
            other => panic!("Expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn parse_metadata_where_float_literal() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE price >= 2.5").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Comparison { value, .. } => {
                assert_eq!(*value, Literal::Float(2.5));
            }
            other => panic!("Expected Comparison, got {:?}", other),
        }
    }

    #[test]
    fn parse_metadata_where_boolean_literal() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE active = true").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Comparison { value, .. } => {
                assert_eq!(*value, Literal::Boolean(true));
            }
            other => panic!("Expected Comparison, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // IN predicate
    // -----------------------------------------------------------------------

    #[test]
    fn parse_in_predicate() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE category IN ('a', 'b', 'c')").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::In { field, values } => {
                assert_eq!(field, "category");
                assert_eq!(
                    *values,
                    vec![Literal::String("a".into()), Literal::String("b".into()), Literal::String("c".into()),]
                );
            }
            other => panic!("Expected In, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // BETWEEN predicate
    // -----------------------------------------------------------------------

    #[test]
    fn parse_between_predicate() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE year BETWEEN 2020 AND 2025").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Between { field, low, high } => {
                assert_eq!(field, "year");
                assert_eq!(*low, Literal::Integer(2020));
                assert_eq!(*high, Literal::Integer(2025));
            }
            other => panic!("Expected Between, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // LIKE predicate — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_like_predicate() {
        let q =
            parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE cuisine LIKE 'ita%'").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Like { field, pattern } => {
                assert_eq!(field, "cuisine");
                assert_eq!(pattern, "ita%");
            }
            other => panic!("Expected Like, got {:?}", other),
        }
    }

    #[test]
    fn parse_like_with_and_combination() {
        let q = parse_vql(
            "FIND SIMILARITY(emb, 'q') WITH METADATA WHERE cuisine LIKE 'ita%' AND year >= 2020",
        )
        .unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::And(preds) => {
                assert_eq!(preds.len(), 2);
                assert!(matches!(&preds[0], Predicate::Like { field, .. } if field == "cuisine"));
                assert!(matches!(&preds[1], Predicate::Comparison { field, .. } if field == "year"));
            }
            other => panic!("Expected And, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // AND combination
    // -----------------------------------------------------------------------

    #[test]
    fn parse_and_combination() {
        let q = parse_vql(
            "FIND SIMILARITY(emb, 'AI') WITH METADATA WHERE date >= 2024 AND category IN ('tech', 'science')",
        )
        .unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::And(preds) => {
                assert_eq!(preds.len(), 2);
                assert!(matches!(
                    &preds[0],
                    Predicate::Comparison { field, operator: ComparisonOp::Gte, value: Literal::Integer(2024) }
                    if field == "date"
                ));
                assert!(matches!(
                    &preds[1],
                    Predicate::In { field, values } if field == "category" && values.len() == 2
                ));
            }
            other => panic!("Expected And, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // LIMIT
    // -----------------------------------------------------------------------

    #[test]
    fn parse_limit() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') LIMIT 10").unwrap();
        assert_eq!(q.limit.unwrap().count, 10);
    }

    #[test]
    fn parse_limit_large_number() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') LIMIT 999999").unwrap();
        assert_eq!(q.limit.unwrap().count, 999999);
    }

    // -----------------------------------------------------------------------
    // OFFSET — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_offset_with_limit() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') LIMIT 50 OFFSET 100").unwrap();
        assert_eq!(q.limit.unwrap().count, 50);
        assert_eq!(q.offset.unwrap().count, 100);
    }

    #[test]
    fn parse_offset_with_order_independence() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') OFFSET 100 LIMIT 50").unwrap();
        assert_eq!(q.limit.unwrap().count, 50);
        assert_eq!(q.offset.unwrap().count, 100);
    }

    #[test]
    fn reject_offset_without_limit() {
        let result = parse_vql("FIND SIMILARITY(emb, 'q') OFFSET 100");
        assert!(result.is_err());
    }

    #[test]
    fn parse_large_offset() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') LIMIT 10 OFFSET 9999").unwrap();
        assert_eq!(q.offset.unwrap().count, 9999);
    }

    // -----------------------------------------------------------------------
    // WITHIN
    // -----------------------------------------------------------------------

    #[test]
    fn parse_within() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITHIN 100ms").unwrap();
        assert_eq!(q.within.unwrap().millis, 100);
    }

    #[test]
    fn reject_within_missing_ms_suffix() {
        let result = parse_vql("FIND SIMILARITY(emb, 'q') WITHIN 100");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // EN (alias for WITHIN) — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_en() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') EN 100ms").unwrap();
        assert_eq!(q.within.unwrap().millis, 100);
    }

    #[test]
    fn parse_en_rejects_missing_ms_suffix() {
        let result = parse_vql("FIND SIMILARITY(emb, 'q') EN 100");
        assert!(result.is_err());
    }

    #[test]
    fn en_and_within_are_aliases() {
        let en_q = parse_vql("FIND SIMILARITY(emb, 'q') EN 200ms").unwrap();
        let within_q = parse_vql("FIND SIMILARITY(emb, 'q') WITHIN 200ms").unwrap();
        assert_eq!(en_q.within.unwrap(), within_q.within.unwrap());
    }

    // -----------------------------------------------------------------------
    // PROJECT ON — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_project_on_single() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') PROJECT ON year").unwrap();
        let po = q.project_on.unwrap();
        assert_eq!(po.projections.len(), 1);
        assert_eq!(po.projections[0], Projection::Field("year".into()));
    }

    #[test]
    fn parse_project_on_multiple() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') PROJECT ON year, category").unwrap();
        let po = q.project_on.unwrap();
        assert_eq!(po.projections.len(), 2);
        assert_eq!(po.projections[0], Projection::Field("year".into()));
        assert_eq!(po.projections[1], Projection::Field("category".into()));
    }

    #[test]
    fn parse_project_on_aliased() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') PROJECT ON year AS y, category AS cat").unwrap();
        let po = q.project_on.unwrap();
        assert_eq!(po.projections.len(), 2);
        assert_eq!(po.projections[0], Projection::Aliased { field: "year".into(), alias: "y".into() });
        assert_eq!(po.projections[1], Projection::Aliased { field: "category".into(), alias: "cat".into() });
    }

    #[test]
    fn parse_project_on_function() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') PROJECT ON WEEK(date)").unwrap();
        let po = q.project_on.unwrap();
        assert_eq!(po.projections.len(), 1);
        assert_eq!(
            po.projections[0],
            Projection::Function { name: "WEEK".into(), field: "date".into() }
        );
    }

    #[test]
    fn parse_project_on_mixed() {
        let q =
            parse_vql("FIND SIMILARITY(emb, 'q') PROJECT ON year, category AS cat, WEEK(date)").unwrap();
        let po = q.project_on.unwrap();
        assert_eq!(po.projections.len(), 3);
        assert_eq!(po.projections[0], Projection::Field("year".into()));
        assert_eq!(
            po.projections[1],
            Projection::Aliased { field: "category".into(), alias: "cat".into() }
        );
        assert_eq!(
            po.projections[2],
            Projection::Function { name: "WEEK".into(), field: "date".into() }
        );
    }

    // -----------------------------------------------------------------------
    // BIAS — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_bias_simple() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') BIAS recency() LIMIT 10").unwrap();
        let bias = q.bias.unwrap();
        assert_eq!(bias.scoring_fn, "recency");
        assert!(bias.args.is_empty());
    }

    #[test]
    fn parse_bias_with_arg() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') BIAS personal(user_id)").unwrap();
        let bias = q.bias.unwrap();
        assert_eq!(bias.scoring_fn, "personal");
        assert_eq!(bias.args, vec!["user_id"]);
    }

    #[test]
    fn parse_bias_multiple_args() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') BIAS relevance_clicks(user_id, scope)").unwrap();
        let bias = q.bias.unwrap();
        assert_eq!(bias.scoring_fn, "relevance_clicks");
        assert_eq!(bias.args, vec!["user_id", "scope"]);
    }

    // -----------------------------------------------------------------------
    // null literal — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_null_literal_in_predicate() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') WITH METADATA WHERE field = null").unwrap();
        let mw = q.metadata_where.unwrap();
        match &mw.predicates[0] {
            Predicate::Comparison { value, .. } => {
                assert_eq!(*value, Literal::Null);
            }
            other => panic!("Expected Comparison with Null, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // ORDER BY
    // -----------------------------------------------------------------------

    #[test]
    fn parse_order_by() {
        let q =
            parse_vql("FIND SIMILARITY(emb, 'history') ORDER BY relevance_clicks(current_user) DESC LIMIT 5").unwrap();
        let ob = q.order_by.unwrap();
        assert_eq!(ob.scoring_fn, "relevance_clicks");
        assert_eq!(ob.args, vec!["current_user"]);
        assert!(ob.descending);
        assert_eq!(q.limit.unwrap().count, 5);
    }

    #[test]
    fn parse_order_by_default_asc() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') ORDER BY score() LIMIT 10").unwrap();
        let ob = q.order_by.unwrap();
        assert_eq!(ob.scoring_fn, "score");
        assert!(ob.args.is_empty());
        assert!(!ob.descending);
    }

    #[test]
    fn parse_order_by_with_string_arg() {
        // Some scoring functions accept string literal arguments
        let q = parse_vql("FIND SIMILARITY(emb, 'q') ORDER BY similarity(emb, 'query')").unwrap();
        let ob = q.order_by.unwrap();
        assert_eq!(ob.scoring_fn, "similarity");
        assert_eq!(ob.args, vec!["emb", "query"]);
    }

    #[test]
    fn parse_order_by_asc_explicit() {
        let q = parse_vql("FIND SIMILARITY(emb, 'q') ORDER BY score() ASC").unwrap();
        let ob = q.order_by.unwrap();
        assert!(!ob.descending);
    }

    // -----------------------------------------------------------------------
    // Clause order independence — new in this spec
    // -----------------------------------------------------------------------

    #[test]
    fn parse_clauses_in_different_order() {
        // Standard order
        let q1 = parse_vql(
            "FIND SIMILARITY(emb, 'q') WITH METADATA WHERE year >= 2020 ORDER BY score() LIMIT 10 WITHIN 100ms",
        )
        .unwrap();
        // Reversed order
        let q2 = parse_vql(
            "FIND SIMILARITY(emb, 'q') WITHIN 100ms LIMIT 10 ORDER BY score() WITH METADATA WHERE year >= 2020",
        )
        .unwrap();

        assert_eq!(q1.find, q2.find);
        assert_eq!(q1.limit.unwrap().count, q2.limit.unwrap().count);
        assert_eq!(q1.within.unwrap().millis, q2.within.unwrap().millis);
        assert_eq!(q1.order_by.unwrap().scoring_fn, q2.order_by.unwrap().scoring_fn);
    }

    #[test]
    fn parse_all_new_clauses_together() {
        let q = parse_vql(
            "FIND SEMANTIC(emb, VECTOR(0.1, 0.2)) \
             WITH METADATA WHERE cuisine LIKE 'ita%' AND year >= 2020 \
             PROJECT ON year, category \
             BIAS recency() \
             LIMIT 20 OFFSET 5 \
             EN 200ms",
        )
        .unwrap();

        assert_eq!(q.find, "SEMANTIC");
        assert!(q.project_on.is_some());
        assert!(q.bias.is_some());
        assert!(q.metadata_where.is_some());
        assert_eq!(q.limit.unwrap().count, 20);
        assert_eq!(q.offset.unwrap().count, 5);
        assert_eq!(q.within.unwrap().millis, 200);
    }

    // -----------------------------------------------------------------------
    // Full queries — all clauses together
    // -----------------------------------------------------------------------

    #[test]
    fn parse_full_query() {
        let q = parse_vql(
            "FIND SIMILARITY(embedding, 'quantum computing') \
             WITH METADATA WHERE year BETWEEN 2020 AND 2025 \
             ORDER BY relevance_clicks(current_user) DESC \
             LIMIT 20 \
             WITHIN 200ms",
        )
        .unwrap();
        assert_eq!(q.find, "SIMILARITY");
        let sim = q.similarity.as_ref().unwrap();
        assert_eq!(sim.field, "embedding");
        assert_eq!(sim.query_text, "quantum computing");
        assert!(q.metadata_where.is_some());
        assert!(q.order_by.is_some());
        assert_eq!(q.limit.unwrap().count, 20);
        assert_eq!(q.within.unwrap().millis, 200);
    }

    // -----------------------------------------------------------------------
    // Rejection cases
    // -----------------------------------------------------------------------

    #[test]
    fn reject_malformed_operator() {
        let result = parse_vql("FIND SIMILARITY(emb, 'AI') WITH METADATA WHERE color =< 5");
        assert!(result.is_err());
    }

    #[test]
    fn reject_trailing_content() {
        let result = parse_vql("FIND SIMILARITY(emb, 'q') LIMIT 10 extra");
        assert!(result.is_err());
    }

    #[test]
    fn reject_empty_input() {
        let result = parse_vql("");
        assert!(result.is_err());
    }

    #[test]
    fn reject_only_find_keyword() {
        let result = parse_vql("FIND");
        assert!(result.is_err());
    }

    #[test]
    fn reject_similarity_without_field() {
        let result = parse_vql("FIND SIMILARITY(, 'text')");
        assert!(result.is_err());
    }

    #[test]
    fn reject_similarity_without_query() {
        let result = parse_vql("FIND SIMILARITY(field, )");
        assert!(result.is_err());
    }

    #[test]
    fn reject_duplicate_within() {
        let result = parse_vql("FIND SIMILARITY(emb, 'q') WITHIN 100ms EN 200ms");
        assert!(result.is_err());
    }
}
