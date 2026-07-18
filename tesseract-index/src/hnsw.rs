// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HNSW (Hierarchical Navigable Small World) index implementation.
//!
//! Implements the HNSW algorithm from Malkov & Yashunin (2016) with:
//! - Generic `DistanceComputer` for pluggable distance metrics
//! - Multi-layer navigation with exponential level decay
//! - Weighted distance via `WeightMask` (fused in the distance loop)
//! - `RwLock` for concurrent reads
//! - Tombstone deletion

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::RwLock;

use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::debug;

use tesseract_common::error::{Error, Result};
use tesseract_core::projection::WeightMask;
use tesseract_core::types::VectorId;

use crate::distance::{DistanceComputer, mask_to_dense};
use crate::types::HnswConfig;

/// Maximum layer for level generation.
const MAX_LAYER: usize = 32;

// ── Helper: Ord-compatible f32 wrapper for BinaryHeap ──────────────────────

/// A newtype over `f32` that implements `Ord` (panics on NaN at comparison).
///
/// Distances in HNSW are always finite non-negative values (cosine ∈ [0, 2],
/// euclidean ∈ [0, ∞)), so NaN should never occur in practice. We treat the
/// unlikely NaN case as `Ordering::Equal` to avoid panics.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Distance(f32);

impl Eq for Distance {}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Equal)
    }
}

// ── Graph Node ─────────────────────────────────────────────────────────────

/// A single node in the HNSW graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HnswNode {
    /// Edges per layer: `edges[layer]` = vec of neighbor node indices.
    pub(crate) edges: Vec<Vec<usize>>,
}

// ── Main Index ─────────────────────────────────────────────────────────────

/// A hierarchical navigable small world index parameterised over a distance
/// computer via static dispatch.
///
/// # Type Parameters
///
/// * `D` — A `DistanceComputer` that defines the distance metric (cosine,
///   euclidean, etc.). The trait is `Send + Sync + Clone` so `HnswIndex` is
///   itself `Send + Sync`.
pub struct HnswIndex<D: DistanceComputer> {
    distance: D,
    pub(crate) config: HnswConfig,
    pub(crate) dim: usize,

    /// Flat storage of graph nodes (indexed by internal node id).
    pub(crate) nodes: Vec<HnswNode>,
    /// Maps internal node index → external `VectorId`.
    pub(crate) id_to_node: Vec<VectorId>,
    /// Flat storage of f32 vectors (SoA-friendly).
    pub(crate) vectors: Vec<Vec<f32>>,
    /// Current entry point for graph traversal.
    pub(crate) entry_point: Option<usize>,
    /// Highest layer that has at least one node.
    pub(crate) max_layer: usize,
    /// Tombstone bitset: `true` means the node is logically deleted.
    pub(crate) deleted: Vec<bool>,

    /// Read-write lock enabling concurrent searches.
    lock: RwLock<()>,
}

impl<D: DistanceComputer> HnswIndex<D> {
    /// Create a new empty HNSW index.
    ///
    /// * `dim` — dimensionality of vectors
    /// * `distance` — the distance computer (e.g. `CosineComputer`)
    /// * `config` — HNSW topology parameters
    pub fn new(dim: usize, distance: D, config: HnswConfig) -> Self {
        Self {
            distance,
            config,
            dim,
            nodes: Vec::new(),
            id_to_node: Vec::new(),
            vectors: Vec::new(),
            entry_point: None,
            max_layer: 0,
            deleted: Vec::new(),
            lock: RwLock::new(()),
        }
    }

    /// Return the number of vectors in the index (including tombstones).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Return `true` if the index holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    // ── Public API ──────────────────────────────────────────────────────

    /// Insert a vector with the given external id.
    ///
    /// If `id` already exists, the stored vector is **replaced** (idempotent
    /// insert — node count does not increase). Otherwise a new HNSW node is
    /// created, linked into the graph, and the entry point updated if
    /// necessary.
    pub fn insert(&mut self, id: VectorId, vector: &[f64]) -> Result<()> {
        if vector.len() != self.dim {
            return Err(Error::DimensionMismatch(vector.len(), self.dim));
        }

        // No RwLock write needed — `&mut self` already provides exclusive
        // access. The internal RwLock is for the `search(&self)` read path.
        let vec_f32: Vec<f32> = vector.iter().map(|&x| x as f32).collect();

        // ── Idempotent insert: replace existing vector ──────────────
        if let Some(existing) = self.id_to_node.iter().position(|x| *x == id) {
            self.vectors[existing] = vec_f32;
            return Ok(());
        }

        let node_idx = self.nodes.len();
        let level = self.random_level();

        self.nodes.push(HnswNode { edges: vec![vec![]; level + 1] });
        self.id_to_node.push(id);
        self.vectors.push(vec_f32);
        self.deleted.push(false);

        // First node → entry point, done.
        if node_idx == 0 {
            self.entry_point = Some(0);
            self.max_layer = level;
            debug!(level, "inserted first node");
            return Ok(());
        }

        let entry = self.entry_point.unwrap();

        // ── Phase 1: greedy descent from top layer to level+1 ──────
        // (Per paper, only descend through layers the entry point has)
        let mut curr = entry;
        let descent_top = std::cmp::min(self.max_layer, self.nodes[entry].edges.len().saturating_sub(1));
        let descent_bottom = level + 1;
        for layer in (descent_bottom..=descent_top).rev() {
            if layer < self.nodes[curr].edges.len() {
                let (new_curr, _) = self.greedy_search_layer(curr, &self.vectors[node_idx], layer);
                curr = new_curr;
            }
        }

        // ── Phase 2: search + connect at layers min(level, max_layer) … 0
        // Per paper §3 Algorithm 1 line 8: top = min(L, l).
        let top_layer = std::cmp::min(level, self.max_layer);
        let m_max0 = self.config.m_max0;
        let m = self.config.m;

        for layer in (0..=top_layer).rev() {
            let ef = self.config.ef_construction;
            let candidates = self.search_layer(curr, &self.vectors[node_idx], ef, layer);

            let max_conn = if layer == 0 { m_max0 } else { m };
            let nearest = Self::select_neighbors(&candidates, max_conn);

            // Bidirectional connections
            for &neighbor in &nearest {
                self.nodes[node_idx].edges[layer].push(neighbor);
                self.nodes[neighbor].edges[layer].push(node_idx);
            }

            for &neighbor in &nearest {
                self.shrink_connections(neighbor, layer);
            }

            if !nearest.is_empty() {
                curr = nearest[0];
            }
        }

        // ── Update max_layer / entry point if needed ──────────────────
        if level > self.max_layer {
            self.max_layer = level;
            self.entry_point = Some(node_idx);
        }

        Ok(())
    }

    /// Search for the nearest neighbours of `query`.
    ///
    /// * `query` — f64 query vector (converted to f32 internally)
    /// * `ef` — size of the dynamic candidate list (higher = more recall)
    /// * `mask` — optional weight mask (fused into distance loop)
    ///
    /// Returns up to `ef` results sorted by distance ascending, with
    /// tombstoned nodes excluded.
    pub fn search(&self, query: &[f64], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>> {
        if query.len() != self.dim {
            return Err(Error::DimensionMismatch(query.len(), self.dim));
        }

        let query_f32: Vec<f32> = query.iter().map(|&x| x as f32).collect();
        let weights = mask.map(|m| mask_to_dense(m, self.dim));
        let _lock = self.lock.read().unwrap();

        if self.nodes.is_empty() {
            return Ok(vec![]);
        }

        let mut curr = self.entry_point.unwrap();

        // Greedy descent from top layer to layer 1
        for layer in (1..=self.max_layer).rev() {
            let (new_curr, _) = match weights {
                Some(ref w) => self.greedy_search_layer_weighted(curr, &query_f32, layer, w),
                None => self.greedy_search_layer(curr, &query_f32, layer),
            };
            curr = new_curr;
        }

        // Search layer 0 with the requested ef
        let candidates = match weights {
            Some(ref w) => self.search_layer_weighted(curr, &query_f32, ef, 0, w),
            None => self.search_layer(curr, &query_f32, ef, 0),
        };

        // Filter tombstones, map to external ids, keep ef closest, sort
        let mut results: Vec<(VectorId, f32)> = candidates
            .into_iter()
            .filter(|(idx, _)| !self.deleted[*idx])
            .take(ef)
            .map(|(_idx, dist)| (self.id_to_node[_idx].clone(), dist))
            .collect();

        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        Ok(results)
    }

    /// Remove a vector by marking it as deleted (tombstone).
    ///
    /// The node is kept in the graph to preserve edge structure, but is
    /// excluded from search results.
    pub fn remove(&mut self, id: &VectorId) -> Result<()> {
        // No RwLock needed — `&mut self` already provides exclusive access.
        let pos = self
            .id_to_node
            .iter()
            .position(|x| x == id)
            .ok_or_else(|| Error::NotFound(format!("VectorId {:?} not found in index", id)))?;
        self.deleted[pos] = true;
        debug!(node = pos, "tombstoned node");
        Ok(())
    }

    // ── Distance helpers ──────────────────────────────────────────────

    #[inline]
    fn compute_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        self.distance.distance(a, b)
    }

    #[inline]
    fn compute_distance_weighted(&self, a: &[f32], b: &[f32], weights: &[f32]) -> f32 {
        self.distance.distance_weighted(a, b, weights)
    }

    // ── Level generation ──────────────────────────────────────────────

    /// Maximum number of layers allowed based on current node count.
    ///
    /// Per spec: L = max(1, ceil(log₂(N))) where N is the current node count.
    /// The first node always gets level 0 so that a single-node graph has
    /// exactly one layer.
    fn max_allowed_level(&self) -> usize {
        let n = self.nodes.len();
        if n == 0 {
            return 0; // first node → level 0 → L = 1
        }
        let log2n = (n as f64).log2().ceil() as usize;
        std::cmp::max(1, log2n)
    }

    /// Generate a random layer level using the exponential decay from the
    /// HNSW paper: `l = floor(-ln(uniform(0,1)) × mL)`, capped by
    /// [`MAX_LAYER`] and the node-count-based cap.
    fn random_level(&self) -> usize {
        let mut rng = rand::thread_rng();
        let r: f64 = rng.r#gen();
        let level = (-r.ln() * self.config.ml).floor() as usize;
        level.min(MAX_LAYER).min(self.max_allowed_level())
    }

    // ── Greedy (descent) search ───────────────────────────────────────

    /// Unweighted greedy traversal at a single layer.
    ///
    /// Repeatedly moves to the closest neighbour until no improvement is
    /// found. Returns the closest node and its distance.
    fn greedy_search_layer(&self, start: usize, query: &[f32], layer: usize) -> (usize, f32) {
        let mut curr = start;
        let mut curr_dist = self.compute_distance(&self.vectors[curr], query);
        loop {
            let mut changed = false;
            for &neighbor in &self.nodes[curr].edges[layer] {
                if self.deleted[neighbor] {
                    continue;
                }
                let dist = self.compute_distance(&self.vectors[neighbor], query);
                if dist < curr_dist {
                    curr = neighbor;
                    curr_dist = dist;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (curr, curr_dist)
    }

    /// Weighted greedy traversal at a single layer.
    ///
    /// Same as [`greedy_search_layer`] but uses the fused weighted distance.
    fn greedy_search_layer_weighted(&self, start: usize, query: &[f32], layer: usize, weights: &[f32]) -> (usize, f32) {
        let mut curr = start;
        let mut curr_dist = self.compute_distance_weighted(&self.vectors[curr], query, weights);
        loop {
            let mut changed = false;
            for &neighbor in &self.nodes[curr].edges[layer] {
                if self.deleted[neighbor] {
                    continue;
                }
                let dist = self.compute_distance_weighted(&self.vectors[neighbor], query, weights);
                if dist < curr_dist {
                    curr = neighbor;
                    curr_dist = dist;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (curr, curr_dist)
    }

    // ── EF-based search (single layer) ────────────────────────────────

    /// Unweighted ef-search at a single layer.
    ///
    /// Maintains a min-heap of candidates and a max-heap of results. The
    /// search stops when the closest remaining candidate is farther than
    /// the farthest result and the result heap has reached size `ef`.
    #[allow(clippy::similar_names)]
    fn search_layer(&self, start: usize, query: &[f32], ef: usize, layer: usize) -> Vec<(usize, f32)> {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        let mut candidates: BinaryHeap<Reverse<(Distance, usize)>> = BinaryHeap::new();
        let mut results: BinaryHeap<(Distance, usize)> = BinaryHeap::new();

        // Prune start if it is tombstoned
        let start_dist = self.compute_distance(&self.vectors[start], query);
        candidates.push(Reverse((Distance(start_dist), start)));
        results.push((Distance(start_dist), start));
        visited[start] = true;

        while let Some(Reverse((dist, idx))) = candidates.pop() {
            // Upper-bound pruning: if the closest remaining candidate is
            // farther than the farthest result, we are done.
            if let Some(&(farthest_dist, _)) = results.peek() {
                if dist > farthest_dist && results.len() >= ef {
                    break;
                }
            }

            for &neighbor in &self.nodes[idx].edges[layer] {
                if visited[neighbor] || self.deleted[neighbor] {
                    continue;
                }
                visited[neighbor] = true;

                let n_dist = self.compute_distance(&self.vectors[neighbor], query);
                let nd = Distance(n_dist);

                if results.len() < ef || nd < results.peek().unwrap().0 {
                    candidates.push(Reverse((nd, neighbor)));
                    results.push((nd, neighbor));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        // Convert to sorted vec ascending by distance
        results.into_sorted_vec().into_iter().map(|(d, idx)| (idx, d.0)).collect()
    }

    /// Weighted ef-search at a single layer.
    ///
    /// Same as [`search_layer`] but uses the fused weighted distance.
    #[allow(clippy::similar_names)]
    fn search_layer_weighted(
        &self,
        start: usize,
        query: &[f32],
        ef: usize,
        layer: usize,
        weights: &[f32],
    ) -> Vec<(usize, f32)> {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        let mut candidates: BinaryHeap<Reverse<(Distance, usize)>> = BinaryHeap::new();
        let mut results: BinaryHeap<(Distance, usize)> = BinaryHeap::new();

        let start_dist = self.compute_distance_weighted(&self.vectors[start], query, weights);
        candidates.push(Reverse((Distance(start_dist), start)));
        results.push((Distance(start_dist), start));
        visited[start] = true;

        while let Some(Reverse((dist, idx))) = candidates.pop() {
            if let Some(&(farthest_dist, _)) = results.peek() {
                if dist > farthest_dist && results.len() >= ef {
                    break;
                }
            }

            for &neighbor in &self.nodes[idx].edges[layer] {
                if visited[neighbor] || self.deleted[neighbor] {
                    continue;
                }
                visited[neighbor] = true;

                let n_dist = self.compute_distance_weighted(&self.vectors[neighbor], query, weights);
                let nd = Distance(n_dist);

                if results.len() < ef || nd < results.peek().unwrap().0 {
                    candidates.push(Reverse((nd, neighbor)));
                    results.push((nd, neighbor));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        results.into_sorted_vec().into_iter().map(|(d, idx)| (idx, d.0)).collect()
    }

    // ── Neighbour selection & pruning ─────────────────────────────────

    /// Select the `m` closest candidates (simple truncation — already
    /// sorted by distance from the ef-search).
    fn select_neighbors(candidates: &[(usize, f32)], m: usize) -> Vec<usize> {
        candidates.iter().take(m).map(|(idx, _)| *idx).collect()
    }

    /// Trim a node's edge list so it does not exceed the per-layer maximum.
    ///
    /// Keeps the closest connections by re-sorting against the current
    /// distance metric.
    fn shrink_connections(&mut self, node_idx: usize, layer: usize) {
        let max = if layer == 0 { self.config.m_max0 } else { self.config.m };
        if self.nodes[node_idx].edges[layer].len() <= max {
            return;
        }

        // Keep only the closest `max` connections
        let center = &self.vectors[node_idx].clone();
        let edges = self.nodes[node_idx].edges[layer].clone();
        let mut sorted: Vec<(usize, f32)> =
            edges.iter().map(|&n| (n, self.compute_distance(center, &self.vectors[n]))).collect();
        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        self.nodes[node_idx].edges[layer] = sorted.into_iter().take(max).map(|(n, _)| n).collect();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    use tesseract_core::projection::WeightMask;
    use tesseract_core::types::VectorId;

    use crate::distance::CosineComputer;
    use crate::types::HnswConfig;

    use super::*;

    /// Build a small HNSW index with CosineComputer and default config.
    fn small_index() -> HnswIndex<CosineComputer> {
        let config = HnswConfig::default();
        HnswIndex::new(4, CosineComputer, config)
    }

    /// Normalize a vector in-place for cosine distance.
    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    /// Brute-force k-NN search for verification.
    fn brute_force(vectors: &[Vec<f32>], query: &[f32], k: usize, skip: &[bool]) -> Vec<(usize, f32)> {
        let mut dists: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .filter(|(i, _)| !skip[*i])
            .map(|(i, v)| {
                let dot: f32 = v.iter().zip(query).map(|(x, y)| x * y).sum();
                (i, 1.0 - dot)
            })
            .collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        dists.truncate(k);
        dists
    }

    fn brute_force_euclidean(vectors: &[Vec<f32>], query: &[f32], k: usize, skip: &[bool]) -> Vec<(usize, f32)> {
        let mut dists: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .filter(|(i, _)| !skip[*i])
            .map(|(i, v)| {
                let sum_sq: f32 = v.iter().zip(query).map(|(x, y)| (x - y).powi(2)).sum();
                (i, sum_sq.sqrt())
            })
            .collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        dists.truncate(k);
        dists
    }

    // ── 1. Empty index ────────────────────────────────────────────────

    #[test]
    fn empty_index_returns_no_results() {
        let idx = small_index();
        let results = idx.search(&[0.5, 0.5, 0.5, 0.5], 10, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn empty_index_len_zero() {
        let idx = small_index();
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());
    }

    // ── 2. Single insert ──────────────────────────────────────────────

    #[test]
    fn single_insert_is_found() {
        let mut idx = small_index();
        let v = vec![0.5, 0.5, 0.5, 0.5];
        let mut vn = v.clone();
        normalize(&mut vn);

        idx.insert(VectorId(42), &[0.5, 0.5, 0.5, 0.5]).unwrap();

        let results = idx.search(&[0.5, 0.5, 0.5, 0.5], 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(42));
        assert!(results[0].1 < 1e-6, "distance should be ~0, got {}", results[0].1);
    }

    #[test]
    fn single_insert_len_one() {
        let mut idx = small_index();
        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();
        assert_eq!(idx.len(), 1);
        assert!(!idx.is_empty());
    }

    // ── 3. Multiple inserts vs brute-force ─────────────────────────────

    #[test]
    fn multiple_inserts_nearest_neighbor_matches_brute_force() {
        let mut rng = StdRng::seed_from_u64(42);
        let dim = 8;
        let n_vectors = 100;
        let config = HnswConfig::default();
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(n_vectors);

        for i in 0..n_vectors {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            let v32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
            vectors.push(v32);
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        // Query with a random vector
        let mut q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
        let qnorm: f64 = q.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in &mut q {
            *x /= qnorm;
        }

        let results = idx.search(&q, 10, None).unwrap();
        assert!(!results.is_empty(), "should return results");

        let q32: Vec<f32> = q.iter().map(|&x| x as f32).collect();
        let brute = brute_force(&vectors, &q32, 10, &vec![false; vectors.len()]);

        // The closest result should match brute-force nearest neighbour
        assert_eq!(results[0].0, VectorId(brute[0].0 as u64), "top-1 should match brute-force");
    }

    // ── 4. Weighted search ─────────────────────────────────────────────

    #[test]
    fn weighted_search_returns_different_results() {
        let mut rng = StdRng::seed_from_u64(7);
        let dim = 4;
        let config = HnswConfig::default();
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        let mut vectors: Vec<Vec<f64>> = Vec::new();

        for i in 0..50 {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            vectors.push(v.clone());
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let q: Vec<f64> = vec![0.5, 0.5, 0.5, 0.5];
        let mask = WeightMask(vec![(0, 0.0), (1, 0.0)]); // zero out dims 0 and 1

        let unweighted = idx.search(&q, 20, None).unwrap();
        let weighted = idx.search(&q, 20, Some(&mask)).unwrap();

        // Results should differ when dimensions are zeroed
        let top_ids_unweighted: Vec<VectorId> = unweighted.iter().take(5).map(|(id, _)| id.clone()).collect();
        let top_ids_weighted: Vec<VectorId> = weighted.iter().take(5).map(|(id, _)| id.clone()).collect();

        assert_ne!(top_ids_unweighted, top_ids_weighted, "weighted and unweighted results should differ");
    }

    #[test]
    fn weighted_identity_mask_matches_unweighted() {
        let mut rng = StdRng::seed_from_u64(13);
        let dim = 4;
        let config = HnswConfig::default();
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        for i in 0..30 {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let q: Vec<f64> = vec![0.5, 0.5, 0.5, 0.5];
        // Identity mask (all 1.0) should give the same results
        let mask = WeightMask(vec![]);

        let unweighted = idx.search(&q, 10, None).unwrap();
        let weighted = idx.search(&q, 10, Some(&mask)).unwrap();

        // IDs should match (distances may differ at f32 precision)
        let ids_unweighted: Vec<VectorId> = unweighted.iter().map(|(id, _)| id.clone()).collect();
        let ids_weighted: Vec<VectorId> = weighted.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(ids_unweighted, ids_weighted);
    }

    // ── 5. Recall@10 ──────────────────────────────────────────────────

    #[test]
    fn recall_at_10_meets_threshold() {
        let mut rng = StdRng::seed_from_u64(123);
        let dim = 16;
        let n_vectors = 500; // 500 for fast tests, still gives good recall
        let n_queries = 20;

        let config = HnswConfig { ef_construction: 200, ..HnswConfig::default() };
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(n_vectors);

        for i in 0..n_vectors {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            let v32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
            vectors.push(v32);
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let skip = vec![false; vectors.len()];
        let recall_k = 10;
        let mut total_recall = 0.0_f64;

        for _ in 0..n_queries {
            let mut q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let qnorm: f64 = q.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut q {
                *x /= qnorm;
            }

            let results = idx.search(&q, 200, None).unwrap();
            let q32: Vec<f32> = q.iter().map(|&x| x as f32).collect();
            let brute = brute_force(&vectors, &q32, recall_k, &skip);

            let hnsw_ids: Vec<u64> = results.iter().take(recall_k).map(|(id, _)| id.0).collect();
            let brute_ids: Vec<u64> = brute.iter().map(|(i, _)| *i as u64).collect();

            let intersection = hnsw_ids.iter().filter(|id| brute_ids.contains(id)).count();
            total_recall += intersection as f64 / recall_k as f64;
        }

        let avg_recall = total_recall / n_queries as f64;
        assert!(avg_recall >= 0.85, "recall@10 too low: {:.4} (threshold: 0.85)", avg_recall);
    }

    // ── 6. Tombstone delete ────────────────────────────────────────────

    #[test]
    fn tombstoned_node_excluded_from_results() {
        let mut idx = small_index();

        idx.insert(VectorId(1), &[0.9, 0.1, 0.1, 0.1]).unwrap();
        idx.insert(VectorId(2), &[0.1, 0.9, 0.1, 0.1]).unwrap();

        let before = idx.search(&[0.9, 0.1, 0.1, 0.1], 10, None).unwrap();
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].0, VectorId(1));

        // Remove VectorId(1)
        idx.remove(&VectorId(1)).unwrap();

        let after = idx.search(&[0.9, 0.1, 0.1, 0.1], 10, None).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].0, VectorId(2));
    }

    #[test]
    fn remove_nonexistent_returns_error() {
        let mut idx = small_index();
        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();

        let err = idx.remove(&VectorId(999)).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    // ── 7. Idempotent insert ───────────────────────────────────────────

    #[test]
    fn reinsert_same_id_replaces_vector() {
        let mut idx = small_index();

        // Insert VectorId(1) with vector A
        idx.insert(VectorId(1), &[0.9, 0.1, 0.1, 0.1]).unwrap();
        // Insert VectorId(2) with vector B
        idx.insert(VectorId(2), &[0.1, 0.9, 0.1, 0.1]).unwrap();

        // Re-insert VectorId(1) with vector B (changing it).
        // The vector data is replaced in memory, but the graph structure
        // (edges) is NOT rebuilt — that is the expected trade-off for a
        // simple idempotent insert.
        idx.insert(VectorId(1), &[0.1, 0.9, 0.1, 0.1]).unwrap();

        // Node count should NOT increase
        assert_eq!(idx.len(), 2);

        // Both vectors should be returned (graph may still find them)
        let results = idx.search(&[0.1, 0.9, 0.1, 0.1], 10, None).unwrap();
        assert_eq!(results.len(), 2, "both vectors should be returned");
        let ids: Vec<VectorId> = results.iter().map(|(id, _)| id.clone()).collect();
        assert!(ids.contains(&VectorId(1)));
        assert!(ids.contains(&VectorId(2)));
    }

    #[test]
    fn reinsert_same_id_does_not_increase_count() {
        let mut idx = small_index();
        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();
        idx.insert(VectorId(1), &[0.6, 0.4, 0.4, 0.4]).unwrap();
        assert_eq!(idx.len(), 1, "re-insert should not increase node count");
    }

    // ── 8. Concurrent search ───────────────────────────────────────────

    #[test]
    fn concurrent_searches_all_complete() {
        use std::sync::Arc;
        use std::thread;

        let mut rng = StdRng::seed_from_u64(99);
        let dim = 8;
        let n_vectors = 200;
        let config = HnswConfig::default();
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        for i in 0..n_vectors {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let idx = Arc::new(idx);
        let mut handles = Vec::new();
        let n_threads = 4;

        for t in 0..n_threads {
            let idx = Arc::clone(&idx);
            let handle = thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(100 + t as u64);
                for _ in 0..10 {
                    let mut q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
                    let qnorm: f64 = q.iter().map(|x| x * x).sum::<f64>().sqrt();
                    for x in &mut q {
                        *x /= qnorm;
                    }
                    let results = idx.search(&q, 20, None).unwrap();
                    assert!(!results.is_empty(), "every search should return results");
                    // Verify results are sorted by distance
                    for w in results.windows(2) {
                        assert!(w[0].1 <= w[1].1, "results must be sorted by distance ascending");
                    }
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }
    }

    // ── 9. Edge cases ─────────────────────────────────────────────────

    #[test]
    fn insert_wrong_dimension_returns_error() {
        let mut idx = small_index();
        let err = idx.insert(VectorId(1), &[0.5, 0.5]).unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch(2, 4)));
    }

    #[test]
    fn search_wrong_dimension_returns_error() {
        let idx = small_index();
        let err = idx.search(&[0.5, 0.5], 10, None).unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch(2, 4)));
    }

    #[test]
    fn euclidean_distance_works() {
        use crate::distance::EuclideanComputer;

        let config = HnswConfig { distance_metric: crate::types::DistanceMetric::Euclidean, ..HnswConfig::default() };
        let mut idx = HnswIndex::new(3, EuclideanComputer, config);

        idx.insert(VectorId(1), &[0.0, 0.0, 0.0]).unwrap();
        idx.insert(VectorId(2), &[3.0, 4.0, 0.0]).unwrap();

        let results = idx.search(&[0.0, 0.0, 0.0], 10, None).unwrap();
        assert_eq!(results[0].0, VectorId(1), "closest to origin should be origin");
        assert!(results[0].1 < 1e-6, "distance to self should be ~0");

        let results2 = idx.search(&[3.0, 4.0, 0.0], 10, None).unwrap();
        assert_eq!(results2[0].0, VectorId(2), "closest to (3,4,0) should be (3,4,0)");
    }

    #[test]
    fn recall_ratio_euclidean() {
        use crate::distance::EuclideanComputer;

        let mut rng = StdRng::seed_from_u64(42);
        let dim = 8;
        let n_vectors = 200;
        let n_queries = 10;

        let config = HnswConfig {
            ef_construction: 200,
            distance_metric: crate::types::DistanceMetric::Euclidean,
            ..HnswConfig::default()
        };
        let mut idx = HnswIndex::new(dim, EuclideanComputer, config);

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(n_vectors);

        for i in 0..n_vectors {
            let v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>() * 10.0).collect();
            let v32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
            vectors.push(v32);
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let skip = vec![false; vectors.len()];
        let mut total_recall = 0.0_f64;

        for _ in 0..n_queries {
            let q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>() * 10.0).collect();
            let results = idx.search(&q, 100, None).unwrap();
            let q32: Vec<f32> = q.iter().map(|&x| x as f32).collect();
            let brute = brute_force_euclidean(&vectors, &q32, 10, &skip);

            let hnsw_ids: Vec<u64> = results.iter().take(10).map(|(id, _)| id.0).collect();
            let brute_ids: Vec<u64> = brute.iter().map(|(i, _)| *i as u64).collect();

            let intersection = hnsw_ids.iter().filter(|id| brute_ids.contains(id)).count();
            total_recall += intersection as f64 / 10.0;
        }

        let avg_recall = total_recall / n_queries as f64;
        assert!(avg_recall >= 0.80, "euclidean recall@10 too low: {:.4} (threshold: 0.80)", avg_recall);
    }

    #[test]
    fn multiple_inserts_maintains_correct_length() {
        let mut idx = small_index();
        for i in 0..50 {
            idx.insert(VectorId(i), &[0.5, 0.5, 0.5, 0.5]).unwrap();
        }
        assert_eq!(idx.len(), 50);
    }

    #[test]
    fn tombstoned_node_does_not_affect_len() {
        let mut idx = small_index();
        idx.insert(VectorId(1), &[0.9, 0.1, 0.1, 0.1]).unwrap();
        idx.insert(VectorId(2), &[0.1, 0.9, 0.1, 0.1]).unwrap();
        idx.remove(&VectorId(1)).unwrap();
        // len() counts all nodes including tombstones
        assert_eq!(idx.len(), 2);
    }
}
