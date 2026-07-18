// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Criterion benchmark suite for the HNSW index.
//!
//! Measures:
//! - Search latency (unweighted)
//! - Build time
//! - Weighted search latency
//!
//! Run with: `cargo bench --bench hnsw_bench`

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rand::Rng;
use tesseract_core::projection::WeightMask;
use tesseract_index::distance::CosineComputer;
use tesseract_index::hnsw::HnswIndex;
use tesseract_index::types::HnswConfig;

const DIM: usize = 128;
const N_VECTORS: usize = 1000;
const N_QUERIES: usize = 100;
const EF_SEARCH: usize = 50;

/// Generate `n` L2-normalized random vectors of dimension `dim`.
fn generate_random_vectors(n: usize, dim: usize) -> Vec<Vec<f64>> {
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            v
        })
        .collect()
}

/// Benchmark search latency against synthetic data.
fn bench_hnsw_search(c: &mut Criterion) {
    let vectors = generate_random_vectors(N_VECTORS, DIM);
    let queries = generate_random_vectors(N_QUERIES, DIM);

    let config = HnswConfig::default();
    let mut index = HnswIndex::<CosineComputer>::new(DIM, CosineComputer, config);

    for (i, v) in vectors.iter().enumerate() {
        index.insert(tesseract_core::types::VectorId(i as u64), v).unwrap();
    }

    let mut group = c.benchmark_group("hnsw_search");
    group.bench_with_input(BenchmarkId::new("recall", format!("{}_dim{}", N_VECTORS, DIM)), &queries, |b, qs| {
        b.iter(|| {
            for q in qs {
                let results = index.search(black_box(q), EF_SEARCH, None).unwrap();
                black_box(results);
            }
        });
    });
    group.finish();
}

/// Benchmark index build time.
fn bench_hnsw_build(c: &mut Criterion) {
    let vectors = generate_random_vectors(N_VECTORS, DIM);

    c.bench_function("hnsw_build", |b| {
        b.iter(|| {
            let mut index = HnswIndex::<CosineComputer>::new(DIM, CosineComputer, HnswConfig::default());
            for (i, v) in vectors.iter().enumerate() {
                index.insert(black_box(tesseract_core::types::VectorId(i as u64)), black_box(v)).unwrap();
            }
        });
    });
}

/// Benchmark weighted search latency.
fn bench_hnsw_weighted_search(c: &mut Criterion) {
    let vectors = generate_random_vectors(N_VECTORS, DIM);
    let queries = generate_random_vectors(N_QUERIES, DIM);

    let config = HnswConfig::default();
    let mut index = HnswIndex::<CosineComputer>::new(DIM, CosineComputer, config);

    for (i, v) in vectors.iter().enumerate() {
        index.insert(tesseract_core::types::VectorId(i as u64), v).unwrap();
    }

    // Create a weight mask that zeroes the first 10 dimensions
    let mask = WeightMask((0..10).map(|i| (i, 0.0f32)).collect());

    let mut group = c.benchmark_group("hnsw_weighted_search");
    group.bench_with_input(BenchmarkId::new("weighted", format!("{}_mask10", N_VECTORS)), &queries, |b, qs| {
        b.iter(|| {
            for q in qs {
                let results = index.search(black_box(q), EF_SEARCH, Some(&mask)).unwrap();
                black_box(results);
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_hnsw_search, bench_hnsw_build, bench_hnsw_weighted_search);
criterion_main!(benches);
