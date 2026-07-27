// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Criterion benchmark suite for the Topological Dynamic Index.
//!
//! Benchmarks three bias strategies:
//! - **Baseline**: ANN search (ef = 200), then post-filter by metadata
//! - **Static**: α = 0.3, query biased before ANN search
//! - **Adaptive**: α varies by filter selectivity (restrictive → 0.7, broad → 0.2)
//!
//! Each strategy is evaluated across four query groups:
//! 1. Category filter  2. Year range  3. Combined  4. No filter
//!
//! Ground truth is computed by brute-force scan with Rayon.
//! ANN search simulates the HNSW pattern: find top-ef candidates, then post-filter.
//!
//! Data is generated once and cached to `target/bench-data/` for reproducibility.
//!
//! Run:   cargo bench -p tesseract-core --bench topological
//! Quick: QUICK=1 cargo bench -p tesseract-core --bench topological

use std::path::PathBuf;
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use tesseract_core::topological::{
    apply_topological_bias, BiasFilter, BiasKind, CentroidTracker, CorrelationTracker,
    NumericalBucketTracker, RangeOp,
};

// ═══════════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════════

const DIM: usize = 128;
const N_CATEGORIES: usize = 10;
const CATEGORY_NAMES: [&str; 10] = [
    "science", "art", "music", "sports", "tech",
    "food", "travel", "fashion", "health", "finance",
];

const FULL_VECTORS: usize = 1_000_000;
const QUICK_VECTORS: usize = 10_000;
const K: usize = 10;
const EF_SEARCH: usize = 200;
const STATIC_ALPHA: f64 = 0.3;
const ADAPTIVE_ALPHA_MIN: f64 = 0.2;
const ADAPTIVE_ALPHA_MAX: f64 = 0.7;

// Query counts (full)
const N_CAT: usize = 60;
const N_YEAR: usize = 60;
const N_COMBINED: usize = 40;
const N_NO_FILTER: usize = 40;

// Query counts (quick)
const Q_CAT: usize = 6;
const Q_YEAR: usize = 6;
const Q_COMBINED: usize = 4;
const Q_NO_FILTER: usize = 4;

// ═══════════════════════════════════════════════════════════════════════════════
// Data structures
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize, Deserialize)]
struct DataPoint {
    vector: Vec<f64>,
    category: String,
    year: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum QueryGroup {
    Category,
    Year,
    Combined,
    NoFilter,
}

#[derive(Clone)]
struct BenchmarkQuery {
    query_vector: Vec<f64>,
    filters: Vec<BiasFilter>,
    group: QueryGroup,
    /// Fraction of vectors matching this filter. None for no-filter.
    selectivity: Option<f64>,
}

#[derive(Clone, Copy, Default, Debug)]
struct GroupRecall {
    category: f64,
    year: f64,
    combined: f64,
    no_filter: f64,
}

struct BenchmarkResults {
    baseline_recall: GroupRecall,
    legacy_recall: GroupRecall,    // correlation-only (no buckets)
    static_recall: GroupRecall,    // buckets + correlation fallback
    adaptive_recall: GroupRecall,  // adaptive alpha with buckets
    baseline_latency_us: f64,
    legacy_latency_us: f64,
    static_latency_us: f64,
    adaptive_latency_us: f64,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Path helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("tesseract-core should have a parent").to_path_buf()
}

fn ensure_dir(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("failed to create directory");
}

fn cache_path(n: usize) -> PathBuf {
    workspace_root()
        .join("target")
        .join("bench-data")
        .join(format!("topo-data-{n}-{DIM}.bin"))
}

fn results_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("bench-results")
        .join("topological-benchmark.md")
}

// ═══════════════════════════════════════════════════════════════════════════════
// Data generation
// ═══════════════════════════════════════════════════════════════════════════════

/// Generate synthetic data with:
/// - Random base in 128-dim space
/// - Category cluster signal in dimensions 0..9
/// - Year correlation in dimensions 118..127 (strong: year_norm × 5)
/// - Unit-length normalized vectors for cosine distance
fn generate_data(n: usize) -> Vec<DataPoint> {
    let mut rng = SmallRng::seed_from_u64(42);

    (0..n)
        .map(|_| {
            let cat_idx = rng.r#gen::<usize>() % N_CATEGORIES;
            let category = CATEGORY_NAMES[cat_idx].to_string();
            let year = 2015 + (rng.r#gen::<usize>() % 11) as i32;

            let mut vector = vec![0.0; DIM];

            // Random base
            for v in &mut vector {
                *v = rng.r#gen::<f64>() * 2.0 - 1.0;
            }

            // Category cluster: dims 0..9 — each category has a distinct center
            for (i, v) in vector.iter_mut().enumerate().take(10) {
                *v += if i == cat_idx { 3.0 } else { -0.5 };
            }

            // Year correlation: dims 118..127 — strong recency signal
            let year_norm = (year as f64 - 2015.0) / 10.0; // 0..1
            for v in vector.iter_mut().take(128).skip(118) {
                *v += year_norm * 5.0;
            }

            // Normalize to unit length
            let norm: f64 = vector.iter().map(|x| x * x).sum::<f64>().sqrt();
            for v in &mut vector {
                *v /= norm;
            }

            DataPoint { vector, category, year }
        })
        .collect()
}

fn load_or_generate_data(n: usize) -> Vec<DataPoint> {
    let path = cache_path(n);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(data) = bincode::deserialize::<Vec<DataPoint>>(&bytes) {
            eprintln!("[topo] Loaded {} vectors from cache", data.len());
            return data;
        }
    }

    eprintln!("[topo] Generating {} vectors...", n);
    let start = Instant::now();
    let data = generate_data(n);
    eprintln!("[topo] Generated in {:?}", start.elapsed());

    ensure_dir(path.parent().unwrap());
    let bytes = bincode::serialize(&data).expect("serialize");
    std::fs::write(&path, &bytes).expect("write cache");
    eprintln!("[topo] Cached to {}", path.display());

    data
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tracker building
// ═══════════════════════════════════════════════════════════════════════════════

fn build_trackers(data: &[DataPoint]) -> (CentroidTracker, CorrelationTracker, NumericalBucketTracker) {
    let mut centroids = CentroidTracker::new(DIM);
    let mut correlations = CorrelationTracker::new(DIM);
    let mut buckets = NumericalBucketTracker::new(DIM);
    buckets.register_field("year", vec![2015.0, 2018.0, 2021.0, 2024.0]).expect("static bucket config");
    let cat_fields = vec!["category".to_string()];

    for dp in data {
        let meta = serde_json::json!({"category": dp.category, "year": dp.year});
        centroids.update(&dp.vector, &meta, &cat_fields);
        correlations.update("year", dp.year as f64, &dp.vector);
        buckets.update("year", dp.year as f64, &dp.vector);
    }

    (centroids, correlations, buckets)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Query generation
// ═══════════════════════════════════════════════════════════════════════════════

fn count_matching(data: &[DataPoint], q: &BenchmarkQuery) -> usize {
    match q.group {
        QueryGroup::Category => {
            let cat = match &q.filters[0].kind {
                BiasKind::Category(c) => c.as_str(),
                _ => unreachable!(),
            };
            data.iter().filter(|dp| dp.category == cat).count()
        }
        QueryGroup::Year => {
            let y = match &q.filters[0].kind {
                BiasKind::Numerical { value: v, .. } => *v as i32,
                _ => unreachable!(),
            };
            data.iter().filter(|dp| dp.year >= y).count()
        }
        QueryGroup::Combined => {
            let cat = match &q.filters[0].kind {
                BiasKind::Category(c) => c.as_str(),
                _ => unreachable!(),
            };
            let y = match &q.filters[1].kind {
                BiasKind::Numerical { value: v, .. } => *v as i32,
                _ => unreachable!(),
            };
            data.iter().filter(|dp| dp.category == cat && dp.year >= y).count()
        }
        QueryGroup::NoFilter => data.len(),
    }
}

fn generate_queries(
    n_cat: usize,
    n_year: usize,
    n_combined: usize,
    n_nofilter: usize,
) -> Vec<BenchmarkQuery> {
    let mut rng = SmallRng::seed_from_u64(123);
    let mut queries = Vec::with_capacity(n_cat + n_year + n_combined + n_nofilter);

    let mut random_unit = || -> Vec<f64> {
        let mut v = vec![0.0; DIM];
        for x in &mut v {
            *x = rng.r#gen::<f64>() * 2.0 - 1.0;
        }
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in &mut v {
            *x /= norm;
        }
        v
    };

    // Category — evenly distributed across all categories
    for i in 0..n_cat {
        let cat = CATEGORY_NAMES[i % N_CATEGORIES].to_string();
        queries.push(BenchmarkQuery {
            query_vector: random_unit(),
            filters: vec![BiasFilter {
                field: "category".into(),
                kind: BiasKind::Category(cat),
            }],
            group: QueryGroup::Category,
            selectivity: None,
        });
    }

    // Year — targets at or above the mean (bucketized centroid bias)
    for i in 0..n_year {
        let year = 2020 + (i % 6) as i32; // 2020–2025
        queries.push(BenchmarkQuery {
            query_vector: random_unit(),
            filters: vec![BiasFilter {
                field: "year".into(),
                kind: BiasKind::Numerical { value: year as f64, op: RangeOp::Gte },
            }],
            group: QueryGroup::Year,
            selectivity: None,
        });
    }

    // Combined — category + year
    for i in 0..n_combined {
        let cat = CATEGORY_NAMES[i % N_CATEGORIES].to_string();
        let year = if i % 2 == 0 { 2023 } else { 2021 };
        queries.push(BenchmarkQuery {
            query_vector: random_unit(),
            filters: vec![
                BiasFilter {
                    field: "category".into(),
                    kind: BiasKind::Category(cat),
                },
                BiasFilter {
                    field: "year".into(),
                    kind: BiasKind::Numerical { value: year as f64, op: RangeOp::Gte },
                },
            ],
            group: QueryGroup::Combined,
            selectivity: None,
        });
    }

    // No-filter
    for _ in 0..n_nofilter {
        queries.push(BenchmarkQuery {
            query_vector: random_unit(),
            filters: vec![],
            group: QueryGroup::NoFilter,
            selectivity: None,
        });
    }

    queries
}

fn fill_selectivity(queries: &mut [BenchmarkQuery], data: &[DataPoint]) {
    for q in queries.iter_mut() {
        let matching = count_matching(data, q);
        q.selectivity = Some(matching as f64 / data.len() as f64);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Distance
// ═══════════════════════════════════════════════════════════════════════════════

#[inline]
fn cosine_distance(a: &[f64], b: &[f64]) -> f64 {
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Ground truth — brute-force with metadata filter
// ═══════════════════════════════════════════════════════════════════════════════

fn compute_ground_truth(data: &[DataPoint], queries: &[BenchmarkQuery]) -> Vec<Vec<usize>> {
    queries
        .par_iter()
        .map(|bq| {
            // Collect matching indices
            let matching: Vec<usize> = match bq.group {
                QueryGroup::Category => {
                    let cat = match &bq.filters[0].kind {
                        BiasKind::Category(c) => c.as_str(),
                        _ => unreachable!(),
                    };
                    data.iter()
                        .enumerate()
                        .filter(|(_, dp)| dp.category == cat)
                        .map(|(i, _)| i)
                        .collect()
                }
                QueryGroup::Year => {
                    let y = match &bq.filters[0].kind {
                        BiasKind::Numerical { value: v, .. } => *v as i32,
                        _ => unreachable!(),
                    };
                    data.iter()
                        .enumerate()
                        .filter(|(_, dp)| dp.year >= y)
                        .map(|(i, _)| i)
                        .collect()
                }
                QueryGroup::Combined => {
                    let cat = match &bq.filters[0].kind {
                        BiasKind::Category(c) => c.as_str(),
                        _ => unreachable!(),
                    };
                    let y = match &bq.filters[1].kind {
                        BiasKind::Numerical { value: v, .. } => *v as i32,
                        _ => unreachable!(),
                    };
                    data.iter()
                        .enumerate()
                        .filter(|(_, dp)| dp.category == cat && dp.year >= y)
                        .map(|(i, _)| i)
                        .collect()
                }
                QueryGroup::NoFilter => (0..data.len()).collect(),
            };

            // Score all matching, sort, take top-K
            let mut scored: Vec<(usize, f64)> = matching
                .par_iter()
                .map(|&i| (i, cosine_distance(&bq.query_vector, &data[i].vector)))
                .collect();

            scored.par_sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            scored.truncate(K);
            scored.into_iter().map(|(i, _)| i).collect()
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════════
// ANN simulation — brute-force with ef candidates, then post-filter
// ═══════════════════════════════════════════════════════════════════════════════

/// Simulate ANN search: compute distances to ALL vectors, keep top-EF,
/// then post-filter by the query's metadata condition to produce top-K results.
fn ann_search_postfilter(
    data: &[DataPoint],
    query: &[f64],
    bq: &BenchmarkQuery,
    ef: usize,
) -> Vec<usize> {
    // Compute distances to ALL vectors (brute-force, parallel)
    let mut scored: Vec<(usize, f64)> = data
        .par_iter()
        .enumerate()
        .map(|(i, dp)| (i, cosine_distance(query, &dp.vector)))
        .collect();

    // Take top-ef by distance
    scored.par_sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    scored.truncate(ef);

    // Post-filter by the metadata condition
    let filtered: Vec<usize> = match bq.group {
        QueryGroup::Category => {
            let cat = match &bq.filters[0].kind {
                BiasKind::Category(c) => c.as_str(),
                _ => unreachable!(),
            };
            scored
                .into_iter()
                .filter(|(i, _)| data[*i].category == cat)
                .map(|(i, _)| i)
                .take(K)
                .collect()
        }
        QueryGroup::Year => {
            let y = match &bq.filters[0].kind {
                BiasKind::Numerical { value: v, .. } => *v as i32,
                _ => unreachable!(),
            };
            scored
                .into_iter()
                .filter(|(i, _)| data[*i].year >= y)
                .map(|(i, _)| i)
                .take(K)
                .collect()
        }
        QueryGroup::Combined => {
            let cat = match &bq.filters[0].kind {
                BiasKind::Category(c) => c.as_str(),
                _ => unreachable!(),
            };
            let y = match &bq.filters[1].kind {
                BiasKind::Numerical { value: v, .. } => *v as i32,
                _ => unreachable!(),
            };
            scored
                .into_iter()
                .filter(|(i, _)| data[*i].category == cat && data[*i].year >= y)
                .map(|(i, _)| i)
                .take(K)
                .collect()
        }
        QueryGroup::NoFilter => scored.into_iter().map(|(i, _)| i).take(K).collect(),
    };

    filtered
}

// ═══════════════════════════════════════════════════════════════════════════════
// Adaptive alpha
// ═══════════════════════════════════════════════════════════════════════════════

fn adaptive_alpha(selectivity: Option<f64>) -> f64 {
    match selectivity {
        Some(s) => (ADAPTIVE_ALPHA_MIN + (ADAPTIVE_ALPHA_MAX - ADAPTIVE_ALPHA_MIN) * (1.0 - s))
            .clamp(ADAPTIVE_ALPHA_MIN, ADAPTIVE_ALPHA_MAX),
        None => 0.0,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Strategy evaluator
// ═══════════════════════════════════════════════════════════════════════════════

fn evaluate_strategy(
    data: &[DataPoint],
    queries: &[BenchmarkQuery],
    ground_truth: &[Vec<usize>],
    centroids: &CentroidTracker,
    correlations: &CorrelationTracker,
    buckets: &NumericalBucketTracker,
    alpha_fn: impl Fn(Option<f64>) -> f64 + Sync,
) -> GroupRecall {
    let per_query: Vec<(QueryGroup, f64)> = (0..queries.len())
        .into_par_iter()
        .map(|i| {
            let bq = &queries[i];
            let alpha = alpha_fn(bq.selectivity);

            let search_q = if bq.filters.is_empty() || alpha == 0.0 {
                bq.query_vector.clone()
            } else {
                apply_topological_bias(&bq.query_vector, &bq.filters, centroids, correlations, buckets, alpha)
            };

            let ann_ids = ann_search_postfilter(data, &search_q, bq, EF_SEARCH);
            let hits = ground_truth[i].iter().filter(|id| ann_ids.contains(id)).count();
            let recall = hits as f64 / K as f64;

            (bq.group, recall)
        })
        .collect();

    let mut cat = Vec::new();
    let mut yr = Vec::new();
    let mut comb = Vec::new();
    let mut nof = Vec::new();

    for (g, r) in &per_query {
        match g {
            QueryGroup::Category => cat.push(*r),
            QueryGroup::Year => yr.push(*r),
            QueryGroup::Combined => comb.push(*r),
            QueryGroup::NoFilter => nof.push(*r),
        }
    }

    let avg = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };

    GroupRecall { category: avg(&cat), year: avg(&yr), combined: avg(&comb), no_filter: avg(&nof) }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Latency measurement (informal, in-process timing)
// ═══════════════════════════════════════════════════════════════════════════════

fn measure_search_latency(
    data: &[DataPoint],
    queries: &[BenchmarkQuery],
    centroids: &CentroidTracker,
    correlations: &CorrelationTracker,
    buckets: &NumericalBucketTracker,
) -> (f64, f64, f64, f64) {
    let empty_buckets = NumericalBucketTracker::new(DIM);

    let measure = |alpha_fn: &dyn Fn(Option<f64>) -> f64, tracker: &NumericalBucketTracker| -> f64 {
        let start = Instant::now();
        for bq in queries {
            let alpha = alpha_fn(bq.selectivity);
            let search_q = if bq.filters.is_empty() || alpha == 0.0 {
                bq.query_vector.clone()
            } else {
                apply_topological_bias(&bq.query_vector, &bq.filters, centroids, correlations, tracker, alpha)
            };
            let _ = ann_search_postfilter(data, &search_q, bq, EF_SEARCH);
        }
        start.elapsed().as_secs_f64() * 1_000_000.0 / queries.len() as f64
    };

    let baseline = measure(&|_| 0.0, &empty_buckets);
    let legacy = measure(&|_| STATIC_ALPHA, &empty_buckets);
    let static_lat = measure(&|_| STATIC_ALPHA, buckets);
    let adaptive_lat = measure(&|_| adaptive_alpha(None), buckets);

    (baseline, legacy, static_lat, adaptive_lat)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Results output
// ═══════════════════════════════════════════════════════════════════════════════

fn print_table(results: &BenchmarkResults) {
    println!();
    println!(" Topological Dynamic Index Benchmark Results");
    println!(" ─────────────────────────────────────────────");
    println!();
    println!(
        " {:<12} {:>18} {:>16} {:>18} {:>18} {:>14}",
        "Strategy", "Category recall", "Year recall", "Combined recall", "No-filter recall", "Latency μs"
    );
    println!(
        " {:-<12} {:->18} {:->16} {:->18} {:->18} {:->14}",
        "", "", "", "", "", ""
    );

    let row = |label: &str, r: &GroupRecall, lat: f64| {
        println!(
            " {:<12} {:>18.4} {:>16.4} {:>18.4} {:>18.4} {:>14.1}",
            label, r.category, r.year, r.combined, r.no_filter, lat
        );
    };

    row("Baseline", &results.baseline_recall, results.baseline_latency_us);
    row("Legacy", &results.legacy_recall, results.legacy_latency_us);
    row("Static", &results.static_recall, results.static_latency_us);
    row("Adaptive", &results.adaptive_recall, results.adaptive_latency_us);
    println!();
}

fn write_results_markdown(results: &BenchmarkResults, n_vecs: usize, n_queries: usize) {
    ensure_dir(results_path().parent().unwrap());

    let content = format!(
        "\
# Topological Dynamic Index Benchmark Report

| Strategy | Category recall@10 | Year recall@10 | Combined recall@10 | No-filter recall@10 | Latency μs |
|----------|------------------:|---------------:|-------------------:|--------------------:|-----------:|
| Baseline | {:.4}             | {:.4}          | {:.4}              | {:.4}               | {:.1}      |
| Legacy   | {:.4}             | {:.4}          | {:.4}              | {:.4}               | {:.1}      |
| Static   | {:.4}             | {:.4}          | {:.4}              | {:.4}               | {:.1}      |
| Adaptive | {:.4}             | {:.4}          | {:.4}              | {:.4}               | {:.1}      |

## Methodology

- **Vectors**: {nvecs} × {dim} dims
- **Queries**: {nq} total (cat: {ncat}, year: {nyear}, combined: {ncomb}, no-filter: {nnof})
- **Ground truth**: brute-force scan of all vectors with metadata pre-filter, parallel with Rayon
- **ANN simulation**: brute-force all vectors for top-{ef} candidates, then post-filter by metadata
- **Legacy (correlation-only)**: α = {salpha} — correlation-based year bias (old approach)
- **Static (bucketized centroids)**: α = {salpha} — bucketized centroid year bias with correlation fallback
- **Adaptive bias**: α ∈ [{amin}, {amax}] — linearly interpolated by filter selectivity
  (restrictive filter → high α, broad filter → low α)
- **No-filter sanity check**: all strategies return identical recall because bias is a no-op without filters
- **Metric**: recall@10 = |ground-truth top-10 ∩ ANN top-10 (post-filtered)| / 10
- **Bucket boundaries for year**: [2015, 2018, 2021, 2024] — 4 buckets
",
        results.baseline_recall.category,
        results.baseline_recall.year,
        results.baseline_recall.combined,
        results.baseline_recall.no_filter,
        results.baseline_latency_us,
        results.legacy_recall.category,
        results.legacy_recall.year,
        results.legacy_recall.combined,
        results.legacy_recall.no_filter,
        results.legacy_latency_us,
        results.static_recall.category,
        results.static_recall.year,
        results.static_recall.combined,
        results.static_recall.no_filter,
        results.static_latency_us,
        results.adaptive_recall.category,
        results.adaptive_recall.year,
        results.adaptive_recall.combined,
        results.adaptive_recall.no_filter,
        results.adaptive_latency_us,
        nvecs = n_vecs,
        dim = DIM,
        nq = n_queries,
        ncat = N_CAT,
        nyear = N_YEAR,
        ncomb = N_COMBINED,
        nnof = N_NO_FILTER,
        ef = EF_SEARCH,
        salpha = STATIC_ALPHA,
        amin = ADAPTIVE_ALPHA_MIN,
        amax = ADAPTIVE_ALPHA_MAX,
    );

    std::fs::write(results_path(), &content).expect("write results");
    eprintln!("[topo] Report → {}", results_path().display());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Criterion latency benchmarks (formal, single-query microbenchmark)
// ═══════════════════════════════════════════════════════════════════════════════

fn criterion_latency(c: &mut Criterion, data: &[DataPoint]) {
    // Single query with category filter for clean measurement
    let mut probe = generate_queries(1, 0, 0, 0);
    probe[0].selectivity = Some(0.1);
    let (centroids, correlations, buckets) = build_trackers(data);

    let mut group = c.benchmark_group("topological_search");
    group.sample_size(10).measurement_time(Duration::from_secs(10));

    // Baseline — no bias, just ANN search with post-filter
    group.bench_function("baseline", |b| {
        b.iter(|| {
            let q = &probe[0];
            let _ = black_box(ann_search_postfilter(data, &q.query_vector, q, EF_SEARCH));
        });
    });

    // Legacy bias (correlation-only)
    group.bench_function("legacy_bias", |b| {
        b.iter(|| {
            let q = &probe[0];
            let empty = NumericalBucketTracker::new(DIM);
            let biased = black_box(apply_topological_bias(
                &q.query_vector, &q.filters, &centroids, &correlations, &empty, STATIC_ALPHA,
            ));
            let _ = black_box(ann_search_postfilter(data, &biased, q, EF_SEARCH));
        });
    });

    // Static bias (bucketized centroids + correlation fallback)
    group.bench_function("static_bias", |b| {
        b.iter(|| {
            let q = &probe[0];
            let biased = black_box(apply_topological_bias(
                &q.query_vector, &q.filters, &centroids, &correlations, &buckets, STATIC_ALPHA,
            ));
            let _ = black_box(ann_search_postfilter(data, &biased, q, EF_SEARCH));
        });
    });

    // Adaptive bias
    group.bench_function("adaptive_bias", |b| {
        b.iter(|| {
            let q = &probe[0];
            let alpha = adaptive_alpha(q.selectivity);
            let biased = black_box(apply_topological_bias(
                &q.query_vector, &q.filters, &centroids, &correlations, &buckets, alpha,
            ));
            let _ = black_box(ann_search_postfilter(data, &biased, q, EF_SEARCH));
        });
    });

    group.finish();
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main benchmark orchestrator
// ═══════════════════════════════════════════════════════════════════════════════

fn topological_bench(c: &mut Criterion) {
    let is_quick = std::env::var("QUICK").is_ok();

    let n_vecs = if is_quick { QUICK_VECTORS } else { FULL_VECTORS };
    let (n_cat, n_year, n_comb, n_nof) = if is_quick {
        (Q_CAT, Q_YEAR, Q_COMBINED, Q_NO_FILTER)
    } else {
        (N_CAT, N_YEAR, N_COMBINED, N_NO_FILTER)
    };
    let n_q = n_cat + n_year + n_comb + n_nof;

    // ── Setup (one-time) ──────────────────────────────────────────────────
    let data = load_or_generate_data(n_vecs);
    let (centroids, correlations, buckets) = build_trackers(&data);
    let empty_buckets = NumericalBucketTracker::new(DIM);

    let mut queries = generate_queries(n_cat, n_year, n_comb, n_nof);
    fill_selectivity(&mut queries, &data);

    // ── Ground truth ──────────────────────────────────────────────────────
    eprintln!("[topo] Ground truth...");
    let gt_start = Instant::now();
    let ground_truth = compute_ground_truth(&data, &queries);
    eprintln!("[topo] Ground truth: {:?}", gt_start.elapsed());

    // ── Strategy evaluation ───────────────────────────────────────────────
    eprintln!("[topo] Evaluating strategies...");
    let eval_start = Instant::now();

    let baseline_recall = evaluate_strategy(
        &data, &queries, &ground_truth, &centroids, &correlations, &empty_buckets, |_| 0.0,
    );
    let legacy_recall = evaluate_strategy(
        &data, &queries, &ground_truth, &centroids, &correlations, &empty_buckets, |_| STATIC_ALPHA,
    );
    let static_recall = evaluate_strategy(
        &data, &queries, &ground_truth, &centroids, &correlations, &buckets, |_| STATIC_ALPHA,
    );
    let adaptive_recall = evaluate_strategy(
        &data, &queries, &ground_truth, &centroids, &correlations, &buckets, adaptive_alpha,
    );

    eprintln!("[topo] Evaluation: {:?}", eval_start.elapsed());

    // ── Latency ───────────────────────────────────────────────────────────
    let (blat, llat, slat, alat) =
        measure_search_latency(&data, &queries, &centroids, &correlations, &buckets);

    let results = BenchmarkResults {
        baseline_recall,
        legacy_recall,
        static_recall,
        adaptive_recall,
        baseline_latency_us: blat,
        legacy_latency_us: llat,
        static_latency_us: slat,
        adaptive_latency_us: alat,
    };

    // ── Output ────────────────────────────────────────────────────────────
    print_table(&results);
    write_results_markdown(&results, n_vecs, n_q);

    // ── Criterion formal latency ──────────────────────────────────────────
    criterion_latency(c, &data);
}

criterion_group!(benches, topological_bench);
criterion_main!(benches);
