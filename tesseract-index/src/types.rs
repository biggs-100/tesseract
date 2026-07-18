// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};

/// HNSW graph configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswConfig {
    /// Maximum number of connections per node per layer (default: 16).
    pub m: usize,
    /// Maximum connections for layer 0 (default: 2 * M).
    pub m_max0: usize,
    /// Size of the dynamic candidate list during construction (default: 200).
    pub ef_construction: usize,
    /// Normalization factor for level generation: 1/ln(M).
    pub ml: f64,
    /// Distance metric: "cosine" or "euclidean".
    pub distance_metric: DistanceMetric,
}

/// Supported distance metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceMetric {
    /// Cosine distance: 1.0 - dot(a, b). Vectors must be L2-normalized.
    Cosine,
    /// Euclidean distance: sqrt(Σ(a - b)²).
    Euclidean,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m_max0: 32,
            ef_construction: 200,
            ml: 1.0 / (16.0_f64).ln(),
            distance_metric: DistanceMetric::Cosine,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = HnswConfig::default();
        assert_eq!(cfg.m, 16);
        assert_eq!(cfg.m_max0, 32);
        assert_eq!(cfg.ef_construction, 200);
        assert_eq!(cfg.distance_metric, DistanceMetric::Cosine);
    }

    #[test]
    fn config_bincode_roundtrip() {
        let cfg = HnswConfig::default();
        let bytes = bincode::serialize(&cfg).unwrap();
        let deserialized: HnswConfig = bincode::deserialize(&bytes).unwrap();
        assert_eq!(cfg.m, deserialized.m);
        assert_eq!(cfg.distance_metric, deserialized.distance_metric);
    }

    #[test]
    fn custom_config_values() {
        let cfg = HnswConfig {
            m: 32,
            m_max0: 64,
            ef_construction: 400,
            ml: 1.0 / (32.0_f64).ln(),
            distance_metric: DistanceMetric::Euclidean,
        };
        assert_eq!(cfg.m, 32);
        assert_eq!(cfg.m_max0, 64);
        assert_eq!(cfg.distance_metric, DistanceMetric::Euclidean);
    }

    #[test]
    fn distance_metric_debug_and_eq() {
        assert_eq!(DistanceMetric::Cosine as usize, 0);
        assert_eq!(DistanceMetric::Euclidean as usize, 1);
        assert_ne!(DistanceMetric::Cosine, DistanceMetric::Euclidean);
    }
}
