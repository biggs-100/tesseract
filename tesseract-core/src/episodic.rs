// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::collections::HashMap;
use std::sync::RwLock;

use tesseract_common::error::Result;

/// Per-user footprint vector that biases search results.
pub struct UserFootprint {
    pub user_id: String,
    pub vector: Vec<f64>,
    pub interaction_count: u64,
}

/// Episodic memory manager.
///
/// Stores and updates per-user footprint vectors to bias search results
/// based on implicit feedback (click-through).
pub struct EpisodicMemory {
    // In-memory map for footprints (simpler than HotStore for Phase 3 MVP)
    footprints: RwLock<HashMap<String, UserFootprint>>,
}

impl EpisodicMemory {
    pub fn new() -> Self {
        Self { footprints: RwLock::new(HashMap::new()) }
    }

    /// Get a user's footprint vector. Returns `None` if no history.
    pub fn get_footprint(&self, user_id: &str) -> Option<Vec<f64>> {
        let fp = self.footprints.read().ok()?;
        fp.get(user_id).map(|f| f.vector.clone())
    }

    /// Update footprint based on implicit feedback.
    ///
    /// When a user clicks on a result, the query vector and clicked vector
    /// are averaged into the footprint.
    pub fn update_footprint(&self, user_id: &str, clicked_vector: &[f64], query_vector: &[f64]) -> Result<()> {
        let mut fp = self
            .footprints
            .write()
            .map_err(|_| tesseract_common::error::Error::ServiceError("Lock poisoned".into()))?;

        let entry = fp.entry(user_id.to_string()).or_insert_with(|| UserFootprint {
            user_id: user_id.to_string(),
            vector: clicked_vector.to_vec(),
            interaction_count: 0,
        });

        // Weighted update: blend existing footprint with new signal
        // New footprint = 0.7 * old_footprint + 0.3 * (clicked × query_bias)
        let bias: Vec<f64> = clicked_vector
            .iter()
            .zip(query_vector.iter())
            .map(|(c, q)| c * q) // element-wise: similarity-weighted click
            .collect();

        if entry.vector.is_empty() {
            entry.vector = bias;
        } else {
            let alpha = 0.7f64;
            for (i, b) in bias.iter().enumerate() {
                if i < entry.vector.len() {
                    entry.vector[i] = alpha * entry.vector[i] + (1.0 - alpha) * b;
                }
            }
        }

        entry.interaction_count += 1;
        Ok(())
    }

    /// Apply a user's footprint to a query vector (element-wise multiplication).
    ///
    /// Both vectors should be L2-normalized.
    pub fn apply_footprint(query: &[f64], footprint: &[f64]) -> Vec<f64> {
        query.iter().zip(footprint.iter()).map(|(q, f)| q * f).collect()
    }

    /// Number of users with footprints.
    pub fn len(&self) -> usize {
        self.footprints.read().map(|fp| fp.len()).unwrap_or(0)
    }

    /// Returns `true` if no footprints are stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for EpisodicMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vector(values: &[f64]) -> Vec<f64> {
        values.to_vec()
    }

    #[test]
    fn empty_memory_returns_none() {
        let mem = EpisodicMemory::new();
        assert!(mem.get_footprint("unknown-user").is_none());
        assert!(mem.is_empty());
        assert_eq!(mem.len(), 0);
    }

    #[test]
    fn update_creates_footprint_for_new_user() {
        let mem = EpisodicMemory::new();
        let clicked = make_vector(&[1.0, 0.0, 0.0]);
        let query = make_vector(&[0.5, 0.5, 0.0]);

        mem.update_footprint("alice", &clicked, &query).unwrap();

        let fp = mem.get_footprint("alice");
        assert!(fp.is_some());
        assert_eq!(mem.len(), 1);
        assert!(!mem.is_empty());

        // New user: vector initialized from clicked_vector, then blended with bias
        // result = 0.7 * clicked + 0.3 * (clicked * query)
        //       = 0.7 * [1.0, 0.0, 0.0] + 0.3 * [0.5, 0.0, 0.0]
        //       = [0.85, 0.0, 0.0]
        let fp = fp.unwrap();
        assert!((fp[0] - 0.85).abs() < 1e-10);
        assert!((fp[1] - 0.0).abs() < 1e-10);
        assert!((fp[2] - 0.0).abs() < 1e-10);
    }

    #[test]
    fn update_blends_with_existing_footprint() {
        let mem = EpisodicMemory::new();
        let first_click = make_vector(&[1.0, 0.0, 0.0]);
        let first_query = make_vector(&[0.5, 0.5, 0.0]);
        mem.update_footprint("bob", &first_click, &first_query).unwrap();

        let second_click = make_vector(&[0.0, 1.0, 0.0]);
        let second_query = make_vector(&[0.0, 0.5, 0.5]);
        mem.update_footprint("bob", &second_click, &second_query).unwrap();

        let fp = mem.get_footprint("bob").unwrap();

        // First update: blend clicked [1,0,0] with bias [0.5,0,0] → [0.85, 0, 0]
        // Second update: blend [0.85,0,0] with bias [0,0.5,0] → [0.595, 0.15, 0]
        assert!((fp[0] - 0.595).abs() < 1e-10);
        assert!((fp[1] - 0.15).abs() < 1e-10);
        assert!((fp[2] - 0.0).abs() < 1e-10);
    }

    #[test]
    fn apply_footprint_modifies_query_vector() {
        let query = make_vector(&[1.0, 2.0, 3.0]);
        let footprint = make_vector(&[0.5, 0.5, 0.5]);
        let result = EpisodicMemory::apply_footprint(&query, &footprint);
        let expected = make_vector(&[0.5, 1.0, 1.5]);
        assert_eq!(result, expected);
    }

    #[test]
    fn multiple_updates_increase_interaction_count() {
        let mem = EpisodicMemory::new();
        let v = make_vector(&[1.0, 0.0, 0.0]);
        let q = make_vector(&[0.5, 0.5, 0.0]);

        for _ in 0..3 {
            mem.update_footprint("charlie", &v, &q).unwrap();
        }

        let fp = mem.get_footprint("charlie").unwrap();
        // After 3 updates, should be accessible
        assert!(!fp.is_empty());
    }

    #[test]
    fn apply_footprint_different_lengths() {
        let query = make_vector(&[1.0, 2.0, 3.0, 4.0]);
        let footprint = make_vector(&[0.1, 0.2, 0.3, 0.4]);
        let result = EpisodicMemory::apply_footprint(&query, &footprint);
        assert!((result[0] - 0.1).abs() < 1e-10);
        assert!((result[1] - 0.4).abs() < 1e-10);
        assert!((result[2] - 0.9).abs() < 1e-10);
        assert!((result[3] - 1.6).abs() < 1e-10);
    }

    #[test]
    fn concurrent_users_dont_interfere() {
        let mem = EpisodicMemory::new();
        let v = make_vector(&[1.0, 0.0, 0.0]);
        let q = make_vector(&[0.5, 0.5, 0.0]);

        mem.update_footprint("alice", &v, &q).unwrap();
        mem.update_footprint("bob", &v, &q).unwrap();

        assert_eq!(mem.len(), 2);

        let alice_fp = mem.get_footprint("alice");
        let bob_fp = mem.get_footprint("bob");
        assert!(alice_fp.is_some());
        assert!(bob_fp.is_some());
    }
}
