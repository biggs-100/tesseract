// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! MerkleNode — the fundamental node type in the progressive Merkle tree.
//!
//! Leaf nodes represent vector cluster centroids. Internal nodes aggregate
//! children via weighted average centroids and concatenated Blake3 hashes.

use serde::{Deserialize, Serialize};

/// A node in the progressive Merkle tree.
///
/// # Variants
///
/// * `Leaf` — A cluster centroid. Stores the aggregated centroid,
///   cluster identifier, vector count, and a Blake3 hash of its content.
/// * `Internal` — Aggregates two child subtrees. Stores a combined
///   centroid (weighted average of children), total count, and a hash
///   of the concatenation of child hashes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MerkleNode {
    /// Leaf node: represents a cluster of vectors.
    Leaf {
        /// The aggregated centroid of this cluster.
        centroid: Vec<f32>,
        /// Unique cluster identifier.
        cluster_id: u64,
        /// Number of vectors in this cluster.
        count: u64,
        /// Blake3 hash of this node's content.
        hash: [u8; 32],
    },
    /// Internal node: aggregates children.
    Internal {
        /// Blake3 hash of children (concatenated).
        hash: [u8; 32],
        /// Left child.
        left: Box<MerkleNode>,
        /// Right child.
        right: Box<MerkleNode>,
        /// Aggregated centroid of this subtree.
        centroid: Vec<f32>,
        /// Total vectors in this subtree.
        count: u64,
    },
}

impl MerkleNode {
    /// Compute the hash of this node.
    ///
    /// For leaves: `blake3(centroid_bytes ‖ count_bytes ‖ cluster_id_bytes)`.
    /// For internals: `blake3(left.hash ‖ right.hash)`.
    pub fn recompute_hash(&mut self) {
        match self {
            MerkleNode::Leaf { centroid, count, cluster_id, hash } => {
                let mut hasher = blake3::Hasher::new();
                // Feed centroid bytes in native f32 representation
                for &x in centroid.iter() {
                    hasher.update(&x.to_le_bytes());
                }
                hasher.update(&count.to_le_bytes());
                hasher.update(&cluster_id.to_le_bytes());
                *hash = hasher.finalize().into();
            }
            MerkleNode::Internal { left, right, hash, .. } => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(left.hash());
                hasher.update(right.hash());
                *hash = hasher.finalize().into();
            }
        }
    }

    /// Distance from a query vector to this node's centroid.
    ///
    /// Uses cosine distance: `1.0 - cos(θ) = 1.0 - dot(a,b) / (|a| * |b|)`.
    /// Handles unnormalized centroids (after weighted averaging) by computing
    /// the full cosine similarity with normalization.
    pub fn centroid_distance(&self, query: &[f32]) -> f32 {
        let centroid = self.centroid();
        let dot: f32 = query.iter().zip(centroid.iter()).map(|(a, b)| a * b).sum();
        let query_norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let centroid_norm: f32 = centroid.iter().map(|x| x * x).sum::<f32>().sqrt();
        if query_norm == 0.0 || centroid_norm == 0.0 {
            return 1.0;
        }
        1.0 - dot / (query_norm * centroid_norm)
    }

    // ── Helper accessors ──────────────────────────────────────────────

    /// Return the centroid slice regardless of variant.
    pub fn centroid(&self) -> &[f32] {
        match self {
            MerkleNode::Leaf { centroid, .. } => centroid,
            MerkleNode::Internal { centroid, .. } => centroid,
        }
    }

    /// Return the hash regardless of variant.
    pub fn hash(&self) -> &[u8; 32] {
        match self {
            MerkleNode::Leaf { hash, .. } => hash,
            MerkleNode::Internal { hash, .. } => hash,
        }
    }

    /// Return the vector count regardless of variant.
    pub fn count(&self) -> u64 {
        match self {
            MerkleNode::Leaf { count, .. } => *count,
            MerkleNode::Internal { count, .. } => *count,
        }
    }

    /// Return the cluster ID if this is a leaf node.
    pub fn cluster_id(&self) -> Option<u64> {
        match self {
            MerkleNode::Leaf { cluster_id, .. } => Some(*cluster_id),
            MerkleNode::Internal { .. } => None,
        }
    }

    /// Collect all leaf nodes from this subtree.
    pub fn collect_leaves(&self) -> Vec<&MerkleNode> {
        let mut leaves = Vec::new();
        self.collect_leaves_into(&mut leaves);
        leaves
    }

    fn collect_leaves_into<'a>(&'a self, leaves: &mut Vec<&'a MerkleNode>) {
        match self {
            MerkleNode::Leaf { .. } => leaves.push(self),
            MerkleNode::Internal { left, right, .. } => {
                left.collect_leaves_into(leaves);
                right.collect_leaves_into(leaves);
            }
        }
    }

    /// Collect owned leaf nodes from this subtree (for rebuilding).
    pub fn collect_leaves_owned(self) -> Vec<MerkleNode> {
        let mut leaves = Vec::new();
        self.collect_leaves_owned_into(&mut leaves);
        leaves
    }

    fn collect_leaves_owned_into(self, leaves: &mut Vec<MerkleNode>) {
        match self {
            MerkleNode::Leaf { .. } => leaves.push(self),
            MerkleNode::Internal { left, right, .. } => {
                left.collect_leaves_owned_into(leaves);
                right.collect_leaves_owned_into(leaves);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_leaf(centroid: Vec<f32>, cluster_id: u64, count: u64) -> MerkleNode {
        MerkleNode::Leaf { centroid, cluster_id, count, hash: [0u8; 32] }
    }

    // ── 1. Hash computation ───────────────────────────────────────────

    #[test]
    fn leaf_recompute_hash_produces_deterministic_result() {
        let mut leaf = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        leaf.recompute_hash();
        let hash1 = *leaf.hash();

        // Same content → same hash
        let mut leaf2 = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        leaf2.recompute_hash();
        assert_eq!(hash1, *leaf2.hash(), "same leaf content must produce same hash");
    }

    #[test]
    fn different_centroids_produce_different_hashes() {
        let mut leaf1 = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let mut leaf2 = make_leaf(vec![0.0, 1.0, 0.0], 2, 5);
        leaf1.recompute_hash();
        leaf2.recompute_hash();
        assert_ne!(*leaf1.hash(), *leaf2.hash(), "different centroids must produce different hashes");
    }

    #[test]
    fn different_counts_produce_different_hashes() {
        let mut leaf1 = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let mut leaf2 = make_leaf(vec![1.0, 0.0, 0.0], 1, 10);
        leaf1.recompute_hash();
        leaf2.recompute_hash();
        assert_ne!(*leaf1.hash(), *leaf2.hash(), "different counts must produce different hashes");
    }

    #[test]
    fn internal_hash_aggregates_children() {
        let mut left = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        left.recompute_hash();
        let mut right = make_leaf(vec![0.0, 1.0, 0.0], 2, 3);
        right.recompute_hash();

        let left_hash = *left.hash();
        let right_hash = *right.hash();

        let mut internal = MerkleNode::Internal {
            hash: [0u8; 32],
            left: Box::new(left),
            right: Box::new(right),
            centroid: vec![0.625, 0.375, 0.0], // weighted average
            count: 8,
        };
        internal.recompute_hash();

        // Internal hash should be different from either child
        assert_ne!(*internal.hash(), left_hash);
        assert_ne!(*internal.hash(), right_hash);
    }

    // ── 2. centroid_distance ──────────────────────────────────────────

    #[test]
    fn centroid_distance_self_is_zero() {
        let leaf = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let dist = leaf.centroid_distance(&[1.0, 0.0, 0.0]);
        assert!((dist - 0.0).abs() < 1e-6, "distance to self should be ~0, got {dist}");
    }

    #[test]
    fn centroid_distance_orthogonal_is_one() {
        let leaf = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let dist = leaf.centroid_distance(&[0.0, 1.0, 0.0]);
        assert!((dist - 1.0).abs() < 1e-6, "distance to orthogonal should be 1, got {dist}");
    }

    #[test]
    fn centroid_distance_to_internal_node() {
        let left = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let right = make_leaf(vec![0.0, 1.0, 0.0], 2, 5);
        let internal = MerkleNode::Internal {
            hash: [0u8; 32],
            left: Box::new(left),
            right: Box::new(right),
            centroid: vec![0.5, 0.5, 0.0],
            count: 10,
        };
        let dist = internal.centroid_distance(&[0.5, 0.5, 0.0]);
        assert!((dist - 0.0).abs() < 1e-6, "distance to centroid should be ~0, got {dist}");
    }

    // ── 3. collect_leaves ─────────────────────────────────────────────

    #[test]
    fn single_leaf_collects_itself() {
        let leaf = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let leaves = leaf.collect_leaves();
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].cluster_id(), Some(1));
    }

    #[test]
    fn internal_collects_all_leaves() {
        let left = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let right = make_leaf(vec![0.0, 1.0, 0.0], 2, 3);
        let internal = MerkleNode::Internal {
            hash: [0u8; 32],
            left: Box::new(left),
            right: Box::new(right),
            centroid: vec![0.625, 0.375, 0.0],
            count: 8,
        };
        let leaves = internal.collect_leaves();
        assert_eq!(leaves.len(), 2);
        let ids: Vec<u64> = leaves.iter().filter_map(|l| l.cluster_id()).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[test]
    fn empty_subtree_via_no_leaves() {
        // There's no empty variant; minimum is a single leaf.
        // This test exists to confirm the API handles a single leaf correctly.
        let leaf = make_leaf(vec![0.5, 0.5], 42, 1);
        assert_eq!(leaf.collect_leaves().len(), 1);
    }

    // ── 4. Accessors ──────────────────────────────────────────────────

    #[test]
    fn leaf_accessors() {
        let leaf = make_leaf(vec![1.0, 2.0, 3.0], 7, 42);
        assert_eq!(leaf.centroid(), &[1.0, 2.0, 3.0]);
        assert_eq!(leaf.count(), 42);
        assert_eq!(leaf.cluster_id(), Some(7));
        assert_eq!(leaf.hash(), &[0u8; 32]);
    }

    #[test]
    fn internal_accessors() {
        let left = make_leaf(vec![1.0, 0.0], 1, 10);
        let right = make_leaf(vec![0.0, 1.0], 2, 5);
        let internal = MerkleNode::Internal {
            hash: [0xABu8; 32],
            left: Box::new(left),
            right: Box::new(right),
            centroid: vec![0.67, 0.33],
            count: 15,
        };
        assert!((internal.centroid()[0] - 0.67).abs() < 0.01);
        assert_eq!(internal.count(), 15);
        assert!(internal.cluster_id().is_none());
        assert_eq!(*internal.hash(), [0xABu8; 32]);
    }

    // ── 5. owned leaf collection ──────────────────────────────────────

    #[test]
    fn collect_leaves_owned_returns_owned_nodes() {
        let left = make_leaf(vec![1.0, 0.0, 0.0], 1, 5);
        let right = make_leaf(vec![0.0, 1.0, 0.0], 2, 3);
        let internal = MerkleNode::Internal {
            hash: [0u8; 32],
            left: Box::new(left),
            right: Box::new(right),
            centroid: vec![0.625, 0.375, 0.0],
            count: 8,
        };
        let owned = internal.collect_leaves_owned();
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].cluster_id(), Some(1));
        assert_eq!(owned[1].cluster_id(), Some(2));
    }
}
