// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use dashmap::{DashMap, Entry};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use tesseract_common::error::{Error, Result};
use tesseract_core::types::VectorId;

/// A record stored in the hot tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorRecord {
    pub id: VectorId,
    pub vector: Vec<f64>,
    pub metadata: serde_json::Value,
    pub created_at: u64,
    pub access_count: u64,
}

/// Configuration for the hot store.
#[derive(Debug, Clone)]
pub struct HotStoreConfig {
    /// Maximum number of entries before eviction (0 = unlimited).
    pub max_records: usize,
}

impl Default for HotStoreConfig {
    fn default() -> Self {
        Self { max_records: 1_000_000 }
    }
}

/// Fast in-memory store backed by DashMap for concurrent reads/writes.
pub struct HotStore {
    vectors: Arc<DashMap<VectorId, VectorRecord>>,
    #[allow(dead_code)]
    config: HotStoreConfig,
}

impl HotStore {
    /// Create a new hot store with the given configuration.
    pub fn new(config: HotStoreConfig) -> Self {
        Self { vectors: Arc::new(DashMap::new()), config }
    }

    /// Insert a vector record.
    ///
    /// Returns `Err(AlreadyExists)` if a record with the same `VectorId`
    /// is already present.
    pub fn insert(&self, record: VectorRecord) -> Result<()> {
        let id = record.id.clone();
        match self.vectors.entry(id) {
            Entry::Occupied(_) => {
                Err(Error::AlreadyExists(format!("VectorId {} already exists in hot store", record.id.0)))
            }
            Entry::Vacant(entry) => {
                entry.insert(record);
                Ok(())
            }
        }
    }

    /// Retrieve a vector record by `VectorId`.
    ///
    /// Returns `None` if the id is not present.
    pub fn get(&self, id: &VectorId) -> Option<VectorRecord> {
        self.vectors.get(id).map(|r| r.clone())
    }

    /// Delete a vector record by `VectorId`.
    ///
    /// Returns `true` if the record was removed, `false` if it did not exist.
    pub fn delete(&self, id: &VectorId) -> bool {
        self.vectors.remove(id).is_some()
    }

    /// Flush a batch of records for cold tier transfer.
    ///
    /// Returns up to `max_count` records, ordered by least-accessed first
    /// (ascending `access_count`). Removes the returned records from the store.
    pub fn drain_least_accessed(&self, max_count: usize) -> Vec<VectorRecord> {
        // Collect ids sorted by access_count (ascending)
        let mut entries: Vec<(VectorId, u64)> =
            self.vectors.iter().map(|r| (r.key().clone(), r.access_count)).collect();

        // Fast path: nothing to drain
        if entries.is_empty() {
            return Vec::new();
        }

        entries.sort_by_key(|(_, count)| *count);
        entries.truncate(max_count);

        // Remove from map and collect records
        entries.into_iter().filter_map(|(id, _)| self.vectors.remove(&id)).map(|(_, record)| record).collect()
    }

    /// Number of records currently in the hot store.
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Returns `true` if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Clear all records. Primarily for testing.
    pub fn clear(&self) {
        self.vectors.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: u64, access_count: u64) -> VectorRecord {
        VectorRecord {
            id: VectorId(id),
            vector: vec![1.0, 2.0, 3.0],
            metadata: serde_json::json!({"label": "test"}),
            created_at: 1000,
            access_count,
        }
    }

    #[test]
    fn insert_and_get() {
        let store = HotStore::new(HotStoreConfig::default());
        let record = make_record(42, 0);
        store.insert(record.clone()).unwrap();

        let result = store.get(&VectorId(42));
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, VectorId(42));
    }

    #[test]
    fn get_non_existent_returns_none() {
        let store = HotStore::new(HotStoreConfig::default());
        assert!(store.get(&VectorId(99)).is_none());
    }

    #[test]
    fn insert_duplicate_returns_err() {
        let store = HotStore::new(HotStoreConfig::default());
        store.insert(make_record(1, 0)).unwrap();
        let err = store.insert(make_record(1, 5)).unwrap_err();
        match err {
            Error::AlreadyExists(msg) => assert!(msg.contains("already exists")),
            _ => panic!("expected AlreadyExists, got {err}"),
        }
    }

    #[test]
    fn delete_existing_returns_true() {
        let store = HotStore::new(HotStoreConfig::default());
        store.insert(make_record(7, 0)).unwrap();
        assert!(store.delete(&VectorId(7)));
        assert!(store.get(&VectorId(7)).is_none());
    }

    #[test]
    fn delete_non_existent_returns_false() {
        let store = HotStore::new(HotStoreConfig::default());
        assert!(!store.delete(&VectorId(999)));
    }

    #[test]
    fn drain_least_accessed_ordering() {
        let store = HotStore::new(HotStoreConfig::default());

        for i in 0..5u64 {
            store
                .insert(make_record(i, i * 10)) // access_counts: 0, 10, 20, 30, 40
                .unwrap();
        }

        let drained = store.drain_least_accessed(3);
        assert_eq!(drained.len(), 3);
        // Should return lowest access_count first
        assert_eq!(drained[0].access_count, 0);
        assert_eq!(drained[1].access_count, 10);
        assert_eq!(drained[2].access_count, 20);
        // Remaining two should still be in the store
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn drain_returns_all_when_less_than_max() {
        let store = HotStore::new(HotStoreConfig::default());
        store.insert(make_record(1, 5)).unwrap();
        store.insert(make_record(2, 3)).unwrap();

        let drained = store.drain_least_accessed(10);
        assert_eq!(drained.len(), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn len_after_inserts_and_deletes() {
        let store = HotStore::new(HotStoreConfig::default());
        assert_eq!(store.len(), 0);

        store.insert(make_record(1, 0)).unwrap();
        store.insert(make_record(2, 0)).unwrap();
        store.insert(make_record(3, 0)).unwrap();
        assert_eq!(store.len(), 3);

        store.delete(&VectorId(2));
        assert_eq!(store.len(), 2);

        store.clear();
        assert_eq!(store.len(), 0);
    }

    #[tokio::test]
    async fn concurrent_insert_no_data_loss() {
        let store = Arc::new(HotStore::new(HotStoreConfig::default()));
        let mut handles = vec![];

        for i in 0..8 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                for j in 0..100 {
                    let id = VectorId((i * 100 + j) as u64);
                    let record = VectorRecord {
                        id: id.clone(),
                        vector: vec![j as f64; 4],
                        metadata: serde_json::json!({"source": "test", "task": i}),
                        created_at: 0,
                        access_count: 0,
                    };
                    let _ = store.insert(record);
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(store.len(), 800);
    }

    #[test]
    fn clear_empties_store() {
        let store = HotStore::new(HotStoreConfig::default());
        store.insert(make_record(1, 0)).unwrap();
        store.insert(make_record(2, 0)).unwrap();
        assert!(!store.is_empty());

        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }
}
