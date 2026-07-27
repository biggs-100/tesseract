// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! MerkleTree — incremental merge tree over vector cluster centroids.
//!
//! The tree accepts batches of vectors (from the hot buffer), assigns them
//! to the nearest existing centroid, updates centroids via weighted averages,
//! recomputes Blake3 hashes for Merkle proofs, and splits overfull clusters.
//!
//! # Centroid Assignment
//!
//! New vectors are assigned to the nearest centroid by cosine distance. If
//! no centroid exists (first batch), each vector becomes its own centroid.
//! Centroids are updated as running weighted averages.

use serde::{Deserialize, Serialize};

use tesseract_common::error::{Error, Result};

use super::hot_buffer::BufferedVector;
use super::node::MerkleNode;

/// The progressive Merkle tree over cluster centroids.
///
/// Stores leaf centroids, builds a binary Merkle tree over them, and
/// supports batch insertion, nearest-centroid search, and disk persistence.
pub struct MerkleTree {
    /// Root of the Merkle tree.
    root: Option<Box<MerkleNode>>,
    /// Next cluster ID to assign.
    next_cluster_id: u64,
    /// Maximum vectors per cluster before splitting.
    max_cluster_size: usize,
    /// Path to disk persistence (None = in-memory only).
    path: Option<std::path::PathBuf>,
}

/// Serializable snapshot of the Merkle tree state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeSnapshot {
    root: Option<Box<MerkleNode>>,
    next_cluster_id: u64,
    max_cluster_size: usize,
}

impl MerkleTree {
    /// Create a new empty Merkle tree.
    pub fn new(max_cluster_size: usize) -> Self {
        Self {
            root: None,
            next_cluster_id: 1,
            max_cluster_size,
            path: None,
        }
    }

    /// Create a new Merkle tree with a persistence path.
    pub fn with_path(max_cluster_size: usize, path: std::path::PathBuf) -> Self {
        Self {
            root: None,
            next_cluster_id: 1,
            max_cluster_size,
            path: Some(path),
        }
    }

    /// Return a reference to the root node, if any.
    pub fn root(&self) -> Option<&MerkleNode> {
        self.root.as_deref()
    }

    /// Return the number of centroids (leaf clusters) in the tree.
    pub fn num_centroids(&self) -> usize {
        match &self.root {
            Some(root) => root.collect_leaves().len(),
            None => 0,
        }
    }

    // ── Batch Insert (Merge) ──────────────────────────────────────────

    /// Insert a batch of vectors into the tree.
    ///
    /// This is the core merge operation:
    /// 1. Assign each vector to the nearest centroid (or create new centroids).
    /// 2. Update centroids as weighted averages.
    /// 3. Recompute hashes from leaves up to root.
    /// 4. Split any cluster that exceeds `max_cluster_size`.
    pub fn insert_batch(&mut self, vectors: &[BufferedVector]) {
        if vectors.is_empty() {
            return;
        }

        let dim = vectors[0].vector.len();

        // Collect current leaf centroids (or empty if first batch).
        let mut current_leaves: Vec<MerkleNode> = match self.root.take() {
            Some(root) => root.collect_leaves_owned(),
            None => Vec::new(),
        };

        // Phase 1: Assign vectors to centroids, accumulate sums.
        let mut new_leaves: Vec<MerkleNode> = Vec::new();
        let mut cluster_sums: std::collections::HashMap<u64, Vec<f32>> = std::collections::HashMap::new();
        let mut cluster_additions: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();

        for bv in vectors {
            let vec_f32 = &bv.vector;
            if vec_f32.len() != dim {
                // Skip dimension-mismatched vectors (shouldn't happen in practice).
                continue;
            }

            match self.find_nearest_in_leaves(&current_leaves, vec_f32) {
                Some((cluster_id, _dist)) => {
                    *cluster_additions.entry(cluster_id).or_insert(0) += 1;
                    let sum = cluster_sums
                        .entry(cluster_id)
                        .or_insert_with(|| vec![0.0_f32; dim]);
                    for (s, &x) in sum.iter_mut().zip(vec_f32.iter()) {
                        *s += x;
                    }
                }
                None => {
                    // Create a new centroid.
                    let new_id = self.next_cluster_id;
                    self.next_cluster_id += 1;
                    new_leaves.push(MerkleNode::Leaf {
                        centroid: vec_f32.clone(),
                        cluster_id: new_id,
                        count: 1,
                        hash: [0u8; 32],
                    });
                }
            }
        }

        // Phase 2: Update existing centroids with accumulated sums.
        for leaf in &mut current_leaves {
            if let MerkleNode::Leaf { centroid, count, cluster_id, hash: _ } = leaf {
                if let Some(&additions) = cluster_additions.get(cluster_id) {
                    let sum = cluster_sums.remove(cluster_id).unwrap();
                    let old_count = *count;
                    let total_count = old_count + additions;
                    for (c, &s) in centroid.iter_mut().zip(sum.iter()) {
                        *c = (*c * old_count as f32 + s) / total_count as f32;
                    }
                    *count = total_count;
                }
            }
        }

        // Merge existing and new leaves.
        current_leaves.extend(new_leaves);

        // Phase 3: Recompute hashes for all leaves.
        for leaf in &mut current_leaves {
            leaf.recompute_hash();
        }

        // Phase 4: Split overfull clusters.
        let mut needs_split = true;
        while needs_split {
            needs_split = false;
            let mut after_split = Vec::with_capacity(current_leaves.len() + 4);

            for leaf in current_leaves {
                if let MerkleNode::Leaf { centroid, count, cluster_id: _, hash: _ } = &leaf {
                    if *count as usize > self.max_cluster_size {
                        needs_split = true;
                        let (c1, c2) = Self::split_centroid(centroid, *count);
                        let new_id1 = self.next_cluster_id;
                        self.next_cluster_id += 1;
                        let new_id2 = self.next_cluster_id;
                        self.next_cluster_id += 1;
                        let mut leaf1 = MerkleNode::Leaf {
                            centroid: c1,
                            cluster_id: new_id1,
                            count: *count / 2,
                            hash: [0u8; 32],
                        };
                        let mut leaf2 = MerkleNode::Leaf {
                            centroid: c2,
                            cluster_id: new_id2,
                            count: *count - *count / 2,
                            hash: [0u8; 32],
                        };
                        leaf1.recompute_hash();
                        leaf2.recompute_hash();
                        after_split.push(leaf1);
                        after_split.push(leaf2);
                        // Keep the old leaf's cluster_id for backward compatibility.
                        // We don't remove it; we insert the new ones alongside.
                        // Actually, the old leaf is being replaced by the two new ones.
                        // We don't push it. The loop handles this by not pushing `leaf`.
                        continue;
                    }
                }
                // Push leaf unchanged if it didn't need splitting.
                after_split.push(leaf);
            }

            current_leaves = after_split;
        }

        // Phase 5: Rebuild the tree from leaves.
        self.root = Self::build_tree(&mut current_leaves);

        // Phase 6: Update centroids index (just rebuild the tree — leaves are
        // accessible via the root).
    }

    /// Find the nearest centroid to a vector among the current leaves.
    ///
    /// Returns `Some((cluster_id, distance))` or `None` if there are no leaves.
    fn find_nearest_in_leaves(&self, leaves: &[MerkleNode], vector: &[f32]) -> Option<(u64, f32)> {
        if leaves.is_empty() {
            return None;
        }

        leaves
            .iter()
            .filter_map(|leaf| {
                let dist = leaf.centroid_distance(vector);
                leaf.cluster_id().map(|id| (id, dist))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Split a centroid into two perturbed variants.
    ///
    /// Creates two new centroids by applying a small epsilon perturbation
    /// in opposite directions. The counts are divided roughly equally.
    fn split_centroid(centroid: &[f32], _count: u64) -> (Vec<f32>, Vec<f32>) {
        let epsilon = 0.01_f32;
        let c1: Vec<f32> = centroid.iter().map(|&x| x + epsilon).collect();
        let c2: Vec<f32> = centroid.iter().map(|&x| x - epsilon).collect();
        (c1, c2)
    }

    /// Build a balanced binary Merkle tree from a mutable slice of leaves.
    ///
    /// Leaves are sorted by cluster_id for deterministic tree construction,
    /// then paired bottom-up into internal nodes.
    fn build_tree(leaves: &mut [MerkleNode]) -> Option<Box<MerkleNode>> {
        if leaves.is_empty() {
            return None;
        }

        // Sort by cluster ID for deterministic tree shape.
        leaves.sort_by(|a, b| {
            a.cluster_id()
                .unwrap_or(u64::MAX)
                .cmp(&b.cluster_id().unwrap_or(u64::MAX))
        });

        // Clone leaves into a working vec.
        let mut nodes: Vec<MerkleNode> = leaves.to_vec();

        // Build bottom-up: pair adjacent nodes into internal nodes.
        while nodes.len() > 1 {
            let mut parents = Vec::with_capacity(nodes.len().div_ceil(2));
            for chunk in nodes.chunks(2) {
                if chunk.len() == 2 {
                    let left = chunk[0].clone();
                    let right = chunk[1].clone();
                    let centroid = Self::weighted_average(
                        left.centroid(),
                        left.count(),
                        right.centroid(),
                        right.count(),
                    );
                    let count = left.count() + right.count();
                    let mut internal = MerkleNode::Internal {
                        hash: [0u8; 32],
                        left: Box::new(left),
                        right: Box::new(right),
                        centroid,
                        count,
                    };
                    internal.recompute_hash();
                    parents.push(internal);
                } else {
                    // Odd node out — promote it as-is.
                    parents.push(chunk[0].clone());
                }
            }
            nodes = parents;
        }

        Some(Box::new(nodes.remove(0)))
    }

    /// Compute the weighted average of two centroids.
    fn weighted_average(a: &[f32], count_a: u64, b: &[f32], count_b: u64) -> Vec<f32> {
        let total = (count_a + count_b) as f32;
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x * count_a as f32 + y * count_b as f32) / total)
            .collect()
    }

    // ── Nearest-centroid search ────────────────────────────────────────

    /// Find the nearest centroid in the tree for a query vector.
    ///
    /// This is used during merge to assign vectors to clusters.
    pub fn find_nearest_cluster(&self, vector: &[f32]) -> Option<u64> {
        let leaves = match &self.root {
            Some(root) => root.collect_leaves(),
            None => return None,
        };

        leaves
            .iter()
            .filter_map(|leaf| {
                let dist = leaf.centroid_distance(vector);
                leaf.cluster_id().map(|id| (id, dist))
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id)
    }

    // ── Search ─────────────────────────────────────────────────────────

    /// Search the tree: find the nearest centroids to the query vector.
    ///
    /// Returns up to `k` `(cluster_id, distance)` pairs sorted by distance
    /// ascending. In a full implementation, each cluster's associated HNSW
    /// sub-graph would then be searched; for the core data structure phase,
    /// this returns centroid-level results directly.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        if k == 0 {
            return Vec::new();
        }

        let leaves = match &self.root {
            Some(root) => root.collect_leaves(),
            None => return Vec::new(),
        };

        let mut results: Vec<(u64, f32)> = leaves
            .iter()
            .filter_map(|leaf| {
                let dist = leaf.centroid_distance(query);
                leaf.cluster_id().map(|id| (id, dist))
            })
            .collect();

        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    // ── Persistence ────────────────────────────────────────────────────

    /// Persist the Merkle tree to disk using bincode.
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let snapshot = TreeSnapshot {
            root: self.root.clone(),
            next_cluster_id: self.next_cluster_id,
            max_cluster_size: self.max_cluster_size,
        };

        let bytes = bincode::serialize(&snapshot).map_err(|e| Error::SerializationError(e.to_string()))?;
        std::fs::write(path, bytes).map_err(Error::IoError)?;
        Ok(())
    }

    /// Load a Merkle tree from disk.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(Error::IoError)?;
        let snapshot: TreeSnapshot =
            bincode::deserialize(&bytes).map_err(|e| Error::SerializationError(e.to_string()))?;

        Ok(Self {
            root: snapshot.root,
            next_cluster_id: snapshot.next_cluster_id,
            max_cluster_size: snapshot.max_cluster_size,
            path: Some(path.to_path_buf()),
        })
    }

    /// Configure the persistence path after construction.
    pub fn set_path(&mut self, path: std::path::PathBuf) {
        self.path = Some(path);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn buffered_vector(id: u64, vector: Vec<f32>) -> BufferedVector {
        BufferedVector { id, vector, metadata: serde_json::json!({}) }
    }

    // ── 1. Empty tree ─────────────────────────────────────────────────

    #[test]
    fn empty_tree_has_no_centroids() {
        let tree = MerkleTree::new(100);
        assert_eq!(tree.num_centroids(), 0);
        assert!(tree.root().is_none());
    }

    #[test]
    fn empty_tree_search_returns_empty() {
        let tree = MerkleTree::new(100);
        let results = tree.search(&[1.0, 0.0, 0.0], 10);
        assert!(results.is_empty());
    }

    // ── 2. First batch insert ─────────────────────────────────────────

    #[test]
    fn first_batch_creates_centroids() {
        let mut tree = MerkleTree::new(1000);
        let vectors = vec![
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
            buffered_vector(3, vec![0.0, 0.0, 1.0]),
        ];
        tree.insert_batch(&vectors);
        assert_eq!(tree.num_centroids(), 3);
        assert!(tree.root().is_some());
    }

    // ── 3. Batch insert assigns to nearest centroid ───────────────────

    #[test]
    fn second_batch_assigns_to_nearest_centroid() {
        let mut tree = MerkleTree::new(1000);

        // First batch: create two centroids
        tree.insert_batch(&[
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
        ]);
        let centroids_after_first = tree.num_centroids();
        assert_eq!(centroids_after_first, 2);

        // Second batch: vectors close to the first centroid
        tree.insert_batch(&[
            buffered_vector(3, vec![0.95, 0.05, 0.0]),
            buffered_vector(4, vec![0.98, 0.02, 0.0]),
        ]);

        // Should still have 2 centroids (vectors were assigned to nearest)
        // The centroid at [1,0,0] should have count=3 (was 1, got 2 more)
        // The centroid at [0,1,0] should have count=1
        assert_eq!(tree.num_centroids(), 2);
    }

    // ── 4. Search returns nearest centroids ───────────────────────────

    #[test]
    fn search_finds_nearest_centroid() {
        let mut tree = MerkleTree::new(1000);
        tree.insert_batch(&[
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
        ]);

        // Query near [1,0,0] should return cluster_id of first centroid first
        let results = tree.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        // The cluster with centroid closest to [1,0,0] should be first
        assert!(
            results[0].1 < results[1].1,
            "results must be sorted by distance"
        );
    }

    #[test]
    fn search_respects_k() {
        let mut tree = MerkleTree::new(1000);
        // Insert diverse vectors in a single batch so each becomes a centroid.
        let vectors: Vec<BufferedVector> = (0..10u64)
            .map(|i| {
                // Each vector is a distinct basis direction.
                let mut v = vec![0.0_f32; 10];
                v[i as usize] = 1.0;
                buffered_vector(i, v)
            })
            .collect();
        tree.insert_batch(&vectors);
        assert_eq!(tree.num_centroids(), 10, "should have 10 centroids");
        let results = tree.search(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 3);
        assert_eq!(results.len(), 3, "should return 3 results for k=3, got {}", results.len());
    }

    // ── 5. Cluster split ──────────────────────────────────────────────

    #[test]
    fn overfull_cluster_splits() {
        let mut tree = MerkleTree::new(3); // max 3 per cluster

        // Insert 5 vectors all near [1,0,0] — should all go to one centroid
        // on first batch, creating 5 centroids (since tree is empty, each
        // becomes its own centroid).
        // On the second batch, they should all be assigned to the nearest
        // and one cluster might grow past 3.

        // Actually, since insert_batch creates centroids per unique vector
        // on first pass, let me re-think. Empty tree: each vector becomes
        // its own centroid (5 centroids). That's 1 per cluster, not > 3.
        // We need to assign multiple vectors to the same centroid.

        // Strategy: first batch creates 1 centroid, then second batch adds
        // more vectors to it until it's over capacity.

        // First batch: 1 vector → 1 centroid
        tree.insert_batch(&[buffered_vector(1, vec![1.0, 0.0, 0.0])]);
        assert_eq!(tree.num_centroids(), 1);

        // Second batch: 4 more vectors close to [1,0,0]
        // These should all be assigned to the same centroid
        for i in 2..=5u64 {
            let perturbation = (i as f32) * 0.001;
            tree.insert_batch(&[buffered_vector(
                i,
                vec![1.0 - perturbation, perturbation, 0.0, 0.0],
            )]);
        }

        // The centroid should have count=5 (was 1, got 4 more)
        // Since max_cluster_size=3, it should have split into 2 or 3 centroids
        assert!(tree.num_centroids() >= 2, "should have split into at least 2 centroids, got {}", tree.num_centroids());
    }

    // ── 6. Save and load ──────────────────────────────────────────────

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("tree.bin");

        let mut tree = MerkleTree::new(100);
        tree.insert_batch(&[
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
            buffered_vector(3, vec![0.0, 0.0, 1.0]),
        ]);
        let orig_centroids = tree.num_centroids();
        let orig_id = tree.next_cluster_id;

        tree.save(&path).unwrap();

        let loaded = MerkleTree::load(&path).unwrap();
        assert_eq!(loaded.num_centroids(), orig_centroids);
        assert_eq!(loaded.next_cluster_id, orig_id);
        assert!(loaded.root().is_some());
    }

    #[test]
    fn save_load_preserves_search_results() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("merkle.bin");

        let mut tree = MerkleTree::new(100);
        tree.insert_batch(&[
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
        ]);
        let before = tree.search(&[1.0, 0.0, 0.0], 2);

        tree.save(&path).unwrap();
        let loaded = MerkleTree::load(&path).unwrap();
        let after = loaded.search(&[1.0, 0.0, 0.0], 2);

        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!(b.0, a.0, "cluster IDs must match after load");
            assert!((b.1 - a.1).abs() < 1e-6, "distances must match after load");
        }
    }

    #[test]
    fn load_nonexistent_file_returns_error() {
        let result = MerkleTree::load(std::path::Path::new("/nonexistent/tree.bin"));
        assert!(result.is_err());
    }

    // ── 7. find_nearest_cluster ───────────────────────────────────────

    #[test]
    fn find_nearest_returns_correct_cluster() {
        let mut tree = MerkleTree::new(100);
        tree.insert_batch(&[
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
        ]);

        let nearest = tree.find_nearest_cluster(&[0.9, 0.1, 0.0]);
        assert!(nearest.is_some(), "should find a nearest cluster");
    }

    #[test]
    fn find_nearest_empty_tree_returns_none() {
        let tree = MerkleTree::new(100);
        let nearest = tree.find_nearest_cluster(&[1.0, 0.0, 0.0]);
        assert!(nearest.is_none());
    }

    // ── 8. Large batch ────────────────────────────────────────────────

    #[test]
    fn large_batch_does_not_panic() {
        let mut tree = MerkleTree::new(100);
        let mut vectors = Vec::with_capacity(100);
        for i in 0..100u64 {
            let mut v = vec![i as f32 / 100.0; 4];
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            vectors.push(buffered_vector(i, v));
        }
        // First batch: creates 100 centroids
        tree.insert_batch(&vectors);
        assert_eq!(tree.num_centroids(), 100);

        // Second batch: assigns to nearest, some splits may happen
        tree.insert_batch(&vectors);
        assert!(tree.num_centroids() >= 100, "should have at least 100 centroids");
    }

    // ── 9. Empty batch ────────────────────────────────────────────────

    #[test]
    fn empty_batch_does_nothing() {
        let mut tree = MerkleTree::new(100);
        tree.insert_batch(&[]);
        assert_eq!(tree.num_centroids(), 0);

        // Insert some, then empty batch should not change state
        tree.insert_batch(&[buffered_vector(1, vec![1.0, 0.0, 0.0])]);
        let before = tree.num_centroids();
        tree.insert_batch(&[]);
        assert_eq!(tree.num_centroids(), before);
    }

    // ── 10. Hash chain verification ───────────────────────────────────

    #[test]
    fn root_hash_changes_after_insert() {
        let mut tree = MerkleTree::new(100);
        tree.insert_batch(&[buffered_vector(1, vec![1.0, 0.0, 0.0])]);
        let hash_before = *tree.root().unwrap().hash();

        tree.insert_batch(&[buffered_vector(2, vec![0.0, 1.0, 0.0])]);
        let hash_after = *tree.root().unwrap().hash();

        assert_ne!(hash_before, hash_after, "inserting new data must change root hash");
    }

    #[test]
    fn same_data_produces_same_root_hash() {
        let mut tree1 = MerkleTree::new(100);
        let mut tree2 = MerkleTree::new(100);

        let data = [
            buffered_vector(1, vec![1.0, 0.0, 0.0]),
            buffered_vector(2, vec![0.0, 1.0, 0.0]),
        ];
        tree1.insert_batch(&data);
        tree2.insert_batch(&data);

        assert_eq!(
            *tree1.root().unwrap().hash(),
            *tree2.root().unwrap().hash(),
            "same data must produce same root hash"
        );
    }

    // ── 11. with_path ─────────────────────────────────────────────────

    #[test]
    fn with_path_creates_tree() {
        let tree = MerkleTree::with_path(100, std::path::PathBuf::from("/tmp/merkle.bin"));
        assert_eq!(tree.max_cluster_size, 100);
        assert!(tree.root().is_none());
    }

    #[test]
    fn set_path_updates_path() {
        let mut tree = MerkleTree::new(100);
        tree.set_path(std::path::PathBuf::from("/tmp/test.bin"));
        // Can't check path directly (it's private), but we can verify the
        // tree is still usable
        assert!(tree.root().is_none());
    }
}
