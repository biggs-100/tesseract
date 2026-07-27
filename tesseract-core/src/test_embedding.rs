// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Deterministic embedding service for testing.
//!
//! SHA-256 of input text → first N bytes → `[f64; dim]` → L2 normalized.
//! Only available when the `test-embedding` feature is enabled.

use sha2::{Digest, Sha256};
use tesseract_common::error::Result;

use crate::embedding::EmbeddingService;

/// A deterministic embedding service for use in tests.
///
/// Produces the same embedding vector for the same text every time,
/// making it suitable for reproducible integration and E2E tests.
pub struct TestEmbeddingService {
    dim: usize,
}

impl TestEmbeddingService {
    /// Create a new `TestEmbeddingService` with the given dimension.
    ///
    /// The dimension is clamped to at most 32 (SHA-256 output length).
    /// Default is 128 (clamped to 32).
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }

    /// Return the default embedding dimension (128).
    pub const fn default_dim() -> usize {
        128
    }
}

#[async_trait::async_trait]
impl EmbeddingService for TestEmbeddingService {
    async fn embed(&self, text: &str, _model: &str) -> Result<Vec<f64>> {
        let hash = Sha256::digest(text.as_bytes());

        // SHA-256 produces 32 bytes; dim is clamped to at most 32.
        let dim = self.dim.min(32);

        let mut vec: Vec<f64> = hash.iter().take(dim).map(|&b| b as f64).collect();

        // L2 normalize
        let norm = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 0.0 {
            for x in &mut vec {
                *x /= norm;
            }
        }

        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::EmbeddingService;

    #[tokio::test]
    async fn deterministic_embedding() {
        let svc = TestEmbeddingService::new(32);
        let v1 = svc.embed("hello world", "test").await.unwrap();
        let v2 = svc.embed("hello world", "test").await.unwrap();
        assert_eq!(v1, v2, "same text must produce identical vectors");
    }

    #[tokio::test]
    async fn different_texts_differ() {
        let svc = TestEmbeddingService::new(32);
        let v1 = svc.embed("cat", "test").await.unwrap();
        let v2 = svc.embed("dog", "test").await.unwrap();
        assert_ne!(v1, v2, "different texts must produce different vectors");
    }

    #[tokio::test]
    async fn l2_normalized() {
        let svc = TestEmbeddingService::new(32);
        let vec = svc.embed("test vector", "test").await.unwrap();
        let norm: f64 = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
        let diff = (norm - 1.0).abs();
        assert!(
            diff < 1e-6,
            "L2 norm must be approximately 1.0, got {norm} (diff={diff})"
        );
    }

    #[tokio::test]
    async fn dimension_configurable() {
        let svc = TestEmbeddingService::new(16);
        let vec = svc.embed("dim test", "test").await.unwrap();
        assert_eq!(vec.len(), 16, "vector dimension should be 16");
    }

    #[tokio::test]
    async fn dimension_clamped_to_32() {
        let svc = TestEmbeddingService::new(128);
        let vec = svc.embed("clamp test", "test").await.unwrap();
        assert_eq!(
            vec.len(),
            32,
            "vector dimension should be clamped to 32 (SHA-256 byte length)"
        );
    }

    #[tokio::test]
    async fn default_dim_is_128() {
        assert_eq!(TestEmbeddingService::default_dim(), 128);
    }

    #[tokio::test]
    async fn empty_text_produces_vector() {
        let svc = TestEmbeddingService::new(32);
        let vec = svc.embed("", "test").await.unwrap();
        assert_eq!(vec.len(), 32);
        // SHA-256 of empty string is well-defined, so this should not fail
        let norm: f64 = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
        let diff = (norm - 1.0).abs();
        assert!(
            diff < 1e-6,
            "empty text vector must be normalized, got norm={norm}"
        );
    }
}
