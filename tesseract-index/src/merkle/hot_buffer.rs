// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HotBuffer — absorbs inserts, immediately queryable.
//!
//! The hot buffer sits between the WAL and the Merkle tree merge path.
//! New vectors are inserted here first (O(1)), made immediately searchable
//! via linear scan, and asynchronously merged into the tree when the buffer
//! is full.

use std::sync::atomic::AtomicBool;

/// A buffered vector entry waiting to be merged into the Merkle tree.
#[derive(Debug, Clone)]
pub struct BufferedVector {
    /// Unique vector identifier.
    pub id: u64,
    /// The vector data (f32 for SIMD-friendly distance computation).
    pub vector: Vec<f32>,
    /// Optional metadata associated with this vector.
    pub metadata: serde_json::Value,
}

/// In-memory buffer for recent inserts.
///
/// The buffer accepts vectors immediately and makes them queryable via
/// linear scan. When `capacity` is reached, the caller should drain the
/// buffer and merge its contents into the [`MerkleTree`](super::tree::MerkleTree).
///
/// # Concurrency
///
/// The buffer itself is **not** thread-safe (intended to be used behind
/// a `Mutex`). The `merging` flag is atomic so that external merge
/// coordination can check/set merge-in-progress without a full lock.
pub struct HotBuffer {
    /// Flat storage of buffered vectors.
    vectors: Vec<BufferedVector>,
    /// Maximum number of vectors before merge is triggered.
    capacity: usize,
    /// Whether a merge is currently in progress.
    pub merging: AtomicBool,
}

impl HotBuffer {
    /// Create a new hot buffer with the given capacity.
    ///
    /// Once `len()` reaches `capacity`, the buffer is considered full and
    /// the caller should initiate a merge.
    pub fn new(capacity: usize) -> Self {
        Self {
            vectors: Vec::with_capacity(capacity),
            capacity,
            merging: AtomicBool::new(false),
        }
    }

    /// Insert a vector into the buffer.
    ///
    /// Returns `true` if the buffer is now full (i.e. `len() >= capacity`),
    /// signalling that a merge should be triggered.
    pub fn insert(&mut self, id: u64, vector: Vec<f32>, metadata: serde_json::Value) -> bool {
        self.vectors.push(BufferedVector { id, vector, metadata });
        self.is_full()
    }

    /// Linear scan through the buffer, returning up to `k` results sorted
    /// by cosine similarity (closest first).
    ///
    /// Each result is `(id, cosine_distance)`.
    ///
    /// # Performance
    ///
    /// O(buffer_len × dim) — uses a flat `Vec` for cache-friendly access.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        if self.vectors.is_empty() || k == 0 {
            return Vec::new();
        }

        // Compute cosine distance for each buffered vector.
        let mut results: Vec<(u64, f32)> = self
            .vectors
            .iter()
            .map(|bv| {
                let dot: f32 = bv.vector.iter().zip(query).map(|(a, b)| a * b).sum();
                let dist = 1.0 - dot;
                (bv.id, dist)
            })
            .collect();

        // Sort by distance (ascending = closest first).
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    /// Take ownership of the buffer contents, replacing with an empty buffer.
    ///
    /// Used during merge to atomically drain the buffer without blocking
    /// concurrent reads (caller should hold a lock on the `Mutex`).
    pub fn drain(&mut self) -> Vec<BufferedVector> {
        std::mem::take(&mut self.vectors)
    }

    /// Number of vectors currently in the buffer.
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Returns `true` if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Returns `true` if the buffer has reached its capacity.
    pub fn is_full(&self) -> bool {
        self.vectors.len() >= self.capacity
    }

    /// The maximum capacity of this buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Normalize a vector in-place for cosine distance.
    fn normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    // ── 1. Insert and query basics ────────────────────────────────────

    #[test]
    fn insert_single_vector() {
        let mut buffer = HotBuffer::new(100);
        let inserted = buffer.insert(42, vec![1.0, 0.0, 0.0], serde_json::json!({"label": "test"}));
        assert_eq!(buffer.len(), 1);
        assert!(!inserted, "buffer should not be full yet");
    }

    #[test]
    fn search_returns_inserted_vector() {
        let mut buffer = HotBuffer::new(100);
        buffer.insert(42, vec![1.0, 0.0, 0.0], serde_json::json!({}));
        let results = buffer.search(&[1.0, 0.0, 0.0], 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 42);
        assert!(results[0].1 < 1e-6, "distance to self should be ~0, got {}", results[0].1);
    }

    // ── 2. Empty buffer ───────────────────────────────────────────────

    #[test]
    fn empty_buffer_returns_no_results() {
        let buffer = HotBuffer::new(100);
        assert!(buffer.is_empty());
        let results = buffer.search(&[1.0, 0.0, 0.0], 10);
        assert!(results.is_empty());
    }

    // ── 3. Capacity and full detection ────────────────────────────────

    #[test]
    fn buffer_full_when_capacity_reached() {
        let mut buffer = HotBuffer::new(3);
        assert!(!buffer.insert(1, vec![1.0, 0.0, 0.0], serde_json::json!({})));
        assert!(!buffer.insert(2, vec![0.0, 1.0, 0.0], serde_json::json!({})));
        let full = buffer.insert(3, vec![0.0, 0.0, 1.0], serde_json::json!({}));
        assert!(full, "buffer should be full at capacity");
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn buffer_capacity_exceeded_stays_full() {
        let mut buffer = HotBuffer::new(2);
        buffer.insert(1, vec![1.0, 0.0], serde_json::json!({}));
        buffer.insert(2, vec![0.0, 1.0], serde_json::json!({}));
        assert!(buffer.is_full());
        // Inserting more still reports full
        let full = buffer.insert(3, vec![0.5, 0.5], serde_json::json!({}));
        assert!(full);
    }

    // ── 4. Search ranking ─────────────────────────────────────────────

    #[test]
    fn search_returns_closest_first() {
        let mut buffer = HotBuffer::new(100);
        // Insert three vectors at different distances from query
        buffer.insert(1, vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({})); // closest to [1,0,0,0]
        buffer.insert(2, vec![0.0, 1.0, 0.0, 0.0], serde_json::json!({}));
        buffer.insert(3, vec![0.0, 0.0, 1.0, 0.0], serde_json::json!({}));

        let results = buffer.search(&[1.0, 0.0, 0.0, 0.0], 3);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, 1, "id=1 should be closest");
        // Verify sorted by distance
        for w in results.windows(2) {
            assert!(w[0].1 <= w[1].1, "results must be sorted by distance ascending");
        }
    }

    // ── 5. k limit ────────────────────────────────────────────────────

    #[test]
    fn search_respects_k_limit() {
        let mut buffer = HotBuffer::new(100);
        for i in 0..10u64 {
            let mut v = vec![i as f32 / 10.0, 0.0, 0.0, 0.0];
            normalize(&mut v);
            buffer.insert(i, v, serde_json::json!({}));
        }
        let results = buffer.search(&[1.0, 0.0, 0.0, 0.0], 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn search_with_k_zero_returns_empty() {
        let mut buffer = HotBuffer::new(100);
        buffer.insert(1, vec![1.0, 0.0, 0.0], serde_json::json!({}));
        let results = buffer.search(&[1.0, 0.0, 0.0], 0);
        assert!(results.is_empty());
    }

    // ── 6. Drain ──────────────────────────────────────────────────────

    #[test]
    fn drain_empties_buffer() {
        let mut buffer = HotBuffer::new(100);
        buffer.insert(1, vec![1.0, 0.0, 0.0], serde_json::json!({}));
        buffer.insert(2, vec![0.0, 1.0, 0.0], serde_json::json!({}));
        assert_eq!(buffer.len(), 2);

        let drained = buffer.drain();
        assert_eq!(drained.len(), 2);
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn drain_empty_buffer_returns_empty_vec() {
        let mut buffer = HotBuffer::new(100);
        let drained = buffer.drain();
        assert!(drained.is_empty());
        assert!(buffer.is_empty());
    }

    // ── 7. Multiple inserts and search correctness ────────────────────

    #[test]
    fn search_matches_closest_in_multi_vector_scenario() {
        let mut buffer = HotBuffer::new(100);
        // Insert vectors at different positions
        buffer.insert(10, vec![0.99, 0.01, 0.0, 0.0], serde_json::json!({}));
        buffer.insert(20, vec![0.5, 0.5, 0.5, 0.5], serde_json::json!({}));
        buffer.insert(30, vec![0.01, 0.99, 0.0, 0.0], serde_json::json!({}));

        // Normalize all
        // (we're using un-normalized but for query [1,0,0,0], id=10 should still be closest)
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = buffer.search(&query, 2);
        assert_eq!(results[0].0, 10, "id=10 should be closest to [1,0,0,0]");
        // Both 20 (0.5,0.5,0.5,0.5) and 30 (0.01,0.99,0,0) have similar distances
        // but 20 has some overlap with dim0 so should be closer
        assert_eq!(results[1].0, 20, "id=20 should be second closest");
    }

    // ── 8. Merging flag ───────────────────────────────────────────────

    #[test]
    fn merging_flag_defaults_to_false() {
        let buffer = HotBuffer::new(100);
        assert!(!buffer.merging.load(Ordering::SeqCst));
    }

    #[test]
    fn merging_flag_can_be_set() {
        let buffer = HotBuffer::new(100);
        buffer.merging.store(true, Ordering::SeqCst);
        assert!(buffer.merging.load(Ordering::SeqCst));
    }

    // ── 9. Capacity accessor ──────────────────────────────────────────

    #[test]
    fn capacity_returns_configured_value() {
        let buffer = HotBuffer::new(5000);
        assert_eq!(buffer.capacity(), 5000);
    }

    #[test]
    fn is_full_returns_false_when_below_capacity() {
        let mut buffer = HotBuffer::new(10);
        for i in 0..5u64 {
            buffer.insert(i, vec![1.0, 0.0, 0.0], serde_json::json!({}));
        }
        assert!(!buffer.is_full());
    }
}
