// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};
use tesseract_core::projection::WeightMask;

/// A distance computation strategy optimized for the index hot path.
/// Operates on `f32` slices (not `f64`) for SIMD-friendliness.
pub trait DistanceComputer: Send + Sync + Clone {
    /// Standard distance between two vectors.
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;

    /// Weighted distance: applies a dense weight mask during computation.
    /// `weights.len() == a.len() == b.len()`.
    fn distance_weighted(&self, a: &[f32], b: &[f32], weights: &[f32]) -> f32;
}

/// Cosine distance: `1.0 - dot_product(a, b)`.
///
/// Assumes vectors are L2-normalized.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CosineComputer;

impl DistanceComputer for CosineComputer {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        1.0 - dot
    }

    fn distance_weighted(&self, a: &[f32], b: &[f32], weights: &[f32]) -> f32 {
        // Fused: Σ(wi * ai * wi * bi) = Σ(wi² * ai * bi), then 1.0 - result
        let weighted_dot: f32 = a.iter().zip(b.iter()).zip(weights.iter()).map(|((x, y), w)| w * w * x * y).sum();
        1.0 - weighted_dot
    }
}

/// Euclidean distance: `sqrt(Σ((a - b)²))`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EuclideanComputer;

impl DistanceComputer for EuclideanComputer {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let sum_sq: f32 = a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum();
        sum_sq.sqrt()
    }

    fn distance_weighted(&self, a: &[f32], b: &[f32], weights: &[f32]) -> f32 {
        // Fused: Σ(wi² × (ai - bi)²)
        let weighted_sum_sq: f32 =
            a.iter().zip(b.iter()).zip(weights.iter()).map(|((x, y), w)| w * w * (x - y).powi(2)).sum();
        weighted_sum_sq.sqrt()
    }
}

/// Convert a sparse [`WeightMask`] to a dense weight vector.
///
/// Dimensions not present in the mask get weight `1.0` (no modification).
pub fn mask_to_dense(mask: &WeightMask, dim: usize) -> Vec<f32> {
    let mut dense = vec![1.0f32; dim];
    for &(idx, weight) in &mask.0 {
        if idx < dim {
            dense[idx] = weight;
        }
    }
    dense
}

/// Convert an `f64` slice to `f32`.
#[inline]
pub fn f64_slice_to_f32(v: &[f64]) -> Vec<f32> {
    v.iter().map(|&x| x as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cosine ──────────────────────────────────────────────────────────────

    #[test]
    fn cosine_same_vector() {
        let c = CosineComputer;
        let v = vec![0.6f32, 0.8f32]; // normalized
        let d = c.distance(&v, &v);
        assert!((d - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        let c = CosineComputer;
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let d = c.distance(&a, &b);
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let c = CosineComputer;
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let d = c.distance(&a, &b);
        // dot = -1, so distance = 1.0 - (-1) = 2.0
        assert!((d - 2.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_weighted_zeros() {
        let c = CosineComputer;
        let a = vec![0.6, 0.8];
        let b = vec![0.6, 0.8];
        let weights = vec![0.0, 1.0]; // zero out first dimension
        let d = c.distance_weighted(&a, &b, &weights);
        // After weighting: a' = [0, 0.8], b' = [0, 0.8], dot = 0.64
        assert!((d - (1.0 - 0.64)).abs() < 1e-6);
    }

    #[test]
    fn cosine_weighted_identity() {
        let c = CosineComputer;
        let a = vec![0.6, 0.8];
        let b = vec![0.6, 0.8];
        let weights = vec![1.0, 1.0]; // no modification
        let d_weighted = c.distance_weighted(&a, &b, &weights);
        let d = c.distance(&a, &b);
        assert!((d_weighted - d).abs() < 1e-6);
    }

    #[test]
    fn cosine_vs_brute_force_f64() {
        // Compare f32 cosine vs f64 brute-force computation.
        let a32 = vec![0.3f32, 0.4f32, 0.5f32, 0.6f32, 0.7f32];
        let b32 = vec![0.9f32, 0.8f32, 0.7f32, 0.6f32, 0.5f32];

        // f32 cosine
        let c = CosineComputer;
        let d32 = c.distance(&a32, &b32);

        // f64 brute-force
        let a64: Vec<f64> = a32.iter().map(|&x| x as f64).collect();
        let b64: Vec<f64> = b32.iter().map(|&x| x as f64).collect();
        let dot_f64: f64 = a64.iter().zip(&b64).map(|(x, y)| x * y).sum();
        let d64 = 1.0 - dot_f64;

        // Should be close (within f32 precision)
        assert!((d32 as f64 - d64).abs() < 1e-6);
    }

    // ── Euclidean ───────────────────────────────────────────────────────────

    #[test]
    fn euclidean_3_4_5() {
        let e = EuclideanComputer;
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let d = e.distance(&a, &b);
        assert!((d - 5.0).abs() < 1e-6);
    }

    #[test]
    fn euclidean_zero() {
        let e = EuclideanComputer;
        let a = vec![1.0, 2.0, 3.0];
        let d = e.distance(&a, &a);
        assert!((d - 0.0).abs() < 1e-6);
    }

    #[test]
    fn euclidean_weighted() {
        let e = EuclideanComputer;
        let a = vec![0.0, 0.0];
        let b = vec![2.0, 0.0];
        let weights = vec![0.5, 1.0]; // halve first dimension difference
        let d = e.distance_weighted(&a, &b, &weights);
        // After: sqrt((0.5² × 2²) + (1² × 0²)) = sqrt(0.25 × 4) = sqrt(1) = 1.0
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn euclidean_weighted_identity() {
        let e = EuclideanComputer;
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let weights = vec![1.0, 1.0, 1.0];
        let d_weighted = e.distance_weighted(&a, &b, &weights);
        let d = e.distance(&a, &b);
        assert!((d_weighted - d).abs() < 1e-6);
    }

    #[test]
    fn euclidean_vs_brute_force_f64() {
        let a32 = vec![0.3f32, 0.4f32, 0.5f32, 0.6f32, 0.7f32];
        let b32 = vec![0.9f32, 0.8f32, 0.7f32, 0.6f32, 0.5f32];

        let e = EuclideanComputer;
        let d32 = e.distance(&a32, &b32);

        let a64: Vec<f64> = a32.iter().map(|&x| x as f64).collect();
        let b64: Vec<f64> = b32.iter().map(|&x| x as f64).collect();
        let sum_sq_f64: f64 = a64.iter().zip(&b64).map(|(x, y)| (x - y).powi(2)).sum();
        let d64 = sum_sq_f64.sqrt();

        assert!((d32 as f64 - d64).abs() < 1e-6);
    }

    // ── mask_to_dense ───────────────────────────────────────────────────────

    #[test]
    fn mask_to_dense_conversion() {
        let mask = WeightMask(vec![(0, 0.5), (2, 2.0)]);
        let dense = mask_to_dense(&mask, 4);
        assert_eq!(dense, vec![0.5, 1.0, 2.0, 1.0]);
    }

    #[test]
    fn mask_to_dense_empty_mask() {
        let mask = WeightMask(vec![]);
        let dense = mask_to_dense(&mask, 3);
        assert_eq!(dense, vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn mask_to_dense_index_out_of_bounds_ignored() {
        let mask = WeightMask(vec![(10, 0.0)]);
        let dense = mask_to_dense(&mask, 4);
        assert_eq!(dense, vec![1.0, 1.0, 1.0, 1.0]);
    }

    // ── f64_slice_to_f32 ────────────────────────────────────────────────────

    #[test]
    fn f64_to_f32_conversion() {
        let src = vec![0.5_f64, 1.0, 1.5];
        let dst = f64_slice_to_f32(&src);
        assert_eq!(dst, vec![0.5_f32, 1.0, 1.5]);
    }

    // ── Trait bounds ────────────────────────────────────────────────────────

    #[test]
    fn computers_are_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<CosineComputer>();
        assert_sync::<CosineComputer>();
        assert_send::<EuclideanComputer>();
        assert_sync::<EuclideanComputer>();
    }

    #[test]
    fn computers_are_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<CosineComputer>();
        assert_clone::<EuclideanComputer>();
    }
}
