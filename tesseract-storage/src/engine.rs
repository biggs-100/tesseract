// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Top-level storage engine facade.
//!
//! Coordinates WAL, hot tier, cold tier, page cache, vector skeleton,
//! index, and the background tier lifecycle manager. All public mutation
//! and query operations flow through this single entry point.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use tesseract_common::error::{Error, Result};
use tesseract_core::projection::WeightMask;
use tesseract_core::types::VectorId;
use tesseract_index::distance::{CosineComputer, EuclideanComputer};
use tesseract_index::hnsw::HnswIndex;
use tesseract_index::topological_index::{AnyIndex, TopologicalIndex};
use tesseract_index::types::DistanceMetric;

use crate::cold_store::ColdStore;
use crate::hot_store::{HotStore, VectorRecord};
use crate::lifecycle::TierLifecycle;
use crate::page_cache::{Page, PageCache, PageKey};
use crate::skeleton::VectorSkeleton;
use crate::types::*;
use crate::wal::{WalEntry, WriteAheadLog};

/// The top-level storage engine that coordinates WAL, tiers, cache,
/// lifecycle, and the ANN index.
#[allow(dead_code)]
pub struct StorageEngine {
    wal: Arc<WriteAheadLog>,
    hot: Arc<HotStore>,
    cold: Arc<ColdStore>,
    skeleton: Arc<VectorSkeleton>,
    cache: Arc<Mutex<PageCache>>,
    config: StorageConfig,
    index: Option<Mutex<AnyIndex>>,
    _lifecycle_handle: Option<tokio::task::JoinHandle<()>>,
}

impl StorageEngine {
    /// Open or create a storage engine at the given path.
    pub async fn open(config: StorageConfig) -> Result<Self> {
        // 1. Open WAL (creates segment files if needed).
        let wal = Arc::new(WriteAheadLog::open(config.wal.clone()).await?);

        // 2. Open cold store (scans partition directory).
        let cold = Arc::new(ColdStore::open(config.cold.clone()).await?);

        // 3. Initialize hot store.
        let hot = Arc::new(HotStore::new(crate::hot_store::HotStoreConfig { max_records: config.hot.max_records }));

        // 4. Initialize skeleton.
        let skeleton = Arc::new(VectorSkeleton::new(config.skeleton.clone()));

        // 5. Initialize page cache.
        let cache = Arc::new(Mutex::new(PageCache::new(config.cache.capacity)));

        // 6. Initialize ANN index (if enabled).
        let index = if config.index.enabled {
            let hnsw_config = config.index.hnsw.clone();
            let dim = config.index.dim;
            let mut inner = match hnsw_config.distance_metric {
                DistanceMetric::Cosine => AnyIndex::Cosine(HnswIndex::new(dim, CosineComputer, hnsw_config)),
                DistanceMetric::Euclidean => AnyIndex::Euclidean(HnswIndex::new(dim, EuclideanComputer, hnsw_config)),
            };

            // Try to load a previously saved index file.
            let index_path = &config.index.path;
            if index_path.exists() {
                let mut file = std::fs::File::open(index_path).map_err(Error::IoError)?;
                inner.load(&mut file)?;
                info!("Loaded index from {}", index_path.display());
            }

            Some(Mutex::new(inner))
        } else {
            None
        };

        // 7. Recover from WAL: replay unflushed entries into hot store and index.
        let recovered = wal.recover().await?;
        if !recovered.is_empty() {
            info!("Recovered {} entries from WAL", recovered.len());
            for entry in &recovered {
                Self::apply_wal_entry(&hot, entry)?;
                if let Some(ref idx_lock) = index {
                    Self::replay_index_entry(idx_lock, entry).await?;
                }
            }
        }

        // 8. Initialize skeleton from cold store partitions.
        for partition in cold.partitions() {
            let records = cold.read_partition(&partition).await?;
            if !records.is_empty() {
                let vectors: Vec<Vec<f64>> = records.iter().map(|r| r.vector.clone()).collect();
                let _ = skeleton.add_partition(partition, &vectors);
            }
        }

        info!("StorageEngine opened: cold_partitions={}", cold.partitions().len());

        // 9. Start lifecycle background task.
        let lifecycle_handle =
            TierLifecycle::start(Arc::clone(&hot), Arc::clone(&cold), Arc::clone(&skeleton), config.lifecycle.clone());

        Ok(Self { wal, hot, cold, skeleton, cache, config, index, _lifecycle_handle: Some(lifecycle_handle) })
    }

    /// Insert a vector with metadata.
    ///
    /// The write flows: WAL append → hot store insert. In durable mode
    /// the WAL entry is fsynced before acknowledging. In fast mode the
    /// entry is acknowledged after the buffer write.
    pub async fn insert(
        &self,
        id: VectorId,
        vector: Vec<f64>,
        metadata: serde_json::Value,
        mode: WriteMode,
    ) -> Result<()> {
        let entry = WalEntry {
            txn_id: crate::types::TransactionId(0), // overridden by WAL
            op_code: OpCode::InsertVector as u8,
            // Use JSON for the payload because serde_json::Value does not
            // roundtrip through bincode (bincode does not support deserialize_any).
            payload: serde_json::to_vec(&(id.clone(), &vector, &metadata))
                .map_err(|e| Error::BincodeError(e.to_string()))?,
        };

        match mode {
            WriteMode::Durable => {
                self.wal.append(entry, WriteMode::Durable).await?;
                // Entry is now fsynced — safe to insert into hot store.
            }
            WriteMode::Fast => {
                self.wal.append(entry, WriteMode::Fast).await?;
                // Entry is in WAL buffer but not fsynced.
            }
        }

        let record = VectorRecord {
            id: id.clone(),
            vector: vector.clone(),
            metadata,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            access_count: 0,
        };

        self.hot.insert(record)?;

        // Insert into ANN index (if enabled) so the vector is searchable
        // immediately.
        if let Some(ref idx_lock) = self.index {
            let mut idx = idx_lock.lock().await;
            idx.insert(id, &vector)?;
        }

        Ok(())
    }

    /// Get a vector by ID.
    ///
    /// Checks the hot tier first, then falls back to scanning cold
    /// partitions via the skeleton centroid index. When a record is
    /// found in the cold tier, the containing page is cached.
    pub async fn get(&self, id: &VectorId) -> Result<Option<VectorRecord>> {
        // 1. Check hot store.
        if let Some(record) = self.hot.get(id) {
            return Ok(Some(record));
        }

        // 2. Search cold store partitions.
        for partition in self.cold.partitions() {
            let records = self.cold.read_partition(&partition).await?;
            if let Some(record) = records.into_iter().find(|r| &r.id == id) {
                // Cache a page representing this partition lookup.
                // In a full implementation, pages would be aligned to
                // batch boundaries.
                let page_key = PageKey { partition_id: partition.0, page_index: 0 };
                if let Ok(page_data) = bincode::serialize(&record) {
                    let cache_page = Page { data: page_data, size: std::mem::size_of::<VectorRecord>() };
                    let cache = self.cache.lock().await;
                    cache.insert(page_key, cache_page);
                }

                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    /// Search the nearest neighbours for a query vector.
    ///
    /// Requires the index to be enabled (see [`IndexConfig::enabled`]).
    /// Returns up to `k` results sorted by distance ascending.
    pub async fn search(&self, query: &[f64], k: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>> {
        match self.index {
            Some(ref idx_lock) => {
                let idx = idx_lock.lock().await;
                idx.search(query, k, mask)
            }
            None => Err(Error::IndexNotBuilt),
        }
    }

    /// Get multiple vectors by their IDs.
    ///
    /// Returns a map of id → record for all found vectors.
    pub async fn batch_get(&self, ids: &[VectorId]) -> Result<std::collections::HashMap<VectorId, VectorRecord>> {
        let mut results = std::collections::HashMap::new();
        for id in ids {
            if let Some(record) = self.hot.get(id) {
                results.insert(id.clone(), record);
            }
        }
        Ok(results)
    }

    /// Perform recovery from WAL, replaying unflushed entries into the hot store
    /// and index.
    ///
    /// Returns the number of entries replayed.
    pub async fn recover(&self) -> Result<usize> {
        let entries = self.wal.recover().await?;
        let count = entries.len();
        for entry in &entries {
            Self::apply_wal_entry(&self.hot, entry)?;
            if let Some(ref idx_lock) = self.index {
                Self::replay_index_entry(idx_lock, entry).await?;
            }
        }
        info!("Recovery: replayed {count} entries into hot store and index");
        Ok(count)
    }

    /// Apply a WAL entry directly to the local store without WAL append.
    ///
    /// Used by followers when receiving replicated entries from the leader.
    /// Deserialises the entry payload and inserts into the hot store and,
    /// if enabled, the ANN index. Does NOT write to the local WAL — the
    /// entry is already durable on the leader.
    pub async fn apply_replicated_entry(&self, entry: &WalEntry) -> Result<()> {
        Self::apply_wal_entry(&self.hot, entry)?;
        if let Some(ref idx_lock) = self.index {
            Self::replay_index_entry(idx_lock, entry).await?;
        }
        Ok(())
    }

    /// Graceful shutdown — persist index and flush WAL.
    pub async fn shutdown(&self) -> Result<()> {
        // Persist index before shutting down so the next session can
        // reload it without a full WAL recovery.
        if let Some(ref idx_lock) = self.index {
            let idx = idx_lock.lock().await;
            let index_path = &self.config.index.path;
            if let Some(parent) = index_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut file = std::fs::File::create(index_path).map_err(Error::IoError)?;
            idx.save(&mut file)?;
            info!("Index saved to {}", index_path.display());
        }

        self.wal.flush().await?;
        info!("StorageEngine shut down");
        Ok(())
    }

    // ─── helpers ──────────────────────────────────────────────────────

    /// Deserialize and apply a single WAL entry to the hot store.
    fn apply_wal_entry(hot: &HotStore, entry: &WalEntry) -> Result<()> {
        if entry.op_code != OpCode::InsertVector as u8 {
            // Only InsertVector is handled in Phase 1.
            return Ok(());
        }

        let (id, vector, metadata) = serde_json::from_slice::<(VectorId, Vec<f64>, serde_json::Value)>(&entry.payload)
            .map_err(|e| Error::BincodeError(e.to_string()))?;

        let record = VectorRecord {
            id,
            vector,
            metadata,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            access_count: 0,
        };

        // Skip duplicates (may happen on re-recovery).
        let _ = hot.insert(record);
        Ok(())
    }

    /// Replay a WAL entry into the ANN index.
    ///
    /// Only `InsertVector` entries are currently indexed. Tombstones and
    /// metadata-only updates are ignored.
    async fn replay_index_entry(idx_lock: &Mutex<AnyIndex>, entry: &WalEntry) -> Result<()> {
        if entry.op_code != OpCode::InsertVector as u8 {
            return Ok(());
        }

        let (id, vector, _metadata): (VectorId, Vec<f64>, serde_json::Value) =
            serde_json::from_slice(&entry.payload).map_err(|e| Error::BincodeError(e.to_string()))?;

        let mut idx = idx_lock.lock().await;
        // Skip duplicates silently (idempotent insert inside HnswIndex).
        let _ = idx.insert(id, &vector);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a minimal `StorageConfig` for testing.
    fn test_config(tmp: &TempDir) -> StorageConfig {
        let root = tmp.path().to_path_buf();
        StorageConfig {
            wal: WalConfig {
                wal_dir: root.join("wal"),
                segment_size: 1024 * 1024,
                fsync_interval_ms: 100,
                fsync_interval_ops: 1000,
            },
            hot: HotStoreConfig { max_records: 100 },
            cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 10 },
            skeleton: SkeletonConfig { wake_threshold: 0.15 },
            cache: PageCacheConfig { capacity: 100 },
            index: IndexConfig {
                enabled: false,
                dim: 4,
                hnsw: tesseract_index::types::HnswConfig::default(),
                path: root.join("index.bin"),
            },
            lifecycle: LifecycleConfig::default(),
        }
    }

    #[tokio::test]
    async fn batch_get_multiple_existing_ids() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();

        // Insert three vectors
        for i in 0..3u64 {
            engine
                .insert(VectorId(i), vec![i as f64; 4], serde_json::json!({"idx": i}), WriteMode::Fast)
                .await
                .unwrap();
        }

        let ids = [VectorId(0), VectorId(1), VectorId(2)];
        let results = engine.batch_get(&ids).await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.contains_key(&VectorId(0)));
        assert!(results.contains_key(&VectorId(1)));
        assert!(results.contains_key(&VectorId(2)));
    }

    #[tokio::test]
    async fn batch_get_mix_of_existing_and_missing() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();

        engine.insert(VectorId(1), vec![1.0; 4], serde_json::json!({}), WriteMode::Fast).await.unwrap();
        engine.insert(VectorId(3), vec![3.0; 4], serde_json::json!({}), WriteMode::Fast).await.unwrap();

        let ids = [VectorId(1), VectorId(2), VectorId(3)];
        let results = engine.batch_get(&ids).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.contains_key(&VectorId(1)));
        assert!(results.contains_key(&VectorId(3)));
        assert!(!results.contains_key(&VectorId(2)));
    }

    #[tokio::test]
    async fn batch_get_empty_ids_returns_empty_map() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();

        engine.insert(VectorId(42), vec![1.0; 4], serde_json::json!({}), WriteMode::Fast).await.unwrap();

        let ids: [VectorId; 0] = [];
        let results = engine.batch_get(&ids).await.unwrap();
        assert!(results.is_empty());
    }
}
