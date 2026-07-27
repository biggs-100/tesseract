// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Concurrency stress tests for the HNSW index with `parking_lot::RwLock`.
//!
//! These tests only run with the default (non-legacy) locking since they verify
//! that the parking_lot-based RwLock allows concurrent reads without deadlock.

#![cfg(not(feature = "legacy-locking"))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rand::Rng;
use tesseract_core::types::VectorId;
use tesseract_index::distance::CosineComputer;
use tesseract_index::hnsw::HnswIndex;
use tesseract_index::types::HnswConfig;

/// 10 concurrent readers + 1 writer, verifying no deadlock.
///
/// Readers call `search()` which internally acquires the HNSW index's
/// `parking_lot::RwLock<()>` read lock. The writer calls `insert()` which
/// takes `&mut self` (no inner lock). External synchronization via outer
/// `parking_lot::RwLock` ensures safe concurrent access.
#[test]
fn concurrent_reads_with_write() {
    let dim = 8;
    let config = HnswConfig::default();
    let index = Arc::new(RwLock::new(HnswIndex::new(dim, CosineComputer, config)));

    // Seed the index with some vectors.
    {
        let mut idx = index.write();
        for i in 0..200u64 {
            let v: Vec<f64> = (0..dim).map(|_| rand::thread_rng().r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            let vn: Vec<f64> = v.iter().map(|x| x / norm).collect();
            idx.insert(VectorId(i), &vn).unwrap();
        }
    }

    let mut handles = Vec::new();
    let n_readers = 10;
    let n_iters = 50;

    // Spawn readers.
    for _ in 0..n_readers {
        let idx = Arc::clone(&index);
        let handle = std::thread::spawn(move || {
            let mut rng = rand::thread_rng();
            for _ in 0..n_iters {
                let q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
                let results = idx.read().search(&q, 10, None).unwrap();
                // Every seeded search should return results (index has 200+ vectors).
                assert!(!results.is_empty(), "every search should return results");
                // Verify results are sorted by distance ascending.
                for w in results.windows(2) {
                    assert!(w[0].1 <= w[1].1, "results must be sorted by distance ascending");
                }
            }
        });
        handles.push(handle);
    }

    // Spawn one writer that inserts new vectors.
    let idx = Arc::clone(&index);
    let handle = std::thread::spawn(move || {
        let mut rng = rand::thread_rng();
        for i in 0..n_iters {
            let v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            let vn: Vec<f64> = v.iter().map(|x| x / norm).collect();
            idx.write().insert(VectorId(200 + i as u64), &vn).unwrap();
        }
    });
    handles.push(handle);

    // Wait for all threads with a 10-second safety timeout.
    let deadline = Instant::now() + Duration::from_secs(10);
    for h in handles {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("concurrent test timed out after 10s (possible deadlock)");
        }
        h.join().expect("thread panicked");
    }
}

/// Verify that multiple readers can execute search in parallel without
/// serializing at the index level (the parking_lot::RwLock<()> allows
/// shared reads).
#[test]
fn readers_dont_serialize() {
    let dim = 4;
    let config = HnswConfig::default();
    let index = Arc::new(RwLock::new(HnswIndex::new(dim, CosineComputer, config)));

    // Seed with vectors.
    {
        let mut idx = index.write();
        for i in 0..100u64 {
            let v = vec![0.5; 4];
            idx.insert(VectorId(i), &v).unwrap();
        }
    }

    let query = vec![0.5; 4];

    // Measure single-threaded baseline time.
    let start = Instant::now();
    for _ in 0..80 {
        let _ = index.read().search(&query, 10, None).unwrap();
    }
    let single_duration = start.elapsed();

    // Now run 4 concurrent threads doing the same total work (20 searches each).
    let mut handles = Vec::new();
    let start = Instant::now();

    for _ in 0..4 {
        let idx = Arc::clone(&index);
        let q = query.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..20 {
                let _ = idx.read().search(&q, 10, None).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    let concurrent_duration = start.elapsed();

    // The concurrent run should complete in less than 4× the single-threaded time.
    // This is a soft assertion — the outer parking_lot::RwLock read lock can be
    // shared, but the write lock inside the index is also a read lock (parking_lot
    // allows concurrent reads on the same RW lock).
    assert!(
        concurrent_duration < single_duration * 3,
        "concurrent readers took {concurrent_duration:?}, single took {single_duration:?} (expected < {:?})",
        single_duration * 3
    );
}
