// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use thiserror::Error;

/// Unified error type for all tesseract crates.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Dimension mismatch: self has {0} dimensions, other has {1}")]
    DimensionMismatch(usize, usize),

    #[error("Index {0} out of bounds for vector of length {1}")]
    IndexOutOfBounds(usize, usize),

    #[error("Parse error at line {line}, column {col}: {message}")]
    ParseError { line: usize, col: usize, message: String },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Corrupt WAL at segment {segment}: CRC mismatch at offset {offset}")]
    CorruptWal { segment: u64, offset: u64 },

    #[error("CRC mismatch: expected {expected:#x}, got {actual:#x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("Payload truncated: expected {expected} bytes, got {actual}")]
    PayloadTruncated { expected: usize, actual: usize },

    #[error("Invalid vector: {0}")]
    InvalidVector(String),

    #[error("Invalid config: {0}")]
    InvalidConfig(String),

    #[error("Lock poisoned: {0}")]
    LockPoisoned(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("JSON error: {0}")]
    JsonError(String),

    #[error("{0} already exists")]
    AlreadyExists(String),

    #[error("{0} not found")]
    NotFound(String),

    #[error("Index not built")]
    IndexNotBuilt,

    #[error("Unsupported dimension: {0}")]
    UnsupportedDimension(usize),

    #[error("Graph corrupt: {0}")]
    GraphCorrupt(String),

    #[error("Shard {0} is not assigned to any node")]
    ShardNotAssigned(u64),

    #[error("All shards failed during distributed query")]
    AllShardsFailed,

    #[error("Node conflict: {0}")]
    NodeConflict(String),

    #[error("{0}")]
    ServiceError(String),
}

impl From<bincode::Error> for Error {
    fn from(e: bincode::Error) -> Self {
        Error::SerializationError(e.to_string())
    }
}

/// Convenience alias for tesseract crate operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_mismatch_display() {
        let err = Error::DimensionMismatch(3, 5);
        assert_eq!(err.to_string(), "Dimension mismatch: self has 3 dimensions, other has 5");
    }

    #[test]
    fn index_out_of_bounds_display() {
        let err = Error::IndexOutOfBounds(7, 5);
        assert_eq!(err.to_string(), "Index 7 out of bounds for vector of length 5");
    }

    #[test]
    fn parse_error_display() {
        let err = Error::ParseError { line: 3, col: 14, message: "expected '('".into() };
        assert_eq!(err.to_string(), "Parse error at line 3, column 14: expected '('");
    }

    #[test]
    fn corrupt_wal_display() {
        let err = Error::CorruptWal { segment: 5, offset: 1024 };
        assert_eq!(err.to_string(), "Corrupt WAL at segment 5: CRC mismatch at offset 1024");
    }

    #[test]
    fn crc_mismatch_display() {
        let err = Error::CrcMismatch { expected: 0xDEAD, actual: 0xBEEF };
        assert_eq!(err.to_string(), "CRC mismatch: expected 0xdead, got 0xbeef");
    }

    #[test]
    fn payload_truncated_display() {
        let err = Error::PayloadTruncated { expected: 100, actual: 42 };
        assert_eq!(err.to_string(), "Payload truncated: expected 100 bytes, got 42");
    }

    #[test]
    fn io_error_from_std() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = Error::from(io_err);
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn shard_not_assigned_display() {
        let err = Error::ShardNotAssigned(7);
        assert_eq!(err.to_string(), "Shard 7 is not assigned to any node");
    }

    #[test]
    fn all_shards_failed_display() {
        let err = Error::AllShardsFailed;
        assert_eq!(err.to_string(), "All shards failed during distributed query");
    }

    #[test]
    fn node_conflict_display() {
        let err = Error::NodeConflict("node-a already registered".into());
        assert_eq!(err.to_string(), "Node conflict: node-a already registered");
    }

    #[test]
    fn invalid_vector_display() {
        let err = Error::InvalidVector("vector must be finite and non-zero".into());
        assert_eq!(err.to_string(), "Invalid vector: vector must be finite and non-zero");
    }

    #[test]
    fn invalid_config_display() {
        let err = Error::InvalidConfig("bucket boundaries must not be empty".into());
        assert_eq!(err.to_string(), "Invalid config: bucket boundaries must not be empty");
    }

    #[test]
    fn lock_poisoned_display() {
        let err = Error::LockPoisoned("engine mutex".into());
        assert_eq!(err.to_string(), "Lock poisoned: engine mutex");
    }

    #[test]
    fn serialization_error_display() {
        let err = Error::SerializationError("bincode error".into());
        assert_eq!(err.to_string(), "Serialization error: bincode error");
    }

    #[test]
    fn json_error_display() {
        let err = Error::JsonError("invalid JSON".into());
        assert_eq!(err.to_string(), "JSON error: invalid JSON");
    }
}
