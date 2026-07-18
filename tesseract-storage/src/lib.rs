// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Storage layer for Tesseract: WAL, hot tier, cold tier, skeleton, page cache.

pub mod cold_store;
pub mod engine;
pub mod hot_store;
pub mod lifecycle;
pub mod page_cache;
pub mod skeleton;
pub mod types;
pub mod wal;

pub use cold_store::{ColdStore, ColdStoreConfig, PartitionId, PartitionMeta};
pub use engine::StorageEngine;
pub use hot_store::{HotStore, HotStoreConfig, VectorRecord};
pub use lifecycle::TierLifecycle;
pub use page_cache::{Page, PageCache, PageKey};
pub use skeleton::{Centroid, SkeletonConfig, VectorSkeleton};
pub use types::*;
pub use wal::{WalEntry, WriteAheadLog};
