// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};
use tesseract_common::error::{Error, Result};

/// A sparse mask that pairs dimension indices with scalar weights.
///
/// Only dimensions listed in the mask are modified; all other dimensions
/// retain their original value (equivalent to a weight of 1.0).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WeightMask(pub Vec<(usize, f32)>);

/// A type that can be projected (weighted) by a [`WeightMask`].
pub trait Projection {
    /// Apply `mask` to `self`, producing a new weighted instance.
    fn project(&self, mask: &WeightMask) -> Result<Self>
    where
        Self: Sized;
}

impl Projection for Vec<f64> {
    fn project(&self, mask: &WeightMask) -> Result<Self> {
        for &(idx, _) in &mask.0 {
            if idx >= self.len() {
                return Err(Error::IndexOutOfBounds(idx, self.len()));
            }
        }
        let mut result = self.clone();
        for &(idx, weight) in &mask.0 {
            result[idx] *= weight as f64;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mask_returns_original() {
        let v = vec![1.0, 2.0, 3.0];
        let mask = WeightMask(vec![]);
        let projected = v.project(&mask).unwrap();
        assert_eq!(projected, v);
    }

    #[test]
    fn partial_mask_modifies_specified_dimensions() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        let mask = WeightMask(vec![(0, 2.0), (2, 0.5)]);
        let projected = v.project(&mask).unwrap();
        assert_eq!(projected, vec![2.0, 2.0, 1.5, 4.0]);
    }

    #[test]
    fn zero_weight_produces_zero() {
        let v = vec![5.0, 3.0];
        let mask = WeightMask(vec![(1, 0.0)]);
        let projected = v.project(&mask).unwrap();
        assert_eq!(projected, vec![5.0, 0.0]);
    }

    #[test]
    fn out_of_bounds_index_returns_err() {
        let v = vec![1.0, 2.0];
        let mask = WeightMask(vec![(5, 1.0)]);
        let err = v.project(&mask).unwrap_err();
        assert!(matches!(err, Error::IndexOutOfBounds(5, 2)));
    }
}
