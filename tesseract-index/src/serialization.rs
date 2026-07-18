// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Bincode-based serialization for HNSW graph state.
//!
//! The wire format is:
//!
//! ```text
//! [version: u32 LE] [bincode(HnswSnapshot)]
//! ```
//!
//! The `u32` version prefix enables forward / backward compatibility
//! detection. Currently only version `1` is defined.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use tesseract_core::types::VectorId;

use tesseract_common::error::{Error, Result};

use crate::distance::DistanceComputer;
use crate::hnsw::{HnswIndex, HnswNode};

/// Serializable snapshot of the full HNSW graph state.
///
/// All fields are `pub` to support inspection during testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswSnapshot {
    /// Format version (currently `1`).
    pub version: u32,
    /// Maximum number of connections per node per layer (M).
    pub m: usize,
    /// Maximum connections for layer 0 (m_max0).
    pub m_max0: usize,
    /// Candidate list size during construction.
    pub ef_construction: usize,
    /// Normalization factor for level generation: 1/ln(M).
    pub ml: f64,
    /// Dimensionality of stored vectors.
    pub dim: usize,
    /// Current entry point for graph traversal.
    pub entry_point: Option<usize>,
    /// Highest layer that has at least one node.
    pub max_layer: usize,
    /// Maps internal node index → external `VectorId`.
    pub id_to_node: Vec<VectorId>,
    /// Flat storage of f32 vectors.
    pub vectors: Vec<Vec<f32>>,
    /// Adjacency lists: `edges[node][layer]` = vec of neighbour indices.
    pub edges: Vec<Vec<Vec<usize>>>,
    /// Tombstone bitset: `true` means the node is logically deleted.
    pub deleted: Vec<bool>,
}

// ── Save / Load inherent methods on HnswIndex ─────────────────────────────

impl<D: DistanceComputer> HnswIndex<D> {
    /// Save the index to a writer.
    ///
    /// Format: `[version: u32 LE] [bincode(HnswSnapshot)]`
    ///
    /// # Errors
    ///
    /// Returns `Error::BincodeError` if serialization fails, or
    /// `Error::IoError` if writing fails.
    pub fn save(&self, writer: &mut dyn Write) -> Result<()> {
        let snapshot = HnswSnapshot {
            version: 1,
            m: self.config.m,
            m_max0: self.config.m_max0,
            ef_construction: self.config.ef_construction,
            ml: self.config.ml,
            dim: self.dim,
            entry_point: self.entry_point,
            max_layer: self.max_layer,
            id_to_node: self.id_to_node.clone(),
            vectors: self.vectors.clone(),
            edges: self.nodes.iter().map(|n| n.edges.clone()).collect(),
            deleted: self.deleted.clone(),
        };

        let bytes = bincode::serialize(&snapshot)?;
        writer.write_all(&1u32.to_le_bytes())?;
        writer.write_all(&bytes)?;
        Ok(())
    }

    /// Load the index from a reader.
    ///
    /// Validates the `u32` version prefix before deserializing the
    /// `HnswSnapshot`. Only version `1` is currently supported.
    ///
    /// # Errors
    ///
    /// Returns `Error::GraphCorrupt` if the version prefix is not
    /// recognised, or `Error::BincodeError` if deserialization fails.
    pub fn load(&mut self, reader: &mut dyn Read) -> Result<()> {
        let mut version_buf = [0u8; 4];
        reader.read_exact(&mut version_buf)?;
        let version = u32::from_le_bytes(version_buf);

        if version != 1 {
            return Err(Error::GraphCorrupt(format!("Unsupported format version: {}, expected 1", version)));
        }

        let snapshot: HnswSnapshot = bincode::deserialize_from(reader)?;

        self.config.m = snapshot.m;
        self.config.m_max0 = snapshot.m_max0;
        self.config.ef_construction = snapshot.ef_construction;
        self.config.ml = snapshot.ml;
        self.dim = snapshot.dim;
        self.entry_point = snapshot.entry_point;
        self.max_layer = snapshot.max_layer;
        self.id_to_node = snapshot.id_to_node;
        self.vectors = snapshot.vectors;
        self.deleted = snapshot.deleted;

        self.nodes = snapshot.edges.into_iter().map(|edges| HnswNode { edges }).collect();

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    use tesseract_core::types::VectorId;

    use crate::distance::CosineComputer;
    use crate::types::HnswConfig;

    use super::*;

    /// Build a small HNSW index with 100 random vectors.
    fn populated_index() -> HnswIndex<CosineComputer> {
        let mut rng = StdRng::seed_from_u64(42);
        let dim = 8;
        let config = HnswConfig { ef_construction: 200, ..HnswConfig::default() };
        let mut idx = HnswIndex::new(dim, CosineComputer, config);

        for i in 0..100u64 {
            let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
            let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            for x in &mut v {
                *x /= norm;
            }
            idx.insert(VectorId(i), &v).unwrap();
        }
        idx
    }

    // ── Roundtrip: identity ───────────────────────────────────────────

    #[test]
    fn save_load_roundtrip_preserves_search_results() {
        let idx = populated_index();
        let mut rng = StdRng::seed_from_u64(999);

        let dim = 8;
        let q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
        let qnorm: f64 = q.iter().map(|x| x * x).sum::<f64>().sqrt();
        let q: Vec<f64> = q.iter().map(|x| x / qnorm).collect();

        let results_before = idx.search(&q, 10, None).unwrap();

        // Save to bytes
        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        // Load into fresh index
        let mut loaded = HnswIndex::new(dim, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        // Search after load
        let results_after = loaded.search(&q, 10, None).unwrap();

        assert_eq!(results_before.len(), results_after.len());
        for (a, b) in results_before.iter().zip(results_after.iter()) {
            assert_eq!(a.0, b.0, "IDs should match after roundtrip");
            assert!((a.1 - b.1).abs() < 1e-5, "Distances should match after roundtrip: {} vs {}", a.1, b.1);
        }
    }

    #[test]
    fn save_load_roundtrip_preserves_node_count() {
        let idx = populated_index();
        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        let mut loaded = HnswIndex::new(8, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        assert_eq!(loaded.len(), idx.len());
    }

    #[test]
    fn save_load_roundtrip_preserves_entry_point() {
        let idx = populated_index();
        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        let mut loaded = HnswIndex::new(8, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        // Verify the loaded index actually works (entry point is valid)
        let q = vec![0.5_f64; 8];
        let results = loaded.search(&q, 10, None).unwrap();
        assert!(!results.is_empty());
    }

    // ── Version detection ─────────────────────────────────────────────

    #[test]
    fn load_with_wrong_version_returns_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

        let mut idx = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        let err = idx.load(&mut &buf[..]).unwrap_err();
        assert!(
            matches!(err, Error::GraphCorrupt(ref msg) if msg.contains("expected 1")),
            "Expected GraphCorrupt with version message, got: {}",
            err
        );
    }

    #[test]
    fn load_with_future_version_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());

        let mut idx = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        let err = idx.load(&mut &buf[..]).unwrap_err();
        assert!(
            matches!(err, Error::GraphCorrupt(ref msg) if msg.contains("expected 1")),
            "Expected GraphCorrupt with version message, got: {}",
            err
        );
    }

    // ── Empty index ───────────────────────────────────────────────────

    #[test]
    fn empty_index_save_load() {
        let idx = HnswIndex::<CosineComputer>::new(4, CosineComputer, HnswConfig::default());
        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        let mut loaded = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        assert_eq!(loaded.len(), 0);
        let results = loaded.search(&[0.5, 0.5, 0.5, 0.5], 10, None).unwrap();
        assert!(results.is_empty());
    }

    // ── Snapshot format ───────────────────────────────────────────────

    #[test]
    fn saved_data_starts_with_version_prefix() {
        let idx = populated_index();
        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        assert!(buf.len() >= 4, "must contain at least version prefix");
        let prefix = u32::from_le_bytes(buf[..4].try_into().unwrap());
        assert_eq!(prefix, 1, "version prefix must be 1");
    }

    // ── Tombstone preservation ────────────────────────────────────────

    #[test]
    fn save_load_preserves_tombstones() {
        let mut idx = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        idx.insert(VectorId(1), &[0.9, 0.1, 0.1, 0.1]).unwrap();
        idx.insert(VectorId(2), &[0.1, 0.9, 0.1, 0.1]).unwrap();
        idx.remove(&VectorId(1)).unwrap();

        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        let mut loaded = HnswIndex::new(4, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        // Tombstoned node should be excluded
        let results = loaded.search(&[0.9, 0.1, 0.1, 0.1], 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, VectorId(2));
    }

    // ── Config preservation ───────────────────────────────────────────

    #[test]
    fn save_load_preserves_custom_config() {
        let custom_config =
            HnswConfig { m: 32, m_max0: 64, ef_construction: 400, ml: 1.0 / (32.0_f64).ln(), ..HnswConfig::default() };
        let mut idx = HnswIndex::new(8, CosineComputer, custom_config.clone());
        idx.insert(VectorId(1), &[0.5; 8]).unwrap();

        let mut buf = Vec::new();
        idx.save(&mut buf).unwrap();

        let mut loaded = HnswIndex::new(8, CosineComputer, HnswConfig::default());
        loaded.load(&mut &buf[..]).unwrap();

        assert_eq!(loaded.config.m, 32);
        assert_eq!(loaded.config.m_max0, 64);
        assert_eq!(loaded.config.ef_construction, 400);
    }
}
