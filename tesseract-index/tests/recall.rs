// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Integration tests verifying HNSW recall against brute-force search.

use rand::Rng;
use tesseract_core::types::VectorId;
use tesseract_index::distance::CosineComputer;
use tesseract_index::hnsw::HnswIndex;
use tesseract_index::types::HnswConfig;

/// Brute-force k-NN search using cosine distance.
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

#[test]
fn test_recall_vs_bruteforce() {
    let mut rng = rand::thread_rng();
    let dim = 16;
    let n_vectors = 500;
    let n_queries = 20;
    let recall_k = 10;

    let config = HnswConfig { ef_construction: 200, ..HnswConfig::default() };
    let mut index = HnswIndex::<CosineComputer>::new(dim, CosineComputer, config);

    // Generate and insert vectors
    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(n_vectors);
    for i in 0..n_vectors {
        let mut v: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in &mut v {
            *x /= norm;
        }
        let v32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
        vectors.push(v32);
        index.insert(VectorId(i as u64), &v).unwrap();
    }

    let skip = vec![false; vectors.len()];
    let mut total_recall = 0.0_f64;

    for _ in 0..n_queries {
        // Generate a random query vector
        let mut q: Vec<f64> = (0..dim).map(|_| rng.r#gen::<f64>()).collect();
        let qnorm: f64 = q.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in &mut q {
            *x /= qnorm;
        }

        // HNSW search
        let results = index.search(&q, 200, None).unwrap();
        let q32: Vec<f32> = q.iter().map(|&x| x as f32).collect();

        // Brute-force search
        let brute = brute_force(&vectors, &q32, recall_k, &skip);

        // Compute recall@k
        let hnsw_ids: Vec<u64> = results.iter().take(recall_k).map(|(id, _)| id.0).collect();
        let brute_ids: Vec<u64> = brute.iter().map(|(i, _)| *i as u64).collect();
        let intersection = hnsw_ids.iter().filter(|id| brute_ids.contains(id)).count();
        total_recall += intersection as f64 / recall_k as f64;
    }

    let avg_recall = total_recall / n_queries as f64;
    assert!(avg_recall >= 0.9, "recall@10 too low: {:.4} (threshold: 0.9)", avg_recall);
}
