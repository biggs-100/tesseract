// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::collections::HashMap;
use std::sync::RwLock;

use tesseract_common::error::{Error, Result};

use crate::cold_store::PartitionId;

/// A centroid representing a cold partition.
///
/// Stores the element-wise mean as `Vec<f32`> to keep memory usage under
/// 1 KB per entry even at high dimensionality (1536 or more).
#[derive(Debug, Clone)]
pub struct Centroid {
    pub partition_id: PartitionId,
    pub mean: Vec<f32>,
    pub record_count: usize,
}

/// Configuration for the vector skeleton.
#[derive(Debug, Clone)]
pub struct SkeletonConfig {
    /// Distance threshold for partition wake.
    ///
    /// A partition is returned by `find_nearby` when the Euclidean
    /// distance between the query vector and the partition centroid
    /// is strictly less than this value.
    ///
    /// Default: `0.15`.
    pub wake_threshold: f64,
}

impl Default for SkeletonConfig {
    fn default() -> Self {
        Self { wake_threshold: 0.15 }
    }
}

/// In-memory centroid cache for cold partition awakening.
///
/// Each cold partition is represented by a single `Vec<f32`> centroid
/// (the element-wise mean of all vectors in the partition). A query
/// vector is compared against all centroids; partitions whose centroid
/// falls within the configurable `wake_threshold` are returned for
/// loading from the cold tier.
///
/// Thread-safe via `RwLock` (many concurrent readers, rare writers).
pub struct VectorSkeleton {
    centroids: RwLock<HashMap<PartitionId, Centroid>>,
    config: SkeletonConfig,
}

impl VectorSkeleton {
    /// Create a new empty skeleton.
    pub fn new(config: SkeletonConfig) -> Self {
        Self { centroids: RwLock::new(HashMap::new()), config }
    }

    /// Add or replace a partition centroid computed from the given vectors.
    ///
    /// The centroid is the element-wise mean of all vectors, stored as
    /// `Vec<f32`>.
    ///
    /// # Errors
    ///
    /// - Returns `NotFound` if `vectors` is empty.
    /// - Returns `DimensionMismatch` if vectors have inconsistent lengths.
    pub fn add_partition(&self, partition_id: PartitionId, vectors: &[Vec<f64>]) -> Result<()> {
        if vectors.is_empty() {
            return Err(Error::NotFound("cannot compute centroid from empty vector set".into()));
        }

        let dim = vectors[0].len();
        let mut sum = vec![0.0_f64; dim];

        for v in vectors {
            if v.len() != dim {
                return Err(Error::DimensionMismatch(dim, v.len()));
            }
            for (i, &val) in v.iter().enumerate() {
                sum[i] += val;
            }
        }

        let count = vectors.len() as f64;
        let mean: Vec<f32> = sum.into_iter().map(|s| (s / count) as f32).collect();

        let centroid = Centroid { partition_id: partition_id.clone(), mean, record_count: vectors.len() };

        let mut centroids = self.centroids.write().expect("skeleton rwlock poisoned");
        centroids.insert(partition_id, centroid);
        Ok(())
    }

    /// Find partitions whose centroid is close to the query vector.
    ///
    /// Returns partition IDs whose Euclidean distance from the query is
    /// strictly less than `config.wake_threshold`.
    ///
    /// Partitions with mismatched dimensionality are silently skipped.
    pub fn find_nearby(&self, query: &[f64]) -> Vec<PartitionId> {
        let centroids = self.centroids.read().expect("skeleton rwlock poisoned");
        let query_f32: Vec<f32> = query.iter().map(|&v| v as f32).collect();

        centroids
            .values()
            .filter(|c| {
                if c.mean.len() != query_f32.len() {
                    return false;
                }
                let dist = euclidean_distance(&query_f32, &c.mean);
                (dist as f64) < self.config.wake_threshold
            })
            .map(|c| c.partition_id.clone())
            .collect()
    }

    /// Remove a partition's centroid.
    pub fn remove_partition(&self, partition_id: &PartitionId) {
        let mut centroids = self.centroids.write().expect("skeleton rwlock poisoned");
        centroids.remove(partition_id);
    }

    /// Number of centroids currently in the skeleton.
    pub fn len(&self) -> usize {
        let centroids = self.centroids.read().expect("skeleton rwlock poisoned");
        centroids.len()
    }

    /// Returns `true` if the skeleton is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Update a partition centroid incrementally with a new vector batch.
    ///
    /// Uses the formula:
    ///
    /// ```text
    /// new_mean = (old_mean · old_count + ∑new_vectors) / (old_count + new_count)
    /// ```
    ///
    /// # Errors
    ///
    /// - Returns `NotFound` if the partition has no existing centroid.
    /// - Returns `DimensionMismatch` if new vectors have the wrong
    ///   dimensionality.
    pub fn update_centroid(&self, partition_id: &PartitionId, new_vectors: &[Vec<f64>]) -> Result<()> {
        if new_vectors.is_empty() {
            return Ok(());
        }

        let mut centroids = self.centroids.write().expect("skeleton rwlock poisoned");
        let centroid = centroids
            .get_mut(partition_id)
            .ok_or_else(|| Error::NotFound(format!("partition {:?} not found in skeleton", partition_id.0)))?;

        let dim = centroid.mean.len();
        let old_count = centroid.record_count as f64;

        // Verify dimension consistency.
        for v in new_vectors {
            if v.len() != dim {
                return Err(Error::DimensionMismatch(dim, v.len()));
            }
        }

        // Sum new vectors.
        let mut new_sum = vec![0.0_f64; dim];
        for v in new_vectors {
            for (i, &val) in v.iter().enumerate() {
                new_sum[i] += val;
            }
        }

        // new_mean = (old_mean * old_count + sum(new)) / (old_count + new_count)
        let new_count = centroid.record_count + new_vectors.len();
        let new_count_f64 = new_count as f64;

        let mut new_mean = vec![0.0_f32; dim];
        for i in 0..dim {
            let weighted_old = centroid.mean[i] as f64 * old_count;
            new_mean[i] = ((weighted_old + new_sum[i]) / new_count_f64) as f32;
        }

        centroid.mean = new_mean;
        centroid.record_count = new_count;

        Ok(())
    }
}

/// Compute the Euclidean (L2) distance between two f32 vectors.
fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum::<f32>()
        .sqrt()
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skeleton() -> VectorSkeleton {
        VectorSkeleton::new(SkeletonConfig { wake_threshold: 0.5 })
    }

    #[test]
    fn centroid_computed_correctly() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);
        let vectors = vec![vec![1.0, 2.0], vec![3.0, 4.0]];

        skeleton.add_partition(partition.clone(), &vectors).unwrap();

        let centroids = skeleton.centroids.read().unwrap();
        let centroid = centroids.get(&partition).unwrap();
        assert_eq!(centroid.mean, vec![2.0_f32, 3.0_f32]); // element-wise mean
        assert_eq!(centroid.record_count, 2);
    }

    #[test]
    fn find_nearby_returns_close_partition() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);
        skeleton.add_partition(partition.clone(), &[vec![10.0, 20.0]]).unwrap();

        // Query very close to centroid — should match.
        let nearby = skeleton.find_nearby(&[10.1, 20.1]);
        assert!(!nearby.is_empty());
        assert!(nearby.contains(&partition));
    }

    #[test]
    fn find_nearby_returns_empty_for_far_query() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);
        skeleton.add_partition(partition, &[vec![0.0, 0.0]]).unwrap();

        // Query extremely far from centroid.
        let nearby = skeleton.find_nearby(&[1000.0, 1000.0]);
        assert!(nearby.is_empty());
    }

    #[test]
    fn update_centroid_changes_mean_correctly() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);

        skeleton.add_partition(partition.clone(), &[vec![0.0, 0.0]]).unwrap();

        // Add a second vector — mean should shift to midpoint.
        skeleton.update_centroid(&partition, &[vec![10.0, 10.0]]).unwrap();

        let centroids = skeleton.centroids.read().unwrap();
        let centroid = centroids.get(&partition).unwrap();
        assert_eq!(centroid.mean, vec![5.0_f32, 5.0_f32]); // (0+10)/2
        assert_eq!(centroid.record_count, 2);
    }

    #[test]
    fn remove_partition_removes_from_queries() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);

        skeleton.add_partition(partition.clone(), &[vec![1.0, 2.0]]).unwrap();
        assert_eq!(skeleton.len(), 1);

        skeleton.remove_partition(&partition);
        assert_eq!(skeleton.len(), 0);

        let nearby = skeleton.find_nearby(&[1.0, 2.0]);
        assert!(nearby.is_empty());
    }

    #[test]
    fn add_empty_vectors_returns_err() {
        let skeleton = make_skeleton();
        let result = skeleton.add_partition(PartitionId(1), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn dimension_mismatch_returns_err() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);

        skeleton.add_partition(partition.clone(), &[vec![1.0, 2.0]]).unwrap();
        let result = skeleton.update_centroid(&partition, &[vec![1.0, 2.0, 3.0]]);
        assert!(result.is_err());
    }

    #[test]
    fn find_nearby_with_empty_skeleton() {
        let skeleton = make_skeleton();
        assert!(skeleton.is_empty());

        let nearby = skeleton.find_nearby(&[1.0, 2.0]);
        assert!(nearby.is_empty());
    }

    #[test]
    fn multiple_partitions_only_close_ones_returned() {
        let skeleton = make_skeleton(); // threshold = 0.5

        let close_p = PartitionId(1);
        let far_p = PartitionId(2);

        skeleton.add_partition(close_p.clone(), &[vec![10.0, 10.0]]).unwrap();
        skeleton.add_partition(far_p.clone(), &[vec![1000.0, 1000.0]]).unwrap();

        let nearby = skeleton.find_nearby(&[10.05, 10.05]);
        assert_eq!(nearby.len(), 1);
        assert_eq!(nearby[0], close_p);
    }

    #[test]
    fn add_partition_with_mixed_dimensions_errors() {
        let skeleton = make_skeleton();
        let result = skeleton.add_partition(PartitionId(1), &[vec![1.0, 2.0], vec![3.0]]);
        assert!(result.is_err());
    }

    #[test]
    fn update_nonexistent_partition_errors() {
        let skeleton = make_skeleton();
        let result = skeleton.update_centroid(&PartitionId(999), &[vec![1.0, 2.0]]);
        assert!(result.is_err());
    }

    #[test]
    fn multiple_updates_accumulate() {
        let skeleton = make_skeleton();
        let partition = PartitionId(1);

        // Start with centroid at (0, 0), count = 1.
        skeleton.add_partition(partition.clone(), &[vec![0.0, 0.0]]).unwrap();

        // Add (10, 10): new centroid = (5, 5), count = 2.
        skeleton.update_centroid(&partition, &[vec![10.0, 10.0]]).unwrap();

        // Add (20, 20): new centroid = ((5*2 + 20)/3, (5*2 + 20)/3) = (10, 10), count = 3.
        skeleton.update_centroid(&partition, &[vec![20.0, 20.0]]).unwrap();

        let centroids = skeleton.centroids.read().unwrap();
        let centroid = centroids.get(&partition).unwrap();
        assert!((centroid.mean[0] - 10.0_f32).abs() < 1e-6);
        assert!((centroid.mean[1] - 10.0_f32).abs() < 1e-6);
        assert_eq!(centroid.record_count, 3);
    }
}
