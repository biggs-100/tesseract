// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Integration tests for the StorageEngine ANN index.
//!
//! Covers: insert-then-search, weighted search, persistence across
//! engine restart, and error handling when the index is disabled.

use tesseract_core::types::VectorId;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;

/// Helper: build a storage config with the index enabled and 4-d vectors.
///
/// Uses Euclidean distance so that zero-vector queries have well-defined
/// nearest neighbours (cosine distance from any vector to the zero vector
/// is always 1.0, which makes ordering arbitrary).
fn make_config(dir: &tempfile::TempDir) -> StorageConfig {
    StorageConfig {
        wal: WalConfig {
            wal_dir: dir.path().join("wal"),
            segment_size: 64 * 1024 * 1024,
            fsync_interval_ms: 1000,
            fsync_interval_ops: 10000,
        },
        hot: HotStoreConfig { max_records: 10000 },
        cold: ColdStoreConfig { data_dir: dir.path().join("cold"), zstd_level: 3, max_rows_per_file: 10000 },
        cache: PageCacheConfig { capacity: 100 },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        lifecycle: LifecycleConfig {
            promote_interval_secs: 3600,
            demote_interval_secs: 3600,
            hot_max_records: 10000,
            cold_min_access: 5,
        },
        index: IndexConfig {
            enabled: true,
            dim: 4,
            hnsw: tesseract_index::types::HnswConfig {
                distance_metric: tesseract_index::types::DistanceMetric::Euclidean,
                ..tesseract_index::types::HnswConfig::default()
            },
            path: dir.path().join("index.hnsw"),
        },
        topological: Default::default(),
        merkle: Default::default(),
        shutdown: ShutdownConfig::default(),
    }
}

// ── 1. Insert then search ──────────────────────────────────────────────

#[tokio::test]
async fn test_engine_search_after_insert() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = make_config(&dir);
    let engine = StorageEngine::open(config).await.unwrap();

    // Insert 100 4-d vectors where each component increases with the
    // index.  VectorId(0) ≈ (0, 0, 0, 0) is closest to the origin.
    for i in 0..100u64 {
        let v = vec![(i as f64) / 100.0, ((i * 2) as f64) / 100.0, ((i * 3) as f64) / 100.0, ((i * 4) as f64) / 100.0];
        engine.insert(VectorId(i), v, serde_json::json!({"idx": i}), WriteMode::Fast).await.unwrap();
    }

    // Search for the nearest vectors to the origin.
    let query = vec![0.0, 0.0, 0.0, 0.0];
    let results = engine.search(&query, 5, None).await.unwrap();
    assert!(!results.is_empty(), "Search should return results");
    assert_eq!(results[0].0, VectorId(0), "Closest to zero should be VectorId(0)");

    engine.shutdown().await.unwrap();
}

// ── 2. Weighted search ─────────────────────────────────────────────────

#[tokio::test]
async fn test_engine_weighted_search() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = make_config(&dir);
    let engine = StorageEngine::open(config).await.unwrap();

    // Group A (0..24): dim0 = 1.0, rest 0.0
    // Group B (25..49): dim0 = 0.0, rest 0.0
    for i in 0..50u64 {
        let v = vec![if i < 25 { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0];
        engine
            .insert(VectorId(i), v, serde_json::json!({"group": if i < 25 { "A" } else { "B" }}), WriteMode::Fast)
            .await
            .unwrap();
    }

    let query = vec![1.0, 0.0, 0.0, 0.0];

    // Unweighted: group A has distance 0 to query (exact match on dim0),
    // group B has distance 1.  Only group A vectors are returned.
    let unweighted = engine.search(&query, 10, None).await.unwrap();
    assert_eq!(unweighted.len(), 10);

    // Weighted: zero out dim0; all vectors become (0,0,0,0), distance
    // to query is 1.0 for every candidate (only dim0 differs).
    let mask = tesseract_core::projection::WeightMask(vec![(0, 0.0)]);
    let weighted = engine.search(&query, 10, Some(&mask)).await.unwrap();
    assert!(!weighted.is_empty());

    engine.shutdown().await.unwrap();
}

// ── 3. Persistence across restart ──────────────────────────────────────

#[tokio::test]
async fn test_engine_search_persistence() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = make_config(&dir);

    // First session: insert 20 vectors with Durable mode.
    {
        let engine = StorageEngine::open(config.clone()).await.unwrap();
        for i in 0..20u64 {
            let v = vec![(i as f64) / 20.0; 4];
            engine.insert(VectorId(i), v, serde_json::json!({"i": i}), WriteMode::Durable).await.unwrap();
        }
        engine.shutdown().await.unwrap();
    }

    // Second session: reopen and verify search still works.
    {
        let engine = StorageEngine::open(config).await.unwrap();
        let query = vec![0.0; 4];
        let results = engine.search(&query, 5, None).await.unwrap();
        assert!(!results.is_empty(), "Search should work after restart");
        engine.shutdown().await.unwrap();
    }
}

// ── 4. Search with index disabled returns IndexNotBuilt ───────────────

#[tokio::test]
async fn test_engine_search_disabled_index_returns_error() {
    let dir = tempfile::TempDir::new().unwrap();
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
        shutdown: ShutdownConfig::default(),
    };

    let engine = StorageEngine::open(config).await.unwrap();
    let err = engine.search(&[0.0; 4], 5, None).await.unwrap_err();
    assert!(matches!(err, tesseract_common::error::Error::IndexNotBuilt), "Expected IndexNotBuilt, got: {err}",);
    engine.shutdown().await.unwrap();
}
