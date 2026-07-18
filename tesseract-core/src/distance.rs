// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};
use tesseract_common::error::{Error, Result};

/// A distance measure between two instances of the same type.
pub trait Distance {
    fn distance(&self, other: &Self) -> Result<f64>;
}

/// L2-normalized vector wrapper.
///
/// Construction divides by the L2 norm; panics on zero or non-finite input.
/// The inner `Vec<f64>` is private — all construction goes through `::new()`
/// which enforces normalization.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(try_from = "Vec<f64>")]
pub struct NormalizedVector(Vec<f64>);

impl NormalizedVector {
    /// Build a `NormalizedVector` from raw components, asserting L2
    /// normalization invariants (non-zero, finite norm).
    ///
    /// # Panics
    ///
    /// Panics if the input vector is zero or contains non-finite values (NaN/Inf).
    pub fn new(v: Vec<f64>) -> Self {
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!(norm.is_finite() && norm > 0.0, "NormalizedVector requires a finite, non-zero vector");
        Self(v.into_iter().map(|x| x / norm).collect())
    }
}

/// Custom deserialization — uses `new()` to enforce the invariant.
impl TryFrom<Vec<f64>> for NormalizedVector {
    type Error = String;

    fn try_from(v: Vec<f64>) -> std::result::Result<Self, Self::Error> {
        Ok(Self::new(v))
    }
}

impl std::ops::Deref for NormalizedVector {
    type Target = Vec<f64>;

    fn deref(&self) -> &Vec<f64> {
        &self.0
    }
}

/// Cosine distance on L2-normalized vectors: `1.0 - dot_product(a, b)`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CosineDistance(pub NormalizedVector);

impl Distance for CosineDistance {
    fn distance(&self, other: &Self) -> Result<f64> {
        if self.0.len() != other.0.len() {
            return Err(Error::DimensionMismatch(self.0.len(), other.0.len()));
        }
        let dot: f64 = self.0.iter().zip(other.0.iter()).map(|(a, b)| a * b).sum();
        Ok(1.0 - dot)
    }
}

/// Standard Euclidean distance: `sqrt(sum((a - b)^2))`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EuclideanDistance(pub Vec<f64>);

impl Distance for EuclideanDistance {
    fn distance(&self, other: &Self) -> Result<f64> {
        if self.0.len() != other.0.len() {
            return Err(Error::DimensionMismatch(self.0.len(), other.0.len()));
        }
        let sum_sq: f64 = self.0.iter().zip(&other.0).map(|(a, b)| (a - b).powi(2)).sum();
        Ok(sum_sq.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // NormalizedVector
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_3_4_gives_0_6_0_8() {
        let nv = NormalizedVector::new(vec![3.0, 4.0]);
        assert_eq!(&*nv, &vec![0.6, 0.8]);
    }

    #[test]
    fn normalize_already_unit() {
        let nv = NormalizedVector::new(vec![1.0, 0.0]);
        assert_eq!(&*nv, &vec![1.0, 0.0]);
    }

    #[test]
    #[should_panic(expected = "NormalizedVector requires a finite, non-zero vector")]
    fn zero_vector_panics() {
        let _ = NormalizedVector::new(vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn new_with_single_element() {
        let nv = NormalizedVector::new(vec![5.0]);
        assert!((nv[0] - 1.0).abs() < 1e-15);
    }

    // -----------------------------------------------------------------------
    // CosineDistance
    // -----------------------------------------------------------------------

    #[test]
    fn cosine_identical_vectors() {
        let a = CosineDistance(NormalizedVector::new(vec![3.0, 4.0]));
        let b = CosineDistance(NormalizedVector::new(vec![3.0, 4.0]));
        assert_eq!(a.distance(&b).unwrap(), 0.0);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = CosineDistance(NormalizedVector::new(vec![1.0, 0.0]));
        let b = CosineDistance(NormalizedVector::new(vec![0.0, 1.0]));
        let dist = a.distance(&b).unwrap();
        assert!((dist - 1.0).abs() < 1e-15);
    }

    #[test]
    fn cosine_dimension_mismatch() {
        let a = CosineDistance(NormalizedVector::new(vec![1.0, 0.0]));
        let b = CosineDistance(NormalizedVector::new(vec![1.0]));
        let err = a.distance(&b).unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch(2, 1)));
    }

    // -----------------------------------------------------------------------
    // EuclideanDistance
    // -----------------------------------------------------------------------

    #[test]
    fn euclidean_3_4_5_triangle() {
        let a = EuclideanDistance(vec![0.0, 0.0]);
        let b = EuclideanDistance(vec![3.0, 4.0]);
        let dist = a.distance(&b).unwrap();
        assert!((dist - 5.0).abs() < 1e-15);
    }

    #[test]
    fn euclidean_same_vector() {
        let a = EuclideanDistance(vec![1.0, 2.0, 3.0]);
        let b = EuclideanDistance(vec![1.0, 2.0, 3.0]);
        assert_eq!(a.distance(&b).unwrap(), 0.0);
    }

    #[test]
    fn euclidean_dimension_mismatch() {
        let a = EuclideanDistance(vec![1.0, 2.0, 3.0]);
        let b = EuclideanDistance(vec![1.0, 2.0]);
        let err = a.distance(&b).unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch(3, 2)));
    }
}
