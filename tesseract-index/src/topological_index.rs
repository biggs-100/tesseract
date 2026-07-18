// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Topological index trait and type-erased enum.
//!
//! `TopologicalIndex` defines a common interface for ANN algorithms.
//! `AnyIndex` provides type-erased storage dispatching to concrete HNSW
//! implementations.

use std::io::{Read, Write};

use tesseract_core::projection::WeightMask;
use tesseract_core::types::VectorId;

use tesseract_common::error::Result;

use crate::distance::{CosineComputer, DistanceComputer, EuclideanComputer};
use crate::hnsw::HnswIndex;

/// Abstraction for any ANN index algorithm.
///
/// Implementations must be `Send + Sync` to support concurrent access patterns
/// in the storage engine.
pub trait TopologicalIndex: Send + Sync {
    /// Insert a vector with the given ID.
    ///
    /// If the ID already exists, the vector is replaced (idempotent).
    fn insert(&mut self, id: VectorId, vector: &[f64]) -> Result<()>;

    /// Search for the nearest neighbors to `query`.
    ///
    /// `ef` controls the search breadth (recall vs latency tradeoff).
    /// `mask` optionally applies a weighted projection during search.
    fn search(&self, query: &[f64], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>>;

    /// Remove a vector by ID (tombstone).
    fn remove(&mut self, id: &VectorId) -> Result<()>;

    /// Number of vectors in the index (including tombstones).
    fn len(&self) -> usize;

    /// Return `true` if the index holds no vectors.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Save the index state to a writer.
    ///
    /// Format: `[u32 LE version] [bincode(HnswSnapshot)]`
    fn save(&self, writer: &mut dyn Write) -> Result<()>;

    /// Load the index state from a reader.
    ///
    /// Validates the version prefix before deserializing.
    fn load(&mut self, reader: &mut dyn Read) -> Result<()>;
}

// ── Implement TopologicalIndex for HnswIndex ──────────────────────────────

impl<D: DistanceComputer + 'static> TopologicalIndex for HnswIndex<D> {
    fn insert(&mut self, id: VectorId, vector: &[f64]) -> Result<()> {
        self.insert(id, vector)
    }

    fn search(&self, query: &[f64], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>> {
        self.search(query, ef, mask)
    }

    fn remove(&mut self, id: &VectorId) -> Result<()> {
        self.remove(id)
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn save(&self, writer: &mut dyn Write) -> Result<()> {
        self.save(writer)
    }

    fn load(&mut self, reader: &mut dyn Read) -> Result<()> {
        self.load(reader)
    }
}

// ── Type-erased Index ─────────────────────────────────────────────────────

/// Type-erased index that dispatches to the concrete implementation.
///
/// Supports `Cosine` and `Euclidean` distance metrics. Use this enum when
/// the concrete distance computer type is not known at compile time.
pub enum AnyIndex {
    /// HNSW index using cosine distance.
    Cosine(HnswIndex<CosineComputer>),
    /// HNSW index using euclidean distance.
    Euclidean(HnswIndex<EuclideanComputer>),
}

impl TopologicalIndex for AnyIndex {
    fn insert(&mut self, id: VectorId, vector: &[f64]) -> Result<()> {
        match self {
            AnyIndex::Cosine(i) => i.insert(id, vector),
            AnyIndex::Euclidean(i) => i.insert(id, vector),
        }
    }

    fn search(&self, query: &[f64], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>> {
        match self {
            AnyIndex::Cosine(i) => i.search(query, ef, mask),
            AnyIndex::Euclidean(i) => i.search(query, ef, mask),
        }
    }

    fn remove(&mut self, id: &VectorId) -> Result<()> {
        match self {
            AnyIndex::Cosine(i) => i.remove(id),
            AnyIndex::Euclidean(i) => i.remove(id),
        }
    }

    fn len(&self) -> usize {
        match self {
            AnyIndex::Cosine(i) => i.len(),
            AnyIndex::Euclidean(i) => i.len(),
        }
    }

    fn save(&self, writer: &mut dyn Write) -> Result<()> {
        match self {
            AnyIndex::Cosine(i) => i.save(writer),
            AnyIndex::Euclidean(i) => i.save(writer),
        }
    }

    fn load(&mut self, reader: &mut dyn Read) -> Result<()> {
        match self {
            AnyIndex::Cosine(i) => i.load(reader),
            AnyIndex::Euclidean(i) => i.load(reader),
        }
    }
}

impl AnyIndex {
    /// Return `true` if this is a cosine-based index.
    pub fn is_cosine(&self) -> bool {
        matches!(self, AnyIndex::Cosine(_))
    }

    /// Return `true` if this is an euclidean-based index.
    pub fn is_euclidean(&self) -> bool {
        matches!(self, AnyIndex::Euclidean(_))
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

    // ── TopologicalIndex trait dispatch ───────────────────────────────

    #[test]
    fn topological_trait_insert_and_search() {
        let mut idx = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();

        let results = idx.search(&[0.5, 0.5, 0.5, 0.5], 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(1));
    }

    #[test]
    fn topological_trait_polymorphic_dispatch() {
        let mut idx: HnswIndex<CosineComputer> = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        idx.insert(VectorId(42), &[0.5, 0.5, 0.5, 0.5]).unwrap();

        // Dispatch through trait object
        let trait_obj: &dyn TopologicalIndex = &idx;
        assert_eq!(trait_obj.len(), 1);

        let trait_mut: &mut dyn TopologicalIndex = &mut idx;
        trait_mut.insert(VectorId(7), &[0.1, 0.1, 0.1, 0.1]).unwrap();
        assert_eq!(trait_mut.len(), 2);
    }

    #[test]
    fn topological_trait_remove_and_len() {
        let mut idx: HnswIndex<CosineComputer> = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();
        idx.insert(VectorId(2), &[0.1, 0.1, 0.1, 0.1]).unwrap();

        let trait_obj: &mut dyn TopologicalIndex = &mut idx;
        trait_obj.remove(&VectorId(1)).unwrap();
        assert_eq!(trait_obj.len(), 2); // len includes tombstones
    }

    // ── AnyIndex::Cosine ──────────────────────────────────────────────

    #[test]
    fn any_index_cosine_insert_and_search() {
        let mut idx = AnyIndex::Cosine(HnswIndex::new(4, CosineComputer, HnswConfig::default()));

        idx.insert(VectorId(1), &[0.5, 0.5, 0.5, 0.5]).unwrap();
        let results = idx.search(&[0.5, 0.5, 0.5, 0.5], 10, None).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(1));
        assert!(results[0].1 < 1e-6);
    }

    #[test]
    fn any_index_cosine_remove() {
        let mut idx = AnyIndex::Cosine(HnswIndex::new(4, CosineComputer, HnswConfig::default()));

        idx.insert(VectorId(1), &[0.9, 0.1, 0.1, 0.1]).unwrap();
        idx.insert(VectorId(2), &[0.1, 0.9, 0.1, 0.1]).unwrap();

        idx.remove(&VectorId(1)).unwrap();
        let results = idx.search(&[0.9, 0.1, 0.1, 0.1], 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(2));
    }

    #[test]
    fn any_index_cosine_type_check() {
        let idx = AnyIndex::Cosine(HnswIndex::new(4, CosineComputer, HnswConfig::default()));
        assert!(idx.is_cosine());
        assert!(!idx.is_euclidean());
    }

    // ── AnyIndex::Euclidean ───────────────────────────────────────────

    #[test]
    fn any_index_euclidean_insert_and_search() {
        let config = HnswConfig { distance_metric: crate::types::DistanceMetric::Euclidean, ..HnswConfig::default() };
        let mut idx = AnyIndex::Euclidean(HnswIndex::new(3, crate::distance::EuclideanComputer, config));

        idx.insert(VectorId(1), &[0.0, 0.0, 0.0]).unwrap();
        idx.insert(VectorId(2), &[3.0, 4.0, 0.0]).unwrap();

        let results = idx.search(&[0.0, 0.0, 0.0], 10, None).unwrap();
        assert_eq!(results[0].0, VectorId(1));
        assert!(results[0].1 < 1e-6);
    }

    #[test]
    fn any_index_euclidean_remove() {
        let config = HnswConfig { distance_metric: crate::types::DistanceMetric::Euclidean, ..HnswConfig::default() };
        let mut idx = AnyIndex::Euclidean(HnswIndex::new(3, crate::distance::EuclideanComputer, config));

        idx.insert(VectorId(1), &[0.0, 0.0, 0.0]).unwrap();
        idx.insert(VectorId(2), &[3.0, 4.0, 0.0]).unwrap();

        idx.remove(&VectorId(1)).unwrap();
        let results = idx.search(&[0.0, 0.0, 0.0], 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(2));
    }

    #[test]
    fn any_index_euclidean_type_check() {
        let config = HnswConfig { distance_metric: crate::types::DistanceMetric::Euclidean, ..HnswConfig::default() };
        let idx = AnyIndex::Euclidean(HnswIndex::new(3, crate::distance::EuclideanComputer, config));
        assert!(!idx.is_cosine());
        assert!(idx.is_euclidean());
    }

    // ── Trait bound check ─────────────────────────────────────────────

    #[test]
    fn topological_index_is_send_sync() {
        fn assert_send<T: Send + ?Sized>() {}
        fn assert_sync<T: Sync + ?Sized>() {}

        assert_send::<dyn TopologicalIndex>();
        assert_sync::<dyn TopologicalIndex>();
    }

    // ── AnyIndex default len ──────────────────────────────────────────

    #[test]
    fn any_index_cosine_empty_len() {
        let idx = AnyIndex::Cosine(HnswIndex::new(4, CosineComputer, HnswConfig::default()));
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn any_index_euclidean_empty_len() {
        let config = HnswConfig { distance_metric: crate::types::DistanceMetric::Euclidean, ..HnswConfig::default() };
        let idx = AnyIndex::Euclidean(HnswIndex::new(3, crate::distance::EuclideanComputer, config));
        assert_eq!(idx.len(), 0);
    }

    // ── Weighted search through AnyIndex ──────────────────────────────

    #[test]
    fn any_index_cosine_weighted_search() {
        let mut rng = StdRng::seed_from_u64(7);
        let dim = 4;
        let config = HnswConfig::default();
        let mut idx = AnyIndex::Cosine(HnswIndex::new(dim, CosineComputer, config));

        for i in 0..30 {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            idx.insert(VectorId(i as u64), &v).unwrap();
        }

        let q: Vec<f64> = vec![0.5, 0.5, 0.5, 0.5];
        let mask = WeightMask(vec![(0, 0.0)]);
        let weighted = idx.search(&q, 20, Some(&mask)).unwrap();
        assert!(!weighted.is_empty());
        assert_eq!(weighted.len(), 20);
    }
}
