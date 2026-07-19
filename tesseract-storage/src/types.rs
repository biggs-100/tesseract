// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Re-export sub-config types for StorageConfig composition.
pub use crate::cold_store::ColdStoreConfig;
pub use crate::hot_store::HotStoreConfig;
pub use crate::skeleton::SkeletonConfig;

/// Unique identifier for a WAL segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub u64);

/// Unique transaction identifier (monotonic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct TransactionId(pub u64);

/// Checkpoint recording the last fully-flushed transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub last_flushed_txn_id: TransactionId,
    pub segment_id: SegmentId,
}

/// Write mode for consistency guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Ack after WAL fsync. Maximum durability.
    Durable,
    /// Ack after memory write. WAL flush is async.
    Fast,
}

/// WAL configuration.
#[derive(Debug, Clone)]
pub struct WalConfig {
    pub wal_dir: std::path::PathBuf,
    pub segment_size: u64,       // default 64 * 1024 * 1024
    pub fsync_interval_ms: u64,  // default 100
    pub fsync_interval_ops: u64, // default 1000
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            wal_dir: std::path::PathBuf::from("wal"),
            segment_size: 64 * 1024 * 1024,
            fsync_interval_ms: 100,
            fsync_interval_ops: 1000,
        }
    }
}

/// OpCode constants for WAL entry types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpCode {
    InsertVector = 0x01,
    DeleteVector = 0x02,
    UpdateMetadata = 0x03,
    IndexInsert = 0x10,
    IndexDelete = 0x11,
}

impl TryFrom<u8> for OpCode {
    type Error = tesseract_common::error::Error;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x01 => Ok(Self::InsertVector),
            0x02 => Ok(Self::DeleteVector),
            0x03 => Ok(Self::UpdateMetadata),
            0x10 => Ok(Self::IndexInsert),
            0x11 => Ok(Self::IndexDelete),
            other => Err(tesseract_common::error::Error::BincodeError(format!("unknown opcode: {other}"))),
        }
    }
}

/// Page cache configuration.
#[derive(Debug, Clone)]
pub struct PageCacheConfig {
    /// Number of pages the cache can hold.
    pub capacity: usize,
}

impl Default for PageCacheConfig {
    fn default() -> Self {
        Self { capacity: 100 }
    }
}

/// Lifecycle configuration for tier promotion/demotion.
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// How often to run the promotion cycle, in seconds.
    pub promote_interval_secs: u64,
    /// How often to run the demotion cycle, in seconds.
    pub demote_interval_secs: u64,
    /// Maximum records in the hot tier before demotion is triggered.
    pub hot_max_records: usize,
    /// Minimum accesses before a cold partition is promoted.
    pub cold_min_access: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self { promote_interval_secs: 60, demote_interval_secs: 300, hot_max_records: 100_000, cold_min_access: 5 }
    }
}

/// Index configuration for the storage engine.
///
/// Controls whether an ANN index is built alongside writes,
/// enabling approximate nearest-neighbour search.
#[derive(Debug, Clone)]
pub struct IndexConfig {
    /// Whether the index is enabled on open.
    pub enabled: bool,
    /// Dimensionality of indexed vectors.
    pub dim: usize,
    /// HNSW topology parameters.
    pub hnsw: tesseract_index::types::HnswConfig,
    /// File path for persisting the serialised index.
    pub path: std::path::PathBuf,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dim: 384,
            hnsw: tesseract_index::types::HnswConfig::default(),
            path: std::path::PathBuf::from("index.hnsw"),
        }
    }
}

/// Topological bias configuration.
///
/// Controls whether query-time vector biasing is enabled and which
/// metadata fields are tracked for centroid and correlation statistics.
#[derive(Debug, Clone, Default)]
pub struct TopologicalConfig {
    /// Whether topological biasing is enabled.
    pub enabled: bool,
    /// Metadata fields whose string values are tracked as categorical
    /// centroids (e.g. `["category", "genre"]`).
    pub categorical_fields: Vec<String>,
    /// Metadata fields whose numeric values are tracked for dimension-wise
    /// correlation (e.g. `["year", "price"]`).
    pub numerical_fields: Vec<String>,
    /// Bucket boundaries for numerical fields tracked with bucketized
    /// centroids (supersedes correlation-based bias for these fields).
    /// Map of field_name → sorted bucket boundaries.
    /// E.g., `{ "year": vec![2015.0, 2018.0, 2021.0, 2024.0] }`
    /// creates 4 buckets: <2018, 2018-2021, 2021-2024, >=2024.
    pub numerical_buckets: HashMap<String, Vec<f64>>,
}

/// Merkle tree / hot buffer configuration.
#[derive(Debug, Clone)]
pub struct MerkleConfig {
    /// Whether the progressive Merkle tree is enabled.
    pub enabled: bool,
    /// Maximum vectors in the hot buffer before merge is triggered.
    pub hot_buffer_capacity: usize,
    /// Maximum vectors per cluster before splitting.
    pub max_cluster_size: usize,
    /// Optional path for persisting the Merkle tree to disk.
    pub merkle_tree_path: Option<PathBuf>,
}

impl Default for MerkleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hot_buffer_capacity: 10_000,
            max_cluster_size: 1_000,
            merkle_tree_path: None,
        }
    }
}

/// Top-level storage engine configuration.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub wal: WalConfig,
    pub hot: HotStoreConfig,
    pub cold: ColdStoreConfig,
    pub cache: PageCacheConfig,
    pub skeleton: SkeletonConfig,
    pub lifecycle: LifecycleConfig,
    pub index: IndexConfig,
    pub topological: TopologicalConfig,
    pub merkle: MerkleConfig,
}
