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

/// Parses any literal value: string, boolean, float, or integer (in that order).
fn literal(input: Span) -> IResult<Span, Literal> {
    alt((
        map(string_literal, Literal::String),
        map(boolean_literal, Literal::Boolean),
        map(float_literal, Literal::Float),
        map(integer_literal, Literal::Integer),
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

/// Parses one or more predicates joined by AND. A single predicate is returned
/// as-is; multiple predicates are wrapped in `Predicate::And`.
fn and_expression(input: Span) -> IResult<Span, Predicate> {
    let (input, predicates) = separated_list1(
        tuple((ws, tag("AND"), ws)),
        alt((in_predicate, between_predicate, comparison_predicate)),
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

/// Parses `SIMILARITY(field, 'query_text')`.
fn similarity_expr(input: Span) -> IResult<Span, SimilarityExpr> {
    let (input, _) = tag("SIMILARITY")(input)?;
    let (input, _) = tag("(")(input)?;
    let (input, _) = ws(input)?;
    let (input, field) = identifier(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag(",")(input)?;
    let (input, _) = ws(input)?;
    let (input, query_text) = string_literal(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag(")")(input)?;
    Ok((input, SimilarityExpr { field, query_text, vector: None }))
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

/// Parses `ORDER BY <scoring_fn>(<args>) [DESC]`.
fn order_by_clause(input: Span) -> IResult<Span, OrderBy> {
    let (input, _) = tag("ORDER")(input)?;
    let (input, _) = ws(input)?;
    let (input, _) = tag("BY")(input)?;
    let (input, _) = ws(input)?;
    let (input, scoring_fn) = identifier(input)?;
    let (input, args) = delimited(tag("("), separated_list0(tuple((ws, tag(","), ws)), order_by_arg), tag(")"))(input)?;
    let (input, descending) = opt(preceded(ws, tag("DESC")))(input)?;
    Ok((input, OrderBy { scoring_fn, args, descending: descending.is_some() }))
}

/// Parses `LIMIT <count>`.
fn limit_clause(input: Span) -> IResult<Span, Limit> {
    let (input, _) = tag("LIMIT")(input)?;
    let (input, _) = ws(input)?;
    let (input, count) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    Ok((input, Limit { count }))
}

/// Parses `WITHIN <millis>ms`.
fn within_clause(input: Span) -> IResult<Span, Within> {
    let (input, _) = tag("WITHIN")(input)?;
    let (input, _) = ws(input)?;
    let (input, millis) = map_res(recognize(digit1), |s: Span| s.fragment().parse::<u64>())(input)?;
    let (input, _) = tag("ms")(input)?;
    Ok((input, Within { millis }))
}

// ---------------------------------------------------------------------------
// Top-level query parser
// ---------------------------------------------------------------------------

/// Parses a complete VQL query. Expects `FIND SIMILARITY(...)` followed by
/// zero or more optional clauses, with no trailing content.
pub fn query(input: Span) -> IResult<Span, Query> {
    let (input, _) = tag("FIND")(input)?;
    let (input, _) = ws(input)?;
    let (input, similarity) = similarity_expr(input)?;
    let (input, metadata_where) = opt(preceded(ws, metadata_where_clause))(input)?;
    let (input, order_by) = opt(preceded(ws, order_by_clause))(input)?;
    let (input, limit) = opt(preceded(ws, limit_clause))(input)?;
    let (input, within) = opt(preceded(ws, within_clause))(input)?;
    let (input, _) = ws(input)?;

    // Reject trailing content after all clauses are consumed
    if !input.fragment().is_empty() {
        return Err(nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Eof)));
    }

    Ok((
        input,
        Query { find: "SIMILARITY".to_string(), similarity: Some(similarity), metadata_where, order_by, limit, within },
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
        assert!(q.order_by.is_none());
        assert!(q.limit.is_none());
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
}
