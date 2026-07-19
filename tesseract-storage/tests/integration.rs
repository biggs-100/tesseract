// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Integration tests for the full StorageEngine lifecycle.

use tesseract_core::types::VectorId;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;

#[tokio::test]
async fn test_engine_insert_and_get() {
    let dir = tempfile::tempdir().unwrap();
    let config = StorageConfig {
        wal: WalConfig {
            wal_dir: dir.path().join("wal"),
            segment_size: 64 * 1024 * 1024,
            fsync_interval_ms: 100,
            fsync_interval_ops: 1000,
        },
        hot: HotStoreConfig { max_records: 1000 },
        cold: ColdStoreConfig { data_dir: dir.path().join("cold"), zstd_level: 3, max_rows_per_file: 10000 },
        cache: PageCacheConfig { capacity: 100 },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        lifecycle: LifecycleConfig {
            promote_interval_secs: 3600,
            demote_interval_secs: 3600,
            hot_max_records: 1000,
            cold_min_access: 5,
        },
        index: IndexConfig { enabled: false, ..Default::default() },
        topological: Default::default(),
        merkle: Default::default(),
    };

    let engine = StorageEngine::open(config).await.unwrap();
    let id = VectorId(42);
    let vector = vec![1.0, 2.0, 3.0];
    let metadata = serde_json::json!({"category": "test"});

    engine.insert(id.clone(), vector.clone(), metadata.clone(), WriteMode::Durable).await.unwrap();

    let result = engine.get(&id).await.unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().id, id);

    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_engine_durable_vs_fast() {
    let dir = tempfile::tempdir().unwrap();
    let config = StorageConfig {
        wal: WalConfig { wal_dir: dir.path().join("wal"), ..Default::default() },
        hot: HotStoreConfig { max_records: 1000 },
        cold: ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() },
        cache: PageCacheConfig { capacity: 100 },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        lifecycle: LifecycleConfig {
            promote_interval_secs: 3600,
            demote_interval_secs: 3600,
            hot_max_records: 1000,
            cold_min_access: 5,
        },
        index: IndexConfig { enabled: false, ..Default::default() },
        topological: Default::default(),
        merkle: Default::default(),
    };

    let engine = StorageEngine::open(config).await.unwrap();

    // Insert 10 records in durable mode.
    for i in 0..10u64 {
        engine.insert(VectorId(i), vec![i as f64; 3], serde_json::json!({"i": i}), WriteMode::Durable).await.unwrap();
    }

    // Insert 10 records in fast mode.
    for i in 10..20u64 {
        engine.insert(VectorId(i), vec![i as f64; 3], serde_json::json!({"i": i}), WriteMode::Fast).await.unwrap();
    }

    // Verify all 20 records are retrievable.
    for i in 0..20u64 {
        let result = engine.get(&VectorId(i)).await.unwrap();
        assert!(result.is_some(), "Record {} should exist", i);
    }

    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_engine_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("wal");
    let cold_path = dir.path().join("cold");

    let make_config = || StorageConfig {
        wal: WalConfig { wal_dir: wal_path.clone(), ..Default::default() },
        hot: HotStoreConfig { max_records: 1000 },
        cold: ColdStoreConfig { data_dir: cold_path.clone(), ..Default::default() },
        cache: PageCacheConfig { capacity: 100 },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        lifecycle: LifecycleConfig {
            promote_interval_secs: 3600,
            demote_interval_secs: 3600,
            hot_max_records: 1000,
            cold_min_access: 5,
        },
        index: IndexConfig { enabled: false, ..Default::default() },
        topological: Default::default(),
        merkle: Default::default(),
    };

    // First session.
    {
        let engine = StorageEngine::open(make_config()).await.unwrap();
        engine.insert(VectorId(1), vec![1.0; 3], serde_json::json!({"k": "v"}), WriteMode::Durable).await.unwrap();
        engine.shutdown().await.unwrap();
    }

    // Second session (simulate restart).
    {
        let engine = StorageEngine::open(make_config()).await.unwrap();
        let result = engine.get(&VectorId(1)).await.unwrap();
        assert!(result.is_some(), "Record should survive restart");
        assert_eq!(result.unwrap().id, VectorId(1));
        engine.shutdown().await.unwrap();
    }
}
