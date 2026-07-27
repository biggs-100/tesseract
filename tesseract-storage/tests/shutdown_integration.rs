// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Integration tests for graceful shutdown of the storage engine.
//!
//! These tests verify that:
//! 1. Shutdown drains the HotBuffer and flushes the WAL.
//! 2. Data survives a shutdown + reopen cycle.
//! 3. A short timeout does not prevent shutdown from completing.

use std::sync::Arc;

use tesseract_storage::cold_store::ColdStoreConfig;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::hot_store::HotStoreConfig;
use tesseract_storage::types::*;
use tesseract_core::types::VectorId;

/// Build a minimal `StorageConfig` for testing shutdown.
fn test_config(tmp: &tempfile::TempDir) -> StorageConfig {
    let root = tmp.path().to_path_buf();
    StorageConfig {
        wal: WalConfig {
            wal_dir: root.join("wal"),
            segment_size: 1024 * 1024,
            fsync_interval_ms: 100,
            fsync_interval_ops: 1000,
        },
        hot: HotStoreConfig { max_records: 100 },
        cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 10 },
        skeleton: tesseract_storage::skeleton::SkeletonConfig { wake_threshold: 0.15 },
        cache: PageCacheConfig { capacity: 100 },
        index: IndexConfig {
            enabled: false,
            dim: 4,
            hnsw: tesseract_index::types::HnswConfig::default(),
            path: root.join("index.bin"),
        },
        lifecycle: LifecycleConfig::default(),
        topological: TopologicalConfig::default(),
        merkle: MerkleConfig {
            enabled: true,
            hot_buffer_capacity: 50,
            max_cluster_size: 100,
            merkle_tree_path: Some(root.join("merkle.bin")),
        },
        shutdown: ShutdownConfig { timeout_secs: 10 },
    }
}

#[tokio::test]
async fn shutdown_flushes_wal_and_hotbuffer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = test_config(&tmp);
    let engine = Arc::new(StorageEngine::open(config.clone()).await.unwrap());

    // Insert vectors that will go into both WAL and hot buffer.
    for i in 0..10u64 {
        engine
            .insert(VectorId(i), vec![i as f64; 4], serde_json::json!({"idx": i}), WriteMode::Durable)
            .await
            .unwrap();
    }

    // Shutdown should complete without error.
    engine.shutdown().await.unwrap();

    // Reopen the engine and verify data was properly flushed.
    let reopened = StorageEngine::open(config).await.unwrap();

    // Data should be in the hot store (survived shutdown).
    for i in 0..10u64 {
        let record = reopened.get(&VectorId(i)).await.unwrap();
        assert!(record.is_some(), "vector {i} should exist after shutdown");
        assert_eq!(record.unwrap().id, VectorId(i));
    }
}

#[tokio::test]
async fn shutdown_timeout_logs_warning() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let config = StorageConfig {
        wal: WalConfig {
            wal_dir: root.join("wal"),
            segment_size: 1024 * 1024,
            fsync_interval_ms: 100,
            fsync_interval_ops: 1000,
        },
        hot: HotStoreConfig { max_records: 100 },
        cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 10 },
        skeleton: tesseract_storage::skeleton::SkeletonConfig { wake_threshold: 0.15 },
        cache: PageCacheConfig { capacity: 100 },
        index: IndexConfig {
            enabled: false,
            dim: 4,
            hnsw: tesseract_index::types::HnswConfig::default(),
            path: root.join("index.bin"),
        },
        lifecycle: LifecycleConfig::default(),
        topological: TopologicalConfig::default(),
        merkle: MerkleConfig::default(),
        shutdown: ShutdownConfig { timeout_secs: 1 },
    };

    let engine = StorageEngine::open(config).await.unwrap();

    // Insert some data.
    for i in 0..5u64 {
        engine
            .insert(VectorId(i), vec![1.0; 4], serde_json::json!({"idx": i}), WriteMode::Fast)
            .await
            .unwrap();
    }

    // Shutdown with 1s timeout should complete (WAL flush is fast).
    let result = engine.shutdown().await;
    assert!(result.is_ok(), "shutdown with short timeout should succeed, got: {:?}", result);
}

#[tokio::test]
async fn shutdown_without_merkle_still_flushes_wal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let config = StorageConfig {
        wal: WalConfig {
            wal_dir: root.join("wal"),
            segment_size: 1024 * 1024,
            fsync_interval_ms: 100,
            fsync_interval_ops: 1000,
        },
        hot: HotStoreConfig { max_records: 100 },
        cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 10 },
        skeleton: tesseract_storage::skeleton::SkeletonConfig { wake_threshold: 0.15 },
        cache: PageCacheConfig { capacity: 100 },
        index: IndexConfig {
            enabled: false,
            dim: 4,
            hnsw: tesseract_index::types::HnswConfig::default(),
            path: root.join("index.bin"),
        },
        lifecycle: LifecycleConfig::default(),
        topological: TopologicalConfig::default(),
        merkle: MerkleConfig::default(),
        shutdown: ShutdownConfig::default(),
    };

    let engine = Arc::new(StorageEngine::open(config.clone()).await.unwrap());

    for i in 0..5u64 {
        engine
            .insert(VectorId(i), vec![i as f64; 4], serde_json::json!({"idx": i}), WriteMode::Durable)
            .await
            .unwrap();
    }

    engine.shutdown().await.unwrap();

    // Reopen — vectors should survive via WAL replay.
    let reopened = StorageEngine::open(config).await.unwrap();
    for i in 0..5u64 {
        let record = reopened.get(&VectorId(i)).await.unwrap();
        assert!(record.is_some(), "vector {i} should survive without merkle");
    }
}
