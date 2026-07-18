// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};

/// A typed identifier for a single vector entry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct VectorId(pub u64);

/// A Unix timestamp in milliseconds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Timestamp(pub i64);

/// A dynamically-typed metadata value.
///
/// # NaN Semantics
///
/// `MetadataValue::Float(f64)` uses standard `f64` `PartialEq` — `NaN != NaN`.
/// Comparison and hashing of `Float` values follow IEEE 754 rules.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum MetadataValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(i64),
    Array(Vec<MetadataValue>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_id_equality() {
        assert_eq!(VectorId(42), VectorId(42));
        assert_ne!(VectorId(42), VectorId(7));
    }

    #[test]
    fn timestamp_equality() {
        assert_eq!(Timestamp(1_700_000_000_000), Timestamp(1_700_000_000_000));
        assert_ne!(Timestamp(1_700_000_000_000), Timestamp(0));
    }

    #[test]
    fn metadata_value_construction() {
        let _s = MetadataValue::String("hello".into());
        let _i = MetadataValue::Integer(-7);
        let _f = MetadataValue::Float(1.5);
        let _b = MetadataValue::Boolean(true);
        let _d = MetadataValue::DateTime(1_700_000_000_000);
        let _a = MetadataValue::Array(vec![MetadataValue::Integer(1), MetadataValue::Integer(2)]);
    }

    #[test]
    fn metadata_value_nested_array() {
        let nested = MetadataValue::Array(vec![
            MetadataValue::String("outer".into()),
            MetadataValue::Array(vec![MetadataValue::Integer(1), MetadataValue::Integer(2)]),
        ]);
        assert_eq!(
            nested,
            MetadataValue::Array(vec![
                MetadataValue::String("outer".into()),
                MetadataValue::Array(vec![MetadataValue::Integer(1), MetadataValue::Integer(2),]),
            ])
        );
    }

    #[test]
    fn metadata_value_bincode_roundtrip() {
        let cases = vec![
            MetadataValue::String("tesseract".into()),
            MetadataValue::Integer(-99),
            MetadataValue::Float(2.5),
            MetadataValue::Boolean(false),
            MetadataValue::DateTime(1_700_000_000_000),
            MetadataValue::Array(vec![MetadataValue::Float(1.0), MetadataValue::Float(2.0)]),
        ];

        for value in cases {
            let encoded = bincode::serialize(&value).unwrap();
            let decoded: MetadataValue = bincode::deserialize(&encoded).unwrap();
            assert_eq!(value, decoded);
        }
    }
}
