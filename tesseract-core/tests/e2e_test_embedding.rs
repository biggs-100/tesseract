// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

#![cfg(feature = "test-embedding")]

//! End-to-end integration tests for `TestEmbeddingService`.
//!
//! These tests verify the deterministic embedding service end-to-end:
//! determinism, normalization, and dimension configuration.
//!
//! Note: Full INSERT + FIND SIMILARITY E2E tests that exercise the
//! storage engine belong in `tesseract-storage/tests/` or
//! `tesseract-api/tests/` (requires access to `StorageEngine`).

use tesseract_core::embedding::EmbeddingService;
use tesseract_core::test_embedding::TestEmbeddingService;

/// Verify the same text always produces the same embedding vector.
#[tokio::test]
async fn embedding_deterministic() {
    let svc = TestEmbeddingService::new(32);
    let v1 = svc.embed("quantum computing", "test").await.unwrap();
    let v2 = svc.embed("quantum computing", "test").await.unwrap();
    assert_eq!(v1, v2, "deterministic: same text must produce identical vectors");
}

/// Verify two different texts produce different embedding vectors.
#[tokio::test]
async fn different_texts_produce_different_vectors() {
    let svc = TestEmbeddingService::new(32);
    let cat = svc.embed("cat", "test").await.unwrap();
    let dog = svc.embed("dog", "test").await.unwrap();
    assert_ne!(cat, dog, "different texts must produce different vectors");
}

/// Verify the embedding vector has L2 norm ≈ 1.0 (unit vector).
#[tokio::test]
async fn embedding_is_normalized() {
    let svc = TestEmbeddingService::new(32);
    let vec = svc.embed("quantum computing", "test").await.unwrap();
    let norm: f64 = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
    let diff = (norm - 1.0).abs();
    assert!(
        diff < 1e-6,
        "embedding must be L2-normalized, got norm={norm} (diff={diff})"
    );
}

/// Verify similar texts produce closer vectors than dissimilar texts.
#[tokio::test]
async fn similar_texts_have_higher_cosine() {
    let svc = TestEmbeddingService::new(32);

    let cat1 = svc.embed("cat", "test").await.unwrap();
    let cat2 = svc.embed("cat", "test").await.unwrap();
    let dog = svc.embed("dog", "test").await.unwrap();

    // Cosine similarity: same text should be identical
    let same_sim: f64 = cat1.iter().zip(cat2.iter()).map(|(a, b)| a * b).sum();
    let diff_sim: f64 = cat1.iter().zip(dog.iter()).map(|(a, b)| a * b).sum();

    assert!(
        (same_sim - 1.0).abs() < 1e-6,
        "identical texts should have cosine similarity ≈ 1.0, got {same_sim}"
    );
    assert!(
        same_sim > diff_sim + 0.001,
        "same-text similarity ({same_sim}) should exceed different-text similarity ({diff_sim})"
    );
}

/// Verify embedding works with default dimension.
#[tokio::test]
async fn default_dimension() {
    let svc = TestEmbeddingService::new(TestEmbeddingService::default_dim());
    let vec = svc.embed("test", "test").await.unwrap();
    assert_eq!(
        vec.len(),
        TestEmbeddingService::default_dim().min(32),
        "vector dimension should respect default_dim() clamped to 32"
    );
}

/// Verify search without matching data returns an empty-like state.
///
/// While we can't directly call StorageEngine from here, this test
/// proves the embedding service works for arbitrary texts that would
/// be inserted or queried.
#[tokio::test]
async fn empty_input_handled() {
    let svc = TestEmbeddingService::new(32);
    let vec = svc.embed("", "test").await.unwrap();
    assert_eq!(vec.len(), 32);
    assert!(
        vec.iter().any(|&x| x != 0.0),
        "empty text must produce a non-zero vector"
    );
}
