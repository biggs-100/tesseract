// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Criterion benchmark suite for the Progressive Merkle Tree.
//!
//! Measures:
//! - Insert throughput (HotBuffer accepts inserts)
//! - Merge latency (buffer → MerkleTree via insert_batch)
//! - Freshness recall (HotBuffer returns fresh vectors not yet in tree)
//! - Scan overhead at different fill levels (empty → 50% → 100%)
//!
//! Run:   cargo bench -p tesseract-index --bench merkle
//! Quick: QUICK=1 cargo bench -p tesseract-index --bench merkle

use std::path::PathBuf;
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use tesseract_index::merkle::{BufferedVector, HotBuffer, MerkleTree};

// ═══════════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════════

const DIM: usize = 128;
const BUFFER_CAPACITY: usize = 10_000;
const MAX_CLUSTER_SIZE: usize = 500;
const BATCH_SIZE: usize = 10_000;
const N_NEW_VECTORS: usize = 1_000;
const K: usize = 10;

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("tesseract-index should have a parent").to_path_buf()
}

fn ensure_dir(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("failed to create directory");
}

fn results_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("bench-results")
        .join("merkle-benchmark.md")
}

fn generate_vectors(n: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n)
        .map(|_| {
            let mut v: Vec<f32> = (0..DIM).map(|_| rng.gen_range(-1.0..1.0)).collect();
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            v
        })
        .collect()
}

fn buffered_vector(id: u64, vector: Vec<f32>) -> BufferedVector {
    BufferedVector { id, vector, metadata: serde_json::json!({}) }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Freshness recall evaluation (one-time, informal)
// ═══════════════════════════════════════════════════════════════════════════════

struct FreshnessResults {
    hot_recall: f64,
    cold_recall: f64,
}

/// Evaluate recall@10 with and without the hot buffer.
///
/// The "cold" path queries only the MerkleTree (which has base vectors but not
/// the new ones). The "hot" path queries the HotBuffer (which has the new
/// vectors). Ground truth is the new vector itself — since the query IS the
/// vector, exact match has distance ~0.
fn evaluate_freshness() -> FreshnessResults {
    let base_vectors = generate_vectors(BATCH_SIZE, 42);
    let new_vectors = generate_vectors(N_NEW_VECTORS, 43);

    // Build tree with base vectors.
    let mut tree = MerkleTree::new(MAX_CLUSTER_SIZE);
    let base_bvs: Vec<BufferedVector> = base_vectors
        .iter()
        .enumerate()
        .map(|(i, v)| buffered_vector(i as u64, v.clone()))
        .collect();
    tree.insert_batch(&base_bvs);

    // Insert new vectors into hot buffer (IDs start after base batch).
    let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
    for (i, v) in new_vectors.iter().enumerate() {
        let id = (BATCH_SIZE + i) as u64;
        buffer.insert(id, v.clone(), serde_json::json!({}));
    }

    // Evaluate recall@10 for each new vector.
    let mut hot_hits = 0usize;
    let mut cold_hits = 0usize;
    let total = new_vectors.len();

    for (i, v) in new_vectors.iter().enumerate() {
        let expected_id = (BATCH_SIZE + i) as u64;

        // Hot buffer search: the vector IS in the buffer → distance ~0.
        let hot_results = buffer.search(v, K);
        if hot_results.iter().any(|(id, _)| *id == expected_id) {
            hot_hits += 1;
        }

        // Tree-only search: the vector is NOT in the tree → only centroids.
        let cold_results = tree.search(v, K);
        if cold_results.iter().any(|(id, _)| *id == expected_id) {
            cold_hits += 1;
        }
    }

    FreshnessResults {
        hot_recall: hot_hits as f64 / total as f64 * 100.0,
        cold_recall: cold_hits as f64 / total as f64 * 100.0,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Informal timing (for summary table)
// ═══════════════════════════════════════════════════════════════════════════════

struct InformalTiming {
    insert_us_per_op: f64,
    merge_us_per_batch: f64,
    scan_empty_us: f64,
    scan_half_us: f64,
    scan_full_us: f64,
}

fn measure_informal_timing() -> InformalTiming {
    let vectors = generate_vectors(BATCH_SIZE, 42);
    let query = generate_vectors(1, 99).remove(0);
    let n_iterations: usize = 10;

    // ── Insert throughput ──────────────────────────────────────────────────
    let insert_start = Instant::now();
    for _ in 0..n_iterations {
        let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
        for (i, v) in vectors.iter().enumerate() {
            buffer.insert(i as u64, v.clone(), serde_json::json!({"idx": i}));
        }
        black_box(buffer.len());
    }
    let insert_elapsed = insert_start.elapsed().as_secs_f64();
    let total_ops = BATCH_SIZE as f64 * n_iterations as f64;
    let insert_us_per_op = (insert_elapsed * 1_000_000.0) / total_ops;

    // ── Merge latency ──────────────────────────────────────────────────────
    // Pre-fill and drain once (reused across merge iterations).
    let drained = {
        let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
        for (i, v) in vectors.iter().enumerate() {
            buffer.insert(i as u64, v.clone(), serde_json::json!({"idx": i}));
        }
        buffer.drain()
    };

    let merge_start = Instant::now();
    for _ in 0..n_iterations {
        let mut tree = MerkleTree::new(MAX_CLUSTER_SIZE);
        tree.insert_batch(&drained);
        black_box(tree.num_centroids());
    }
    let merge_elapsed = merge_start.elapsed().as_secs_f64();
    let merge_us_per_batch = (merge_elapsed * 1_000_000.0) / n_iterations as f64;

    // ── Scan overhead ──────────────────────────────────────────────────────
    let all_vecs = generate_vectors(BUFFER_CAPACITY, 42);
    let scan_iterations: usize = 100;

    // Empty buffer.
    let empty_buffer = HotBuffer::new(BUFFER_CAPACITY);
    let start = Instant::now();
    for _ in 0..scan_iterations {
        black_box(empty_buffer.search(&query, K));
    }
    let scan_empty_us = start.elapsed().as_secs_f64() * 1_000_000.0 / scan_iterations as f64;

    // 50% full.
    let mut half_buffer = HotBuffer::new(BUFFER_CAPACITY);
    for (i, v) in all_vecs.iter().enumerate().take(BUFFER_CAPACITY / 2) {
        half_buffer.insert(i as u64, v.clone(), serde_json::json!({}));
    }
    let start = Instant::now();
    for _ in 0..scan_iterations {
        black_box(half_buffer.search(&query, K));
    }
    let scan_half_us = start.elapsed().as_secs_f64() * 1_000_000.0 / scan_iterations as f64;

    // 100% full.
    let mut full_buffer = HotBuffer::new(BUFFER_CAPACITY);
    for (i, v) in all_vecs.iter().enumerate() {
        full_buffer.insert(i as u64, v.clone(), serde_json::json!({}));
    }
    let start = Instant::now();
    for _ in 0..scan_iterations {
        black_box(full_buffer.search(&query, K));
    }
    let scan_full_us = start.elapsed().as_secs_f64() * 1_000_000.0 / scan_iterations as f64;

    InformalTiming {
        insert_us_per_op,
        merge_us_per_batch,
        scan_empty_us,
        scan_half_us,
        scan_full_us,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Results output
// ═══════════════════════════════════════════════════════════════════════════════

fn print_table(freshness: &FreshnessResults, timing: &InformalTiming) {
    println!();
    println!(" Merkle Tree Benchmark Results");
    println!(" ────────────────────────────────────────────");
    println!();
    println!(" {:<30} {:>20} {:>20}", "Scenario", "Metric", "Value");
    println!(" {:-<30} {:->20} {:->20}", "", "", "");
    println!(
        " {:<30} {:>20} {:>18.3}",
        "Insert throughput", "Per insert (µs)", timing.insert_us_per_op
    );
    println!(
        " {:<30} {:>20} {:>18.1}",
        "Insert throughput", "Inserts/µs",
        1.0 / timing.insert_us_per_op
    );
    println!(
        " {:<30} {:>20} {:>18.1}",
        "Merge latency", "Batch 10k → tree (µs)", timing.merge_us_per_batch
    );
    println!(
        " {:<30} {:>20} {:>18.1}",
        "Freshness (hot)", "@10 recall (%)", freshness.hot_recall
    );
    println!(
        " {:<30} {:>20} {:>18.1}",
        "Freshness (cold)", "@10 recall (%)", freshness.cold_recall
    );
    println!(
        " {:<30} {:>20} {:>18.3}",
        "Scan overhead (empty)", "µs", timing.scan_empty_us
    );
    println!(
        " {:<30} {:>20} {:>18.3}",
        "Scan overhead (50%)", "µs", timing.scan_half_us
    );
    println!(
        " {:<30} {:>20} {:>18.3}",
        "Scan overhead (100%)", "µs", timing.scan_full_us
    );
    println!();
}

fn write_results_markdown(freshness: &FreshnessResults, timing: &InformalTiming) {
    ensure_dir(results_path().parent().unwrap());

    let content = format!(
        "\
# Merkle Tree Benchmark Report

| Scenario | Metric | Value |
|---|---|---|
| Insert throughput | Per insert (µs) | {insert_us:.3} |
| Insert throughput | Inserts/µs | {insert_ps:.1} |
| Merge latency | Batch 10k → tree (µs) | {merge_us:.1} |
| Freshness (hot) | @10 recall (%) | {hot_recall:.1} |
| Freshness (cold) | @10 recall (%) | {cold_recall:.1} |
| Scan overhead (empty) | µs | {scan_empty:.3} |
| Scan overhead (50%) | µs | {scan_half:.3} |
| Scan overhead (100%) | µs | {scan_full:.3} |

## Methodology

- **Vectors**: 128-dim f32, normalized to unit length
- **Insert throughput**: HotBuffer::insert of 10k vectors, 10 iterations
- **Merge latency**: MerkleTree::insert_batch of 10k vectors, max_cluster_size=500,
  10 iterations
- **Freshness**: 1k new vectors inserted into HotBuffer only (not in tree).
  Each new vector is queried against buffer (hot) and tree (cold). Ground truth
  is the vector itself — exact match → distance ~0. Recall@10 = fraction of
  queries where the expected ID appears in top-10 results.
- **Scan overhead**: HotBuffer::search(query, 10) at 0%, 50%, 100% fill,
  averaged over 100 iterations per level
- **Data generation**: seeded StdRng (seed 42 for base, 43 for new, 99 for query)
",
        insert_us = timing.insert_us_per_op,
        insert_ps = 1.0 / timing.insert_us_per_op,
        merge_us = timing.merge_us_per_batch,
        hot_recall = freshness.hot_recall,
        cold_recall = freshness.cold_recall,
        scan_empty = timing.scan_empty_us,
        scan_half = timing.scan_half_us,
        scan_full = timing.scan_full_us,
    );

    std::fs::write(results_path(), &content).expect("write results");
    eprintln!("[merkle] Report → {}", results_path().display());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Criterion formal benchmarks
// ═══════════════════════════════════════════════════════════════════════════════

fn bench_insert_throughput(c: &mut Criterion) {
    let vectors = generate_vectors(BATCH_SIZE, 42);

    let mut group = c.benchmark_group("merkle_insert");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("hot_buffer_10k_inserts", |b| {
        b.iter(|| {
            let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
            for (i, v) in vectors.iter().enumerate() {
                buffer.insert(i as u64, v.clone(), serde_json::json!({"idx": i}));
            }
            black_box(buffer.len());
        });
    });

    group.finish();
}

fn bench_merge_latency(c: &mut Criterion) {
    let vectors = generate_vectors(BATCH_SIZE, 42);

    // Pre-fill buffer and drain (shared outside the measured loop).
    let drained = {
        let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
        for (i, v) in vectors.iter().enumerate() {
            buffer.insert(i as u64, v.clone(), serde_json::json!({"idx": i}));
        }
        buffer.drain()
    };

    let mut group = c.benchmark_group("merkle_merge");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("batch_10k_to_tree", |b| {
        b.iter(|| {
            let mut tree = MerkleTree::new(MAX_CLUSTER_SIZE);
            tree.insert_batch(&drained);
            black_box(tree.num_centroids());
        });
    });

    group.finish();
}

fn bench_scan_overhead(c: &mut Criterion) {
    let query = generate_vectors(1, 99).remove(0);
    let all_vectors = generate_vectors(BUFFER_CAPACITY, 42);

    let mut group = c.benchmark_group("merkle_scan");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    // Empty buffer.
    group.bench_with_input(BenchmarkId::new("hot_buffer_search", "empty"), &0usize, |b, _| {
        let buffer = HotBuffer::new(BUFFER_CAPACITY);
        b.iter(|| {
            let results = buffer.search(&query, K);
            black_box(results);
        });
    });

    // 50% full.
    {
        let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
        for (i, v) in all_vectors.iter().enumerate().take(BUFFER_CAPACITY / 2) {
            buffer.insert(i as u64, v.clone(), serde_json::json!({}));
        }
        group.bench_with_input(
            BenchmarkId::new("hot_buffer_search", "50pct"),
            &(BUFFER_CAPACITY / 2),
            |b, _| {
                b.iter(|| {
                    let results = buffer.search(&query, K);
                    black_box(results);
                });
            },
        );
    }

    // 100% full.
    {
        let mut buffer = HotBuffer::new(BUFFER_CAPACITY);
        for (i, v) in all_vectors.iter().enumerate() {
            buffer.insert(i as u64, v.clone(), serde_json::json!({}));
        }
        group.bench_with_input(BenchmarkId::new("hot_buffer_search", "100pct"), &BUFFER_CAPACITY, |b, _| {
            b.iter(|| {
                let results = buffer.search(&query, K);
                black_box(results);
            });
        });
    }

    group.finish();
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main benchmark orchestrator
// ═══════════════════════════════════════════════════════════════════════════════

fn merkle_bench(c: &mut Criterion) {
    // ── Criterion formal benchmarks ──────────────────────────────────────
    bench_insert_throughput(c);
    bench_merge_latency(c);
    bench_scan_overhead(c);

    // ── Informal evaluation ──────────────────────────────────────────────
    eprintln!("[merkle] Evaluating freshness recall...");
    let freshness = evaluate_freshness();

    eprintln!("[merkle] Measuring informal timing...");
    let timing = measure_informal_timing();

    // ── Output ───────────────────────────────────────────────────────────
    print_table(&freshness, &timing);
    write_results_markdown(&freshness, &timing);
}

criterion_group!(benches, merkle_bench);
criterion_main!(benches);
