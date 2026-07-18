// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use nom_locate::LocatedSpan;
use tesseract_common::error::{Error, Result};

use crate::ast::Query;

/// A located span type used internally by the parser.
pub type Span<'a> = LocatedSpan<&'a str>;

/// Parses a VQL query string into its AST representation.
///
/// # Errors
///
/// Returns `Error::ParseError` with line, column, and a diagnostic message
/// when the input cannot be parsed as a valid VQL query.
///
/// # Examples
///
/// ```
/// use tesseract_vql::parser::parse;
///
/// let query = parse("FIND SIMILARITY(emb, 'hello world')").unwrap();
/// assert_eq!(query.find, "SIMILARITY");
/// ```
pub fn parse(input: &str) -> Result<Query> {
    let span = Span::new(input);
    match crate::grammar::query(span) {
        Ok((_, query)) => Ok(query),
        Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
            let line = e.input.location_line();
            let col = e.input.get_column();
            let msg = format!("{:?}", e);
            Err(Error::ParseError { line: line as usize, col, message: msg })
        }
        Err(nom::Err::Incomplete(_)) => {
            Err(Error::ParseError { line: 0, col: 0, message: "Incomplete input".to_string() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let query = parse("FIND SIMILARITY(embedding, 'quantum computing')").unwrap();
        assert_eq!(query.find, "SIMILARITY");
        let sim = query.similarity.unwrap();
        assert_eq!(sim.field, "embedding");
        assert_eq!(sim.query_text, "quantum computing");
    }

    #[test]
    fn parse_with_metadata_where() {
        let query = parse(
            "FIND SIMILARITY(embedding, 'machine learning') \
             WITH METADATA WHERE category = 'science'",
        )
        .unwrap();
        assert!(query.metadata_where.is_some());
    }

    #[test]
    fn parse_with_limit() {
        let query = parse("FIND SIMILARITY(emb, 'test') LIMIT 42").unwrap();
        assert_eq!(query.limit.unwrap().count, 42);
    }

    #[test]
    fn parse_with_within() {
        let query = parse("FIND SIMILARITY(emb, 'test') WITHIN 150ms").unwrap();
        assert_eq!(query.within.unwrap().millis, 150);
    }

    #[test]
    fn parse_full_query_all_clauses() {
        let query = parse(
            "FIND SIMILARITY(embedding, 'climate') \
             WITH METADATA WHERE year BETWEEN 2020 AND 2025 \
             LIMIT 20 \
             WITHIN 200ms",
        )
        .unwrap();
        assert_eq!(query.find, "SIMILARITY");
        assert!(query.metadata_where.is_some());
        assert!(query.order_by.is_none());
        assert_eq!(query.limit.unwrap().count, 20);
        assert_eq!(query.within.unwrap().millis, 200);
    }

    #[test]
    fn parse_order_by_desc() {
        let query = parse(
            "FIND SIMILARITY(emb, 'history') \
             ORDER BY relevance_clicks(current_user) DESC \
             LIMIT 5 \
             WITHIN 150ms",
        )
        .unwrap();
        let ob = query.order_by.unwrap();
        assert_eq!(ob.scoring_fn, "relevance_clicks");
        assert!(ob.descending);
        assert_eq!(query.limit.unwrap().count, 5);
        assert_eq!(query.within.unwrap().millis, 150);
    }

    #[test]
    fn parse_where_and_in() {
        let query = parse(
            "FIND SIMILARITY(emb, 'AI') \
             WITH METADATA WHERE date >= 2024 AND category IN ('tech', 'science') \
             LIMIT 10",
        )
        .unwrap();
        let mw = query.metadata_where.unwrap();
        assert_eq!(mw.predicates.len(), 1);
        match &mw.predicates[0] {
            crate::ast::Predicate::And(preds) => assert_eq!(preds.len(), 2),
            other => panic!("Expected And, got {:?}", other),
        }
    }

    #[test]
    fn parse_between() {
        let query = parse(
            "FIND SIMILARITY(emb, 'climate') \
             WITH METADATA WHERE year BETWEEN 2020 AND 2025 \
             LIMIT 20 \
             WITHIN 200ms",
        )
        .unwrap();
        let mw = query.metadata_where.unwrap();
        match &mw.predicates[0] {
            crate::ast::Predicate::Between { field, low, high } => {
                assert_eq!(field, "year");
                assert_eq!(*low, crate::ast::Literal::Integer(2020));
                assert_eq!(*high, crate::ast::Literal::Integer(2025));
            }
            other => panic!("Expected Between, got {:?}", other),
        }
    }

    #[test]
    fn reject_no_find() {
        let err = parse("SIMILARITY(emb, 'text')");
        assert!(err.is_err());
        if let Err(Error::ParseError { line, col, message }) = err {
            assert!(line > 0);
            assert!(col > 0);
            assert!(!message.is_empty());
        } else {
            panic!("Expected ParseError");
        }
    }

    #[test]
    fn reject_missing_closing_paren() {
        let err = parse("FIND SIMILARITY(emb, 'hello'");
        assert!(err.is_err());
        if let Err(Error::ParseError { line, col, message }) = err {
            // Should have meaningful position info
            assert!(line > 0 || col > 0);
            assert!(!message.is_empty());
        } else {
            panic!("Expected ParseError");
        }
    }
}
