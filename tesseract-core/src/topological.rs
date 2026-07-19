// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Topological bias engine — centroid-based and correlation-based query vector
//! biasing with zero training.
//!
//! The core insight is to shift the query vector **toward** the region that
//! matches the filter, so HNSW naturally finds relevant results without
//! post-filtering.
//!
//! ## Categorical filters (`category = 'science'`)
//! ```text
//! biased_query = query + α · (centroid(science) - global_centroid)
//! ```
//!
//! ## Numerical filters (`year >= 2020`)
//! ```text
//! biased_query = query + α · Σ correlation_dim · (filter_value - mean_value)
//! ```

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// BiasFilter — query-level bias specification
// ---------------------------------------------------------------------------

/// A single bias filter extracted from a VQL WHERE clause.
///
/// Each filter tells the bias engine to shift the query vector toward a
/// specific metadata region — either categorical (centroid-based) or
/// numerical (correlation-based).
#[derive(Debug, Clone)]
pub struct BiasFilter {
    /// The metadata field this filter applies to (e.g. "category", "year").
    pub field: String,
    /// The kind of bias to apply.
    pub kind: BiasKind,
}

/// A range comparison operator for numerical bias filters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeOp {
    /// Field == value
    Eq,
    /// Field >= value
    Gte,
    /// Field > value
    Gt,
    /// Field <= value
    Lte,
    /// Field < value
    Lt,
    /// Field IN [low, high] (inclusive)
    Range(f64, f64),
}

/// The kind of topological bias to apply.
#[derive(Debug, Clone)]
pub enum BiasKind {
    /// Bias toward the centroid of a specific category value.
    /// The string is the category value (e.g. "science").
    Category(String),
    /// Bias toward a specific numerical target value using bucketized centroids
    /// (or correlation-based fallback when no buckets are configured).
    /// The value is the filter value (e.g. 2020.0 for `year >= 2020`)
    /// and the operator specifies the comparison direction.
    Numerical { value: f64, op: RangeOp },
}

// ---------------------------------------------------------------------------
// CentroidTracker
// ---------------------------------------------------------------------------

/// Tracks per-category centroids and the global centroid for categorical
/// metadata fields.
///
/// Maintained incrementally — every `update()` call adjusts running sums
/// without re-scanning historical data. Used at query time to compute the
/// delta vector `centroid(category) - global_centroid`.
pub struct CentroidTracker {
    /// Running sum of all vectors seen (global).
    pub global_sum: Vec<f64>,
    /// Count of all vectors seen (global).
    pub global_count: u64,
    /// Per-field, per-category running stats:
    ///   field_name → category_value → (sum_vector, count)
    pub categories: HashMap<String, HashMap<String, (Vec<f64>, u64)>>,
}

impl CentroidTracker {
    /// Create a new tracker for `dim`-dimensional vectors.
    pub fn new(dim: usize) -> Self {
        Self {
            global_sum: vec![0.0; dim],
            global_count: 0,
            categories: HashMap::new(),
        }
    }

    /// Update statistics with a new vector and its metadata.
    ///
    /// Only processes fields listed in `tracked_fields`. For each such field
    /// whose value in `metadata` is a string, the vector is added to both the
    /// global centroid and the per-category centroid.
    ///
    /// Called on every insert where topological tracking is enabled.
    pub fn update(&mut self, vector: &[f64], metadata: &serde_json::Value, tracked_fields: &[String]) {
        // Update global centroid
        for (i, v) in vector.iter().enumerate() {
            if i < self.global_sum.len() {
                self.global_sum[i] += v;
            }
        }
        self.global_count += 1;

        // Update per-category centroids for configured fields
        for field in tracked_fields {
            if let Some(serde_json::Value::String(cat)) = metadata.get(field) {
                let entry = self
                    .categories
                    .entry(field.clone())
                    .or_default()
                    .entry(cat.clone())
                    .or_insert_with(|| (vec![0.0; self.global_sum.len()], 0));
                for (i, v) in vector.iter().enumerate() {
                    if i < entry.0.len() {
                        entry.0[i] += v;
                    }
                }
                entry.1 += 1;
            }
        }
    }

    /// Compute the delta vector for a categorical filter:
    /// `centroid(category) - global_centroid`.
    ///
    /// Returns `None` if the field or category has no data.
    pub fn delta(&self, field: &str, value: &str) -> Option<Vec<f64>> {
        if self.global_count == 0 {
            return None;
        }
        let global = self.global_centroid();
        let (cat_sum, cat_count) = self.categories.get(field)?.get(value)?;
        if *cat_count == 0 {
            return None;
        }
        let _dim = global.len();
        let cat_centroid: Vec<f64> = cat_sum.iter().map(|s| s / *cat_count as f64).collect();
        Some(
            global
                .iter()
                .zip(cat_centroid.iter())
                .map(|(g, c)| c - g)
                .collect(),
        )
    }

    /// Get the global centroid (mean of all vectors seen).
    ///
    /// Returns a zero vector when no data has been recorded.
    pub fn global_centroid(&self) -> Vec<f64> {
        if self.global_count == 0 {
            return vec![0.0; self.global_sum.len()];
        }
        let n = self.global_count as f64;
        self.global_sum.iter().map(|s| s / n).collect()
    }

    /// Returns true if no vectors have been recorded.
    pub fn is_empty(&self) -> bool {
        self.global_count == 0
    }
}

// ---------------------------------------------------------------------------
// CorrelationTracker (Welford's online algorithm)
// ---------------------------------------------------------------------------

/// Running numerical statistics for a single field-dimension pair.
///
/// Implements Welford's online algorithm for numerically stable single-pass
/// computation of mean, variance, and covariance.
#[derive(Debug, Clone)]
pub struct NumericalStats {
    /// Number of observations.
    pub count: u64,
    /// Running mean of the field value.
    pub field_mean: f64,
    /// Running M2 for field variance: Σ(f_i - f̄)²
    pub field_m2: f64,
    /// Per-dimension running means of the vector components.
    pub dim_means: Vec<f64>,
    /// Per-dimension running M2: Σ(v_{d,i} - v̄_d)²
    pub dim_m2: Vec<f64>,
    /// Per-dimension running cross-product: Σ(f_i - f̄)(v_{d,i} - v̄_d)
    pub dim_cross: Vec<f64>,
}

impl NumericalStats {
    /// Create new stats for `dim`-dimensional vectors.
    fn new(dim: usize) -> Self {
        Self {
            count: 0,
            field_mean: 0.0,
            field_m2: 0.0,
            dim_means: vec![0.0; dim],
            dim_m2: vec![0.0; dim],
            dim_cross: vec![0.0; dim],
        }
    }

    /// Update running statistics with a new (field_value, vector) pair using
    /// Welford's online algorithm.
    ///
    /// Numerically stable — avoids catastrophic cancellation found in naive
    /// two-pass variance computation.
    fn update(&mut self, field_value: f64, vector: &[f64]) {
        if self.count == 0 {
            // First observation: initialize means, keep M2 = 0
            self.count = 1;
            self.field_mean = field_value;
            self.dim_means = vector.to_vec();
            // M2 and cross remain 0 for single observation
            return;
        }

        let n_old = self.count;
        self.count += 1;
        let n_new = self.count;
        let ratio = n_old as f64 / n_new as f64;

        // Welford update: use OLD means for cross and M2 computation
        let delta_f = field_value - self.field_mean;

        for (d, (dim_val, dim_mean)) in vector.iter().zip(self.dim_means.iter_mut()).enumerate() {
            let delta_d = dim_val - *dim_mean;

            // Update cross-sum: C += dx · dy · n_old / n_new
            self.dim_cross[d] += delta_f * delta_d * ratio;
            // Update dim variance M2
            self.dim_m2[d] += delta_d * delta_d * ratio;

            // Update dim mean
            *dim_mean += delta_d / n_new as f64;
        }

        // Update field variance M2
        self.field_m2 += delta_f * delta_f * ratio;
        // Update field mean
        self.field_mean += delta_f / n_new as f64;
    }
}

/// Tracks dimension-wise Pearson correlation between embedding dimensions and
/// numerical metadata fields.
///
/// Uses Welford's online algorithm for numerically stable covariance tracking.
pub struct CorrelationTracker {
    /// Dimensionality of the embedding space.
    pub dim: usize,
    /// Per-field running statistics.
    pub fields: HashMap<String, NumericalStats>,
}

impl CorrelationTracker {
    /// Create a new tracker for `dim`-dimensional vectors.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            fields: HashMap::new(),
        }
    }

    /// Update statistics for a numerical field with a new (field_value, vector)
    /// pair. Creates the field entry if it doesn't exist.
    pub fn update(&mut self, field: &str, field_value: f64, vector: &[f64]) {
        let stats = self
            .fields
            .entry(field.to_string())
            .or_insert_with(|| NumericalStats::new(self.dim));
        stats.update(field_value, vector);
    }

    /// Get per-dimension Pearson correlation coefficients for a field.
    ///
    /// Returns `Vec<f64>` of length `dim`, each value in `[-1, 1]`.
    /// Returns `None` if the field has no data or fewer than 2 observations
    /// (correlation requires at least 2 points).
    ///
    /// Dimensions with zero variance return 0.0 (no correlation measurable).
    pub fn correlations(&self, field: &str) -> Option<Vec<f64>> {
        let stats = self.fields.get(field)?;
        if stats.count < 2 {
            return Some(vec![0.0; self.dim]);
        }

        let field_var = stats.field_m2 / stats.count as f64;
        if field_var <= 0.0 {
            return Some(vec![0.0; self.dim]);
        }
        let field_std = field_var.sqrt();

        let mut result = Vec::with_capacity(self.dim);
        for d in 0..self.dim {
            if d >= stats.dim_m2.len() || d >= stats.dim_cross.len() {
                result.push(0.0);
                continue;
            }
            let dim_var = stats.dim_m2[d] / stats.count as f64;
            if dim_var <= 0.0 {
                result.push(0.0);
                continue;
            }
            let dim_std = dim_var.sqrt();
            let r = stats.dim_cross[d] / (stats.count as f64 * field_std * dim_std);
            result.push(r.clamp(-1.0, 1.0));
        }
        Some(result)
    }

    /// Get the field's mean value. Returns `None` if the field has no data.
    pub fn field_mean(&self, field: &str) -> Option<f64> {
        let stats = self.fields.get(field)?;
        if stats.count == 0 {
            return None;
        }
        Some(stats.field_mean)
    }
}

// ---------------------------------------------------------------------------
// NumericalBucketTracker
// ---------------------------------------------------------------------------

/// Per-bucket data for a numerical field.
pub struct NumericalBuckets {
    /// Sorted bucket boundaries.
    /// With boundaries `[2015, 2018, 2021, 2024]` there are 4 buckets:
    ///   bucket 0: < 2018
    ///   bucket 1: 2018..2021
    ///   bucket 2: 2021..2024
    ///   bucket 3: >= 2024
    pub boundaries: Vec<f64>,
    /// Per-bucket sum vectors.
    pub sums: Vec<Vec<f64>>,
    /// Per-bucket observation counts.
    pub counts: Vec<u64>,
}

/// Tracks bucketized centroids for numerical metadata fields.
///
/// Similar to `CentroidTracker` but for bucketed numerical ranges.
/// Each numerical field has a set of bucket boundaries, and the tracker
/// maintains a running centroid per bucket.
///
/// At query time, the filter operator and value determine which buckets'
/// centroids are averaged to produce the bias delta.
pub struct NumericalBucketTracker {
    dim: usize,
    /// Running sum of all vectors seen (global).
    global_sum: Vec<f64>,
    /// Count of all vectors seen (global).
    global_count: u64,
    /// Per-field bucket data: field_name → NumericalBuckets
    fields: HashMap<String, NumericalBuckets>,
}

impl NumericalBucketTracker {
    /// Create a new tracker for `dim`-dimensional vectors.
    pub fn new(dim: usize) -> Self {
        Self { dim, global_sum: vec![0.0; dim], global_count: 0, fields: HashMap::new() }
    }

    /// Register a numerical field with its bucket boundaries.
    ///
    /// `boundaries` are sorted split points. With `n` boundaries, there are `n` buckets:
    ///   - bucket 0: values < `boundaries[1]` (or `boundaries[0]` for the lower bound)
    ///   - bucket i (0 < i < n-1): [`boundaries[i]`, `boundaries[i+1]`)
    ///   - bucket n-1: values >= `boundaries[n-1]`
    ///
    /// # Panics
    /// Panics if `boundaries` is empty.
    pub fn register_field(&mut self, field: &str, boundaries: Vec<f64>) {
        assert!(!boundaries.is_empty(), "bucket boundaries must not be empty");
        let n = boundaries.len();
        self.fields.insert(
            field.to_string(),
            NumericalBuckets {
                boundaries,
                sums: vec![vec![0.0; self.dim]; n],
                counts: vec![0; n],
            },
        );
    }

    /// Determine which bucket a value falls into.
    fn bucket_index(&self, boundaries: &[f64], value: f64) -> usize {
        if value < boundaries[0] {
            return 0;
        }
        for i in 0..boundaries.len().saturating_sub(1) {
            if value < boundaries[i + 1] {
                return i;
            }
        }
        // Value >= last boundary → last bucket
        boundaries.len().saturating_sub(1)
    }

    /// Update statistics with a new (field_value, vector) pair.
    ///
    /// The value is assigned to a bucket and that bucket's centroid is updated.
    /// The global centroid is also updated.
    ///
    /// If the field hasn't been registered, this is a no-op.
    pub fn update(&mut self, field: &str, value: f64, vector: &[f64]) {
        // Update global centroid
        for (i, v) in vector.iter().enumerate() {
            if i < self.global_sum.len() {
                self.global_sum[i] += v;
            }
        }
        self.global_count += 1;

        // Update per-bucket centroid — clone boundaries to avoid borrow conflict
        let boundaries: Option<Vec<f64>> = self.fields.get(field).map(|b| b.boundaries.clone());
        if let Some(bounds) = boundaries {
            let idx = self.bucket_index(&bounds, value);
            if let Some(buckets) = self.fields.get_mut(field) {
                if idx < buckets.sums.len() {
                    for (i, v) in vector.iter().enumerate() {
                        if i < buckets.sums[idx].len() {
                            buckets.sums[idx][i] += v;
                        }
                    }
                    buckets.counts[idx] += 1;
                }
            }
        }
    }

    /// Get the global centroid (mean of all vectors seen).
    pub fn global_centroid(&self) -> Vec<f64> {
        if self.global_count == 0 {
            return vec![0.0; self.global_sum.len()];
        }
        let n = self.global_count as f64;
        self.global_sum.iter().map(|s| s / n).collect()
    }

    /// Returns `true` if the field has registered buckets.
    pub fn has_buckets(&self, field: &str) -> bool {
        self.fields.contains_key(field)
    }

    /// Compute the centroid delta for a numerical filter.
    ///
    /// Returns the mean `(bucket_centroid - global_centroid)` across all
    /// buckets that are relevant for the given `op` and `value`.
    ///
    /// Returns `None` if the field has no configured buckets or no data.
    pub fn delta(&self, field: &str, value: f64, op: RangeOp) -> Option<Vec<f64>> {
        let buckets = self.fields.get(field)?;
        if buckets.boundaries.is_empty() || self.global_count == 0 {
            return None;
        }

        let global = self.global_centroid();
        let value_idx = self.bucket_index(&buckets.boundaries, value);

        // Determine which bucket indices to include based on the operator
        let candidate_indices: Vec<usize> = match op {
            RangeOp::Eq => vec![value_idx],
            RangeOp::Gte | RangeOp::Gt => (value_idx..buckets.counts.len()).collect(),
            RangeOp::Lte | RangeOp::Lt => (0..=value_idx).collect(),
            RangeOp::Range(low, high) => {
                let low_idx = self.bucket_index(&buckets.boundaries, low);
                let high_idx = self.bucket_index(&buckets.boundaries, high);
                (low_idx..=high_idx.min(buckets.counts.len().saturating_sub(1))).collect()
            }
        };

        // Compute the mean centroid delta across non-empty candidate buckets
        let mut sum_delta = vec![0.0; self.dim];
        let mut valid_count = 0u64;

        for &idx in &candidate_indices {
            if idx < buckets.counts.len()
                && buckets.counts[idx] > 0
                && idx < buckets.sums.len()
            {
                for d in 0..self.dim {
                    if d < buckets.sums[idx].len() {
                        let centroid = buckets.sums[idx][d] / buckets.counts[idx] as f64;
                        sum_delta[d] += centroid - global[d];
                    }
                }
                valid_count += 1;
            }
        }

        if valid_count == 0 {
            return None;
        }

        if valid_count == 1 {
            return Some(sum_delta);
        }

        Some(sum_delta.iter().map(|s| s / valid_count as f64).collect())
    }
}

// ---------------------------------------------------------------------------
// apply_topological_bias
// ---------------------------------------------------------------------------

/// Apply topological bias to a query vector.
///
/// Shifts the query vector toward the metadata region(s) specified by
/// `filters`, so that HNSW naturally finds relevant results.
///
/// # Parameters
/// - `query`: the original query vector
/// - `filters`: bias filters extracted from the VQL WHERE clause
/// - `centroids`: categorical centroid tracker (from storage engine)
/// - `correlations`: numerical correlation tracker (from storage engine)
/// - `buckets`: numerical bucket tracker (from storage engine, fallback when
///   no buckets are configured for a field)
/// - `alpha`: bias strength multiplier (default 0.3, lower for tight budgets)
///
/// # Returns
/// A new biased vector. If no filters match, returns a clone of `query`.
pub fn apply_topological_bias(
    query: &[f64],
    filters: &[BiasFilter],
    centroids: &CentroidTracker,
    correlations: &CorrelationTracker,
    buckets: &NumericalBucketTracker,
    alpha: f64,
) -> Vec<f64> {
    let mut biased = query.to_vec();

    for filter in filters {
        match &filter.kind {
            BiasKind::Category(category_value) => {
                // Biased toward category centroid: query + α · (c - g)
                if let Some(delta) = centroids.delta(&filter.field, category_value) {
                    for (i, d) in delta.iter().enumerate() {
                        if i < biased.len() {
                            biased[i] += alpha * d;
                        }
                    }
                }
            }
            BiasKind::Numerical { value, op } => {
                // Try bucketized centroid bias first (primary approach)
                if let Some(delta) = buckets.delta(&filter.field, *value, *op) {
                    for (i, d) in delta.iter().enumerate() {
                        if i < biased.len() {
                            biased[i] += alpha * d;
                        }
                    }
                } else {
                    // Fall back to correlation-based bias: query[d] += α · r_d · (value - mean)
                    if let Some(corrs) = correlations.correlations(&filter.field) {
                        if let Some(mean) = correlations.field_mean(&filter.field) {
                            let diff = value - mean;
                            for (i, c) in corrs.iter().enumerate() {
                                if i < biased.len() {
                                    biased[i] += alpha * c * diff;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    biased
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // CentroidTracker
    // =======================================================================

    #[test]
    fn centroid_new_creates_empty_tracker() {
        let ct = CentroidTracker::new(4);
        assert_eq!(ct.global_sum, vec![0.0; 4]);
        assert_eq!(ct.global_count, 0);
        assert!(ct.categories.is_empty());
        assert!(ct.is_empty());
    }

    #[test]
    fn centroid_single_vector_update() {
        let mut ct = CentroidTracker::new(3);
        let v = vec![1.0, 2.0, 3.0];
        let meta = serde_json::json!({"category": "science"});

        ct.update(&v, &meta, &["category".to_string()]);

        assert_eq!(ct.global_count, 1);
        assert_eq!(ct.global_sum, vec![1.0, 2.0, 3.0]);

        let cat = ct.categories.get("category").unwrap();
        let (sum, count) = cat.get("science").unwrap();
        assert_eq!(*count, 1);
        assert_eq!(sum, &vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn centroid_multiple_vectors_same_category() {
        let mut ct = CentroidTracker::new(2);
        let tracked = &["category".to_string()];

        ct.update(&vec![1.0, 0.0], &serde_json::json!({"category": "science"}), tracked);
        ct.update(&vec![3.0, 4.0], &serde_json::json!({"category": "science"}), tracked);
        ct.update(&vec![5.0, 2.0], &serde_json::json!({"category": "science"}), tracked);

        assert_eq!(ct.global_count, 3);
        let global = ct.global_centroid();
        assert!((global[0] - 3.0).abs() < 1e-10);
        assert!((global[1] - 2.0).abs() < 1e-10);
    }

    #[test]
    fn centroid_delta_correct() {
        let mut ct = CentroidTracker::new(2);
        let tracked = &["category".to_string()];

        // science: (1,0), (3,4), (5,2)  → centroid (3, 2)
        ct.update(&vec![1.0, 0.0], &serde_json::json!({"category": "science"}), tracked);
        ct.update(&vec![3.0, 4.0], &serde_json::json!({"category": "science"}), tracked);
        ct.update(&vec![5.0, 2.0], &serde_json::json!({"category": "science"}), tracked);
        // art: (0, 10) → centroid (0, 10)
        ct.update(&vec![0.0, 10.0], &serde_json::json!({"category": "art"}), tracked);

        // global centroid: (9/4, 16/4) = (2.25, 4.0)
        // delta(science) = (3 - 2.25, 2 - 4.0) = (0.75, -2.0)
        let delta = ct.delta("category", "science").unwrap();
        assert!((delta[0] - 0.75).abs() < 1e-10);
        assert!((delta[1] - (-2.0)).abs() < 1e-10);
    }

    #[test]
    fn centroid_delta_unknown_field_returns_none() {
        let ct = CentroidTracker::new(2);
        assert!(ct.delta("nonexistent", "value").is_none());
    }

    #[test]
    fn centroid_delta_unknown_value_returns_none() {
        let mut ct = CentroidTracker::new(2);
        ct.update(
            &vec![1.0, 2.0],
            &serde_json::json!({"category": "science"}),
            &["category".to_string()],
        );
        assert!(ct.delta("category", "nonexistent").is_none());
    }

    #[test]
    fn centroid_global_empty_returns_zeros() {
        let ct = CentroidTracker::new(3);
        assert_eq!(ct.global_centroid(), vec![0.0; 3]);
    }

    #[test]
    fn centroid_multiple_fields_independent() {
        let mut ct = CentroidTracker::new(2);
        let tracked = &["genre".to_string(), "language".to_string()];

        ct.update(
            &vec![1.0, 2.0],
            &serde_json::json!({"genre": "scifi", "language": "en"}),
            tracked,
        );
        ct.update(
            &vec![3.0, 4.0],
            &serde_json::json!({"genre": "fantasy", "language": "en"}),
            tracked,
        );

        // genre → fantasy centroid = (3, 4)
        let fantasy_delta = ct.delta("genre", "fantasy").unwrap();
        assert!((fantasy_delta[0] - 1.0).abs() < 1e-10); // 3 - 2 = 1
        assert!((fantasy_delta[1] - 1.0).abs() < 1e-10); // 4 - 3 = 1

        // language → 'en' should exist
        assert!(ct.delta("language", "en").is_some());
    }

    #[test]
    fn centroid_ignores_non_string_metadata() {
        let mut ct = CentroidTracker::new(2);
        ct.update(
            &vec![1.0, 2.0],
            &serde_json::json!({"category": "science", "year": 2020}),
            &["category".to_string(), "year".to_string()],
        );

        // Only "category" should have tracked data (value is a string)
        assert!(ct.categories.get("category").unwrap().contains_key("science"));
        // "year" was configured but value is not a string → no entry
        assert!(ct.categories.get("year").is_none() || ct.categories.get("year").unwrap().is_empty());
    }

    // =======================================================================
    // NumericalStats (Welford)
    // =======================================================================

    #[test]
    fn numerical_stats_first_observation_initializes() {
        let mut stats = NumericalStats::new(2);
        stats.update(10.0, &vec![1.0, 2.0]);

        assert_eq!(stats.count, 1);
        assert!((stats.field_mean - 10.0).abs() < 1e-10);
        assert!((stats.dim_means[0] - 1.0).abs() < 1e-10);
        assert!((stats.dim_means[1] - 2.0).abs() < 1e-10);
        // M2 and cross should be 0 for single observation
        assert!((stats.field_m2).abs() < 1e-10);
        assert!((stats.dim_m2[0]).abs() < 1e-10);
        assert!((stats.dim_cross[0]).abs() < 1e-10);
    }

    #[test]
    fn numerical_stats_two_observations() {
        let mut stats = NumericalStats::new(1);
        stats.update(0.0, &vec![0.0]);
        stats.update(1.0, &vec![1.0]);

        assert_eq!(stats.count, 2);
        assert!((stats.field_mean - 0.5).abs() < 1e-10);
        assert!((stats.dim_means[0] - 0.5).abs() < 1e-10);
        // With (0,0) and (1,1): field_m2 = Σ(f - f̄)² = 0.5
        assert!((stats.field_m2 - 0.5).abs() < 1e-10);
        // covariance = 0.5 (perfect positive for 2 points)
        assert!((stats.dim_cross[0] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn numerical_stats_perfect_positive_correlation() {
        let mut stats = NumericalStats::new(1);
        // Perfect linear relationship: y = 2x
        for i in 1..=10 {
            let x = i as f64;
            let y = 2.0 * x;
            stats.update(x, &vec![y]);
        }

        // Pearson r should be ~1.0
        let r = stats.dim_cross[0] / (stats.count as f64 * (stats.field_m2 / stats.count as f64).sqrt() * (stats.dim_m2[0] / stats.count as f64).sqrt());
        assert!((r - 1.0).abs() < 1e-10, "expected r ≈ 1.0, got {r}");
    }

    #[test]
    fn numerical_stats_perfect_negative_correlation() {
        let mut stats = NumericalStats::new(1);
        // Perfect negative linear: y = -3x + 50
        for i in 1..=10 {
            let x = i as f64;
            let y = -3.0 * x + 50.0;
            stats.update(x, &vec![y]);
        }

        let r = stats.dim_cross[0] / (stats.count as f64 * (stats.field_m2 / stats.count as f64).sqrt() * (stats.dim_m2[0] / stats.count as f64).sqrt());
        assert!((r - (-1.0)).abs() < 1e-10, "expected r ≈ -1.0, got {r}");
    }

    #[test]
    fn numerical_stats_zero_correlation() {
        let mut stats = NumericalStats::new(1);
        // Field varies but dimension is constant
        for i in 1..=10 {
            stats.update(i as f64, &vec![42.0]);
        }

        let corrs = {
            let dim_var = stats.dim_m2[0] / stats.count as f64;
            let field_var = stats.field_m2 / stats.count as f64;
            if dim_var <= 0.0 || field_var <= 0.0 {
                0.0
            } else {
                stats.dim_cross[0] / (stats.count as f64 * field_var.sqrt() * dim_var.sqrt())
            }
        };

        // Dimension has zero variance → correlation should be 0
        assert!((corrs - 0.0).abs() < 1e-10, "expected r ≈ 0.0, got {corrs}");
    }

    // =======================================================================
    // CorrelationTracker
    // =======================================================================

    #[test]
    fn correlation_new_creates_empty() {
        let ct = CorrelationTracker::new(3);
        assert_eq!(ct.dim, 3);
        assert!(ct.fields.is_empty());
    }

    #[test]
    fn correlation_update_multiple_fields() {
        let mut ct = CorrelationTracker::new(2);

        ct.update("price", 10.0, &vec![1.0, 0.0]);
        ct.update("price", 20.0, &vec![2.0, 0.0]);
        ct.update("rating", 5.0, &vec![3.0, 1.0]);
        ct.update("rating", 3.0, &vec![1.0, 2.0]);

        assert!(ct.fields.contains_key("price"));
        assert!(ct.fields.contains_key("rating"));
        assert_eq!(ct.fields["price"].count, 2);
        assert_eq!(ct.fields["rating"].count, 2);
    }

    #[test]
    fn correlation_correlations_fewer_than_two_points_returns_zeros() {
        let mut ct = CorrelationTracker::new(2);
        ct.update("price", 10.0, &vec![1.0, 2.0]);

        let corrs = ct.correlations("price").unwrap();
        assert_eq!(corrs, vec![0.0, 0.0]);
    }

    #[test]
    fn correlation_correlations_unknown_field_returns_none() {
        let ct = CorrelationTracker::new(2);
        assert!(ct.correlations("nonexistent").is_none());
    }

    #[test]
    fn correlation_field_mean_unknown_field() {
        let ct = CorrelationTracker::new(2);
        assert!(ct.field_mean("nonexistent").is_none());
    }

    #[test]
    fn correlation_field_mean_returns_correct() {
        let mut ct = CorrelationTracker::new(1);
        ct.update("age", 25.0, &vec![1.0]);
        ct.update("age", 35.0, &vec![2.0]);

        let mean = ct.field_mean("age").unwrap();
        assert!((mean - 30.0).abs() < 1e-10);
    }

    #[test]
    fn correlation_correlations_in_range() {
        let mut ct = CorrelationTracker::new(3);
        // 10 points with varying correlation
        for i in 1..=10 {
            let x = i as f64;
            ct.update("score", x, &vec![x, -x, 0.0]);
        }

        let corrs = ct.correlations("score").unwrap();
        assert_eq!(corrs.len(), 3);
        // dim 0: perfect positive
        assert!((corrs[0] - 1.0).abs() < 1e-5);
        // dim 1: perfect negative
        assert!((corrs[1] - (-1.0)).abs() < 1e-5);
        // dim 2: zero (constant)
        assert!((corrs[2]).abs() < 1e-10);
    }

    // =======================================================================
    // apply_topological_bias
    // =======================================================================

    #[test]
    fn bias_no_filters_returns_query_unchanged() {
        let centroids = CentroidTracker::new(3);
        let correlations = CorrelationTracker::new(3);
        let buckets = NumericalBucketTracker::new(3);
        let query = vec![1.0, 2.0, 3.0];

        let biased = apply_topological_bias(&query, &[], &centroids, &correlations, &buckets, 0.3);
        assert_eq!(biased, query);
    }

    #[test]
    fn bias_categorical_shifts_query_toward_category() {
        let mut centroids = CentroidTracker::new(2);
        let tracked = &["category".to_string()];
        let correlations = CorrelationTracker::new(2); // unused by centoids
        let buckets = NumericalBucketTracker::new(2);

        // Insert two categories to create a meaningful delta
        centroids.update(&vec![10.0, 0.0], &serde_json::json!({"category": "science"}), tracked);
        centroids.update(&vec![0.0, 10.0], &serde_json::json!({"category": "art"}), tracked);

        let filters = vec![BiasFilter {
            field: "category".to_string(),
            kind: BiasKind::Category("science".to_string()),
        }];

        // global centroid = (5, 5), science centroid = (10, 0)
        // delta = (5, -5)
        // bias = query + 0.5 * (5, -5)
        let query = vec![0.0, 0.0];
        let biased = apply_topological_bias(&query, &filters, &centroids, &correlations, &buckets, 0.5);
        assert!((biased[0] - 2.5).abs() < 1e-10);
        assert!((biased[1] - (-2.5)).abs() < 1e-10);
    }

    #[test]
    fn bias_numerical_shifts_by_correlation() {
        let centroids = CentroidTracker::new(2);
        let mut correlations = CorrelationTracker::new(2);
        let buckets = NumericalBucketTracker::new(2);

        // Build correlation: price ~ dim0 (positive), price ~ dim1 (negative)
        for i in 1..=10 {
            let x = i as f64;
            correlations.update("price", x, &vec![x, -x + 10.0]);
        }

        // No buckets registered for "price" → falls back to correlation
        let filters = vec![BiasFilter {
            field: "price".to_string(),
            kind: BiasKind::Numerical { value: 100.0, op: RangeOp::Eq }, // far above mean
        }];

        // mean price ≈ 5.5, diff ≈ 94.5
        // corr[0] ≈ 1.0, corr[1] ≈ -1.0
        // bias[0] = 0.3 * 1.0 * 94.5 ≈ 28.35
        // bias[1] = 0.3 * (-1.0) * 94.5 ≈ -28.35
        let query = vec![0.0, 0.0];
        let biased = apply_topological_bias(&query, &filters, &centroids, &correlations, &buckets, 0.3);

        // dim0 should increase (positive correlation)
        assert!(biased[0] > 20.0, "dim0 should increase, got {}", biased[0]);
        // dim1 should decrease (negative correlation)
        assert!(biased[1] < -20.0, "dim1 should decrease, got {}", biased[1]);
    }

    #[test]
    fn bias_combined_categorical_and_numerical() {
        let mut centroids = CentroidTracker::new(2);
        let mut correlations = CorrelationTracker::new(2);
        let buckets = NumericalBucketTracker::new(2);

        centroids.update(&vec![10.0, 0.0], &serde_json::json!({"category": "science"}), &["category".to_string()]);
        centroids.update(&vec![0.0, 10.0], &serde_json::json!({"category": "art"}), &["category".to_string()]);

        for i in 1..=10 {
            let x = i as f64;
            correlations.update("year", x, &vec![x, 0.0]);
        }

        let filters = vec![
            BiasFilter {
                field: "category".to_string(),
                kind: BiasKind::Category("science".to_string()),
            },
            BiasFilter {
                field: "year".to_string(),
                kind: BiasKind::Numerical { value: 2020.0, op: RangeOp::Gte },
            },
        ];

        let query = vec![0.0, 0.0];
        let biased = apply_topological_bias(&query, &filters, &centroids, &correlations, &buckets, 0.5);

        // Both filters should have contributed
        assert!(biased[0] != 0.0 || biased[1] != 0.0);
    }

    #[test]
    fn bias_alpha_zero_returns_query() {
        let mut centroids = CentroidTracker::new(2);
        centroids.update(&vec![10.0, 0.0], &serde_json::json!({"category": "science"}), &["category".to_string()]);

        let correlations = CorrelationTracker::new(2);
        let buckets = NumericalBucketTracker::new(2);
        let filters = vec![BiasFilter {
            field: "category".to_string(),
            kind: BiasKind::Category("science".to_string()),
        }];

        let query = vec![5.0, 5.0];
        let biased = apply_topological_bias(&query, &filters, &centroids, &correlations, &buckets, 0.0);
        assert_eq!(biased, query);
    }

    // =======================================================================
    // NumericalBucketTracker
    // =======================================================================

    fn make_buckets(dim: usize) -> NumericalBucketTracker {
        let mut bt = NumericalBucketTracker::new(dim);
        bt.register_field("year", vec![2015.0, 2018.0, 2021.0, 2024.0]);
        bt
    }

    #[test]
    fn bucket_new_creates_empty_tracker() {
        let bt = NumericalBucketTracker::new(4);
        assert_eq!(bt.global_sum, vec![0.0; 4]);
        assert_eq!(bt.global_count, 0);
        assert!(bt.fields.is_empty());
    }

    #[test]
    fn bucket_register_field_creates_buckets() {
        let mut bt = NumericalBucketTracker::new(3);
        bt.register_field("year", vec![2015.0, 2018.0, 2021.0, 2024.0]);
        assert!(bt.fields.contains_key("year"));
        let b = bt.fields.get("year").unwrap();
        assert_eq!(b.boundaries.len(), 4);
        assert_eq!(b.sums.len(), 4);
        assert_eq!(b.counts.len(), 4);
        assert_eq!(b.counts, vec![0, 0, 0, 0]);
    }

    #[test]
    fn bucket_has_buckets_true_after_register() {
        let mut bt = NumericalBucketTracker::new(2);
        assert!(!bt.has_buckets("year"));
        bt.register_field("year", vec![2015.0, 2020.0]);
        assert!(bt.has_buckets("year"));
    }

    #[test]
    fn bucket_update_adds_to_correct_bucket() {
        let mut bt = make_buckets(3);
        // year=2020 → bucket 1 (2018..2021)
        bt.update("year", 2020.0, &vec![1.0, 2.0, 3.0]);

        assert_eq!(bt.global_count, 1);
        let b = bt.fields.get("year").unwrap();
        assert_eq!(b.counts[0], 0);
        assert_eq!(b.counts[1], 1);
        assert_eq!(b.counts[2], 0);
        assert_eq!(b.counts[3], 0);
        assert_eq!(b.sums[1], vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn bucket_update_high_value_last_bucket() {
        let mut bt = make_buckets(2);
        // year=2025 → last bucket (>= 2024)
        bt.update("year", 2025.0, &vec![5.0, 5.0]);
        assert_eq!(bt.fields.get("year").unwrap().counts[3], 1);
    }

    #[test]
    fn bucket_update_low_value_first_bucket() {
        let mut bt = make_buckets(2);
        // year=2014 → clamped to first bucket (< 2018)
        bt.update("year", 2014.0, &vec![1.0, 1.0]);
        assert_eq!(bt.fields.get("year").unwrap().counts[0], 1);
    }

    #[test]
    fn bucket_update_unregistered_field_is_noop() {
        let mut bt = NumericalBucketTracker::new(2);
        bt.update("nonexistent", 2020.0, &vec![1.0, 2.0]);
        assert_eq!(bt.global_count, 1); // global is still updated
        assert!(bt.fields.is_empty());
    }

    #[test]
    fn bucket_delta_unknown_field_returns_none() {
        let bt = make_buckets(2);
        assert!(bt.delta("nonexistent", 2020.0, RangeOp::Eq).is_none());
    }

    #[test]
    fn bucket_delta_no_data_returns_none() {
        let bt = make_buckets(2);
        assert!(bt.delta("year", 2020.0, RangeOp::Eq).is_none());
    }

    #[test]
    fn bucket_delta_eq_uses_single_bucket() {
        let mut bt = make_buckets(3);
        // bucket 0 (year < 2018): (0, 0, 0)
        bt.update("year", 2015.0, &vec![0.0, 0.0, 0.0]);
        // bucket 1 (2018..2021): (10, 10, 10) — centroid (10, 10, 10)
        bt.update("year", 2020.0, &vec![10.0, 10.0, 10.0]);
        // bucket 2 (2021..2024): (20, 20, 20)
        bt.update("year", 2023.0, &vec![20.0, 20.0, 20.0]);
        // bucket 3 (>= 2024): (30, 30, 30)
        bt.update("year", 2025.0, &vec![30.0, 30.0, 30.0]);

        // global centroid = (15, 15, 15)
        // delta for Eq(2020) → bucket 1 centroid (10, 10, 10) - global = (-5, -5, -5)
        let d = bt.delta("year", 2020.0, RangeOp::Eq).unwrap();
        assert!((d[0] - (-5.0)).abs() < 1e-10, "d[0]={}", d[0]);
        assert!((d[1] - (-5.0)).abs() < 1e-10, "d[1]={}", d[1]);
    }

    #[test]
    fn bucket_delta_gte_averages_upper_buckets() {
        let mut bt = make_buckets(2);
        // bucket 0: (0, 0)
        bt.update("year", 2015.0, &vec![0.0, 0.0]);
        // bucket 1: (10, 10)
        bt.update("year", 2020.0, &vec![10.0, 10.0]);
        // bucket 2: (20, 20)
        bt.update("year", 2023.0, &vec![20.0, 20.0]);
        // bucket 3: (30, 30)
        bt.update("year", 2025.0, &vec![30.0, 30.0]);

        // global centroid = (15, 15)
        // Gte(2020) → buckets 1, 2, 3
        //   centroid(1)=(10,10), centroid(2)=(20,20), centroid(3)=(30,30)
        //   mean = (20, 20)
        //   delta = (20-15, 20-15) = (5, 5)
        let d = bt.delta("year", 2020.0, RangeOp::Gte).unwrap();
        assert!((d[0] - 5.0).abs() < 1e-10, "d[0]={}", d[0]);
        assert!((d[1] - 5.0).abs() < 1e-10, "d[1]={}", d[1]);
    }

    #[test]
    fn bucket_delta_lte_averages_lower_buckets() {
        let mut bt = make_buckets(2);
        bt.update("year", 2015.0, &vec![0.0, 0.0]);
        bt.update("year", 2020.0, &vec![10.0, 10.0]);
        bt.update("year", 2023.0, &vec![20.0, 20.0]);
        bt.update("year", 2025.0, &vec![30.0, 30.0]);

        // Lte(2020) → buckets 0, 1
        //   centroid(0)=(0,0), centroid(1)=(10,10)
        //   mean = (5, 5)
        //   delta = (5-15, 5-15) = (-10, -10)
        let d = bt.delta("year", 2020.0, RangeOp::Lte).unwrap();
        assert!((d[0] - (-10.0)).abs() < 1e-10, "d[0]={}", d[0]);
        assert!((d[1] - (-10.0)).abs() < 1e-10, "d[1]={}", d[1]);
    }

    #[test]
    fn bucket_delta_range_uses_buckets_in_range() {
        let mut bt = make_buckets(2);
        bt.update("year", 2015.0, &vec![0.0, 0.0]);
        bt.update("year", 2020.0, &vec![10.0, 10.0]);
        bt.update("year", 2023.0, &vec![20.0, 20.0]);
        bt.update("year", 2025.0, &vec![30.0, 30.0]);

        // Range(2019, 2023.9) → buckets 1, 2 (2019 in bucket 1, 2023.9 in bucket 2)
        //   2023.9 < 2024 → bucket 2
        //   mean centroid = ((10+20)/2, (10+20)/2) = (15, 15)
        //   delta = (15-15, 15-15) = (0, 0)
        let d = bt.delta("year", 2020.0, RangeOp::Range(2019.0, 2023.9)).unwrap();
        assert!((d[0] - 0.0).abs() < 1e-10, "d[0]={}", d[0]);
        assert!((d[1] - 0.0).abs() < 1e-10, "d[1]={}", d[1]);
    }

    #[test]
    fn bucket_delta_empty_buckets_returns_none() {
        let bt = make_buckets(2);
        // Register but never update → all counts are 0
        assert!(bt.delta("year", 2020.0, RangeOp::Eq).is_none());
    }

    #[test]
    fn bucket_global_centroid_empty_returns_zeros() {
        let bt = NumericalBucketTracker::new(3);
        assert_eq!(bt.global_centroid(), vec![0.0; 3]);
    }

    #[test]
    fn bucket_global_centroid_correct() {
        let mut bt = NumericalBucketTracker::new(2);
        bt.update("price", 10.0, &vec![1.0, 2.0]);
        bt.update("price", 20.0, &vec![3.0, 4.0]);
        let gc = bt.global_centroid();
        assert!((gc[0] - 2.0).abs() < 1e-10);
        assert!((gc[1] - 3.0).abs() < 1e-10);
    }

    #[test]
    fn bucket_bias_uses_bucket_tracker_when_available() {
        // Test that apply_topological_bias uses bucket tracker
        // when buckets are configured for a field.
        let mut centroids = CentroidTracker::new(2);
        let correlations = CorrelationTracker::new(2);
        let mut buckets = NumericalBucketTracker::new(2);
        buckets.register_field("year", vec![2015.0, 2020.0]);

        // Insert vectors with year correlation
        // bucket 0 (< 2020): low values
        centroids.update(&vec![1.0, 1.0], &serde_json::json!({"year": 2018}), &[]);
        buckets.update("year", 2018.0, &vec![1.0, 1.0]);
        centroids.update(&vec![2.0, 2.0], &serde_json::json!({"year": 2019}), &[]);
        buckets.update("year", 2019.0, &vec![2.0, 2.0]);

        // bucket 1 (>= 2020): high values
        centroids.update(&vec![10.0, 10.0], &serde_json::json!({"year": 2020}), &[]);
        buckets.update("year", 2020.0, &vec![10.0, 10.0]);
        centroids.update(&vec![20.0, 20.0], &serde_json::json!({"year": 2023}), &[]);
        buckets.update("year", 2023.0, &vec![20.0, 20.0]);

        // Gte(2020) → bucket 1 centroid = (15, 15)
        // global centroid = (8.25, 8.25)
        // delta = (15-8.25, 15-8.25) = (6.75, 6.75)
        let filters = vec![BiasFilter {
            field: "year".to_string(),
            kind: BiasKind::Numerical { value: 2020.0, op: RangeOp::Gte },
        }];
        let query = vec![0.0, 0.0];
        let biased = apply_topological_bias(&query, &filters, &centroids, &correlations, &buckets, 1.0);

        // Should be biased by bucket delta (6.75, 6.75)
        assert!((biased[0] - 6.75).abs() < 1e-10, "biased[0]={}", biased[0]);
        assert!((biased[1] - 6.75).abs() < 1e-10, "biased[1]={}", biased[1]);
    }
}
