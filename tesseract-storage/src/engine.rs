// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Top-level storage engine facade.
//!
//! Coordinates WAL, hot tier, cold tier, page cache, vector skeleton,
//! index, and the background tier lifecycle manager. All public mutation
//! and query operations flow through this single entry point.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::Mutex;

/// Internal alias for the index lock.
///
/// By default uses `tokio::sync::RwLock` for concurrent reads + exclusive writes.
/// With `legacy-locking` feature, uses `tokio::sync::Mutex` (serializes all access).
#[cfg(not(feature = "legacy-locking"))]
type IndexLock = tokio::sync::RwLock<AnyIndex>;
#[cfg(feature = "legacy-locking")]
type IndexLock = tokio::sync::Mutex<AnyIndex>;
use tracing::{info, warn};

use tesseract_common::error::{Error, Result};
use tesseract_core::projection::WeightMask;
use tesseract_core::topological::{self, CentroidTracker, CorrelationTracker, NumericalBucketTracker};
use tesseract_core::types::VectorId;
use tesseract_index::distance::{CosineComputer, EuclideanComputer};
use tesseract_index::hnsw::HnswIndex;
use tesseract_index::merkle::{HotBuffer, MerkleTree};
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
pub struct StorageEngine {
    wal: Arc<WriteAheadLog>,
    hot: Arc<HotStore>,
    cold: Arc<ColdStore>,
    cache: Arc<Mutex<PageCache>>,
    config: StorageConfig,
    index: Option<IndexLock>,
    _lifecycle_handle: Option<tokio::task::JoinHandle<()>>,
    /// Topological centroid tracker for categorical metadata fields.
    centroids: Option<std::sync::Mutex<CentroidTracker>>,
    /// Topological correlation tracker for numerical metadata fields.
    correlations: Option<std::sync::Mutex<CorrelationTracker>>,
    /// Topological bucket tracker for numerical fields with configured
    /// bucket boundaries. Falls back to correlation when no buckets exist.
    buckets: Option<std::sync::Mutex<NumericalBucketTracker>>,
    /// Hot buffer for recent inserts (progressive Merkle tree tier).
    hot_buffer: Option<std::sync::Mutex<HotBuffer>>,
    /// Progressive Merkle tree for merged centroids.
    merkle_tree: Option<std::sync::Mutex<MerkleTree>>,
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
        let cache = Arc::new(Mutex::new(PageCache::new(config.cache.capacity)?));

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

            Some(IndexLock::new(inner))
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
                    #[cfg(not(feature = "legacy-locking"))]
                    let mut idx = idx_lock.write().await;
                    #[cfg(feature = "legacy-locking")]
                    let mut idx = idx_lock.lock().await;
                    Self::replay_index_entry_inner(&mut idx, entry).await?;
                }
            }
        }

        // 8. Initialize skeleton from cold store partitions.
        for partition in cold.partitions()? {
            let records = cold.read_partition(&partition).await?;
            if !records.is_empty() {
                let vectors: Vec<Vec<f64>> = records.iter().map(|r| r.vector.clone()).collect();
                let _ = skeleton.add_partition(partition, &vectors);
            }
        }

        info!("StorageEngine opened: cold_partitions={}", cold.partitions()?.len());

        // 9. Initialize topological bias trackers (if enabled).
        let centroids = if config.topological.enabled {
            let dim = config.index.dim;
            info!(
                "Topological bias enabled: {} categorical fields, {} numerical fields",
                config.topological.categorical_fields.len(),
                config.topological.numerical_fields.len()
            );
            Some(std::sync::Mutex::new(CentroidTracker::new(dim)))
        } else {
            None
        };

        let correlations = if config.topological.enabled {
            let dim = config.index.dim;
            Some(std::sync::Mutex::new(CorrelationTracker::new(dim)))
        } else {
            None
        };

        let buckets = if config.topological.enabled {
            let dim = config.index.dim;
            let mut bt = NumericalBucketTracker::new(dim);
            let n_bucketed = config.topological.numerical_buckets.len();
            for (field, boundaries) in &config.topological.numerical_buckets {
                bt.register_field(field, boundaries.clone())?;
            }
            if n_bucketed > 0 {
                info!("Bucketized centroids enabled for {} numerical fields", n_bucketed);
            }
            Some(std::sync::Mutex::new(bt))
        } else {
            None
        };

        // 10. Initialize Merkle tree / hot buffer (if enabled).
        let hot_buffer = if config.merkle.enabled {
            Some(std::sync::Mutex::new(HotBuffer::new(config.merkle.hot_buffer_capacity)))
        } else {
            None
        };

        let merkle_tree = if config.merkle.enabled {
            let mt_path = config.merkle.merkle_tree_path.clone();
            let mt = match mt_path.as_ref().filter(|p| p.exists()) {
                Some(path) => {
                    match MerkleTree::load(path) {
                        Ok(tree) => {
                            info!("Loaded Merkle tree from {}", path.display());
                            tree
                        }
                        Err(e) => {
                            info!("Could not load Merkle tree ({}), creating new one", e);
                            MerkleTree::new(config.merkle.max_cluster_size)
                        }
                    }
                }
                None => MerkleTree::new(config.merkle.max_cluster_size),
            };
            Some(std::sync::Mutex::new(mt))
        } else {
            None
        };

        // 11. Start lifecycle background task.
        let lifecycle_handle =
            TierLifecycle::start(Arc::clone(&hot), Arc::clone(&cold), Arc::clone(&skeleton), config.lifecycle.clone());

        Ok(Self {
            wal,
            hot,
            cold,
            cache,
            config,
            index,
            _lifecycle_handle: Some(lifecycle_handle),
            centroids,
            correlations,
            buckets,
            hot_buffer,
            merkle_tree,
        })
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
                .map_err(|e| Error::JsonError(e.to_string()))?,
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

        // Update topological bias trackers BEFORE metadata is moved
        // into VectorRecord (borrow check).
        if let Some(ref centroids_lock) = self.centroids {
            let mut c = centroids_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            c.update(&vector, &metadata, &self.config.topological.categorical_fields);
        }
        if let Some(ref correlations_lock) = self.correlations {
            let mut c = correlations_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            for field in &self.config.topological.numerical_fields {
                if let Some(val) = metadata.get(field) {
                    if let Some(num) = val.as_f64() {
                        c.update(field, num, &vector);
                    }
                }
            }
        }
        // Update bucketized centroid tracker for fields with configured buckets
        if let Some(ref buckets_lock) = self.buckets {
            let mut b = buckets_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            for field in self.config.topological.numerical_buckets.keys() {
                if let Some(val) = metadata.get(field) {
                    if let Some(num) = val.as_f64() {
                        b.update(field, num, &vector);
                    }
                }
            }
        }

        let record = VectorRecord {
            id: id.clone(),
            vector: vector.clone(),
            metadata: metadata.clone(),
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
            #[cfg(not(feature = "legacy-locking"))]
            let mut idx = idx_lock.write().await;
            #[cfg(feature = "legacy-locking")]
            let mut idx = idx_lock.lock().await;
            idx.insert(id.clone(), &vector)?;
        }

        // Insert into HotBuffer (if Merkle tree is enabled).
        if let Some(ref buffer_lock) = self.hot_buffer {
            let mut buffer = buffer_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            let vector_f32: Vec<f32> = vector.iter().map(|&x| x as f32).collect();
            let is_full = buffer.insert(id.0, vector_f32, metadata.clone());

            // If buffer is full, trigger an async merge into the Merkle tree.
            if is_full {
                // Set merging flag to prevent concurrent merges.
                if !buffer.merging.swap(true, Ordering::AcqRel) {
                    let snapshot = buffer.drain();
                    if let Some(ref tree_lock) = self.merkle_tree {
                        let mut tree = tree_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                        tree.insert_batch(&snapshot);
                        info!(
                            "Merkle merge complete: {} vectors merged, {} centroids",
                            snapshot.len(),
                            tree.num_centroids()
                        );
                        // Persist if path is configured.
                        if let Some(path) = &self.config.merkle.merkle_tree_path {
                            if let Err(e) = tree.save(path) {
                                tracing::warn!("Failed to persist Merkle tree: {}", e);
                            }
                        }
                    }
                    buffer.merging.store(false, Ordering::Release);
                }
            }
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
        for partition in self.cold.partitions()? {
            let records = self.cold.read_partition(&partition).await?;
            if let Some(record) = records.into_iter().find(|r| &r.id == id) {
                // Cache a page representing this partition lookup.
                // In a full implementation, pages would be aligned to
                // batch boundaries.
                let page_key = PageKey { partition_id: partition.0, page_index: 0 };
                if let Ok(page_data) = bincode::serialize(&record) {
                    let cache_page = Page { data: page_data, size: std::mem::size_of::<VectorRecord>() };
                    let cache = self.cache.lock().await;
                    let _ = cache.insert(page_key, cache_page);
                }

                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    /// Search the nearest neighbours for a query vector.
    ///
    /// When the Merkle tree is enabled, this combines results from:
    /// 1. The HNSW index (existing, merged data)
    /// 2. The hot buffer (recent inserts, immediately queryable)
    /// 3. The Merkle tree centroid index
    ///
    /// Returns up to `k` results sorted by distance ascending, deduplicated
    /// by `VectorId`.
    pub async fn search(&self, query: &[f64], k: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>> {
        let query_f32: Vec<f32> = query.iter().map(|&x| x as f32).collect();
        let mut all_results: Vec<(VectorId, f32)> = Vec::new();

        // 1. Search existing HNSW index.
        if let Some(ref idx_lock) = self.index {
            #[cfg(not(feature = "legacy-locking"))]
            let idx = idx_lock.read().await;
            #[cfg(feature = "legacy-locking")]
            let idx = idx_lock.lock().await;
            let hnsw_results = idx.search(query, k, mask)?;
            all_results.extend(hnsw_results);
        }

        // 2. Search HotBuffer if active.
        if let Some(ref buffer_lock) = self.hot_buffer {
            let buffer = buffer_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            if !buffer.is_empty() {
                let hot_results = buffer.search(&query_f32, k);
                all_results.extend(hot_results.into_iter().map(|(id, score)| (VectorId(id), score)));
            }
        }

        // 3. Search MerkleTree if available.
        if let Some(ref tree_lock) = self.merkle_tree {
            let tree = tree_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
            if tree.num_centroids() > 0 {
                let tree_results = tree.search(&query_f32, k);
                all_results.extend(tree_results.into_iter().map(|(_cluster_id, score)| {
                    // Tree search returns centroid-level results.
                    // In a full integration, per-cluster HNSW search would
                    // return actual VectorIds. For now, use cluster_ids as
                    // approximations.
                    (VectorId(_cluster_id), score)
                }));
            }
        }

        // If no index, no hot buffer, and no merkle tree, return an error.
        if all_results.is_empty() && self.index.is_none() {
            return Err(Error::IndexNotBuilt);
        }

        // 4. Sort by distance ascending, dedup by id, take top-k.
        all_results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        // Dedup: keep first occurrence (closest) when same id appears.
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|(id, _)| seen.insert(id.clone()));
        all_results.truncate(k);

        Ok(all_results)
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
                #[cfg(not(feature = "legacy-locking"))]
                let mut idx = idx_lock.write().await;
                #[cfg(feature = "legacy-locking")]
                let mut idx = idx_lock.lock().await;
                Self::replay_index_entry_inner(&mut idx, entry).await?;
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
            #[cfg(not(feature = "legacy-locking"))]
            let mut idx = idx_lock.write().await;
            #[cfg(feature = "legacy-locking")]
            let mut idx = idx_lock.lock().await;
            Self::replay_index_entry_inner(&mut idx, entry).await?;
        }
        Ok(())
    }

    /// Graceful shutdown — drain HotBuffer, flush WAL, persist index.
    ///
    /// Operations are wrapped in a configurable timeout. If shutdown
    /// exceeds the timeout, a warning is logged but the process still
    /// terminates without blocking indefinitely.
    pub async fn shutdown(&self) -> Result<()> {
        let timeout = std::time::Duration::from_secs(self.config.shutdown.timeout_secs);

        tokio::time::timeout(timeout, async {
            // 1. Persist index so the next session can reload it
            //    without a full WAL recovery.
            if let Some(ref idx_lock) = self.index {
                #[cfg(not(feature = "legacy-locking"))]
                let idx = idx_lock.write().await;
                #[cfg(feature = "legacy-locking")]
                let idx = idx_lock.lock().await;
                let index_path = &self.config.index.path;
                if let Some(parent) = index_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut file = std::fs::File::create(index_path).map_err(Error::IoError)?;
                idx.save(&mut file)?;
                info!("Index saved to {}", index_path.display());
            }

            // 2. Drain HotBuffer (progressive Merkle tree tier).
            if let Some(ref buffer_lock) = self.hot_buffer {
                let mut buffer = buffer_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                if !buffer.is_empty() {
                    let snapshot = buffer.drain();
                    if let Some(ref tree_lock) = self.merkle_tree {
                        let mut tree = tree_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                        tree.insert_batch(&snapshot);
                        // Persist if path is configured.
                        if let Some(path) = &self.config.merkle.merkle_tree_path {
                            if let Err(e) = tree.save(path) {
                                warn!("Failed to persist Merkle tree during shutdown: {}", e);
                            }
                        }
                    }
                    info!("HotBuffer drained during shutdown: {} vectors", snapshot.len());
                }
            }

            // 3. Flush WAL.
            self.wal.flush().await?;

            info!("StorageEngine shut down");
            Ok(())
        })
        .await
        .map_err(|_| Error::ServiceError("shutdown timed out".into()))?
    }

    /// Apply topological bias to a query vector using centroid,
    /// correlation, and bucketized centroid data.
    ///
    /// Returns the biased vector when topological tracking is enabled;
    /// returns the query unchanged when it is disabled.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::LockPoisoned(...))` if any internal mutex is poisoned.
    pub fn apply_topological_bias(
        &self,
        query: &[f64],
        filters: &[topological::BiasFilter],
        alpha: f64,
    ) -> Result<Vec<f64>> {
        match (&self.centroids, &self.correlations, &self.buckets) {
            (Some(centroids_lock), Some(correlations_lock), Some(buckets_lock)) => {
                let centroids = centroids_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                let correlations = correlations_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                let buckets = buckets_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
                Ok(topological::apply_topological_bias(
                    query, filters, &centroids, &correlations, &buckets, alpha,
                ))
            }
            _ => Ok(query.to_vec()),
        }
    }

    /// Check if the storage engine is ready for requests.
    ///
    /// Returns a map of component → status for diagnostics.
    /// Used by the `/health/readiness` endpoint.
    pub fn is_ready(&self) -> std::collections::HashMap<String, bool> {
        let mut diag = std::collections::HashMap::new();
        // WAL is always present (loaded during open).
        diag.insert("wal".to_string(), true);
        // Index may not be enabled.
        diag.insert("index".to_string(), self.index.is_some());
        // HotBuffer is present when Merkle is enabled.
        diag.insert("hot_buffer".to_string(), self.hot_buffer.is_some());
        diag
    }

    // ─── helpers ──────────────────────────────────────────────────────

    /// Deserialize and apply a single WAL entry to the hot store.
    fn apply_wal_entry(hot: &HotStore, entry: &WalEntry) -> Result<()> {
        if entry.op_code != OpCode::InsertVector as u8 {
            // Only InsertVector is handled in Phase 1.
            return Ok(());
        }

        let (id, vector, metadata) = serde_json::from_slice::<(VectorId, Vec<f64>, serde_json::Value)>(&entry.payload)
            .map_err(|e| Error::JsonError(e.to_string()))?;

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
    async fn replay_index_entry_inner(idx: &mut AnyIndex, entry: &WalEntry) -> Result<()> {
        if entry.op_code != OpCode::InsertVector as u8 {
            return Ok(());
        }

        let (id, vector, _metadata): (VectorId, Vec<f64>, serde_json::Value) =
            serde_json::from_slice(&entry.payload).map_err(|e| Error::JsonError(e.to_string()))?;

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
            topological: TopologicalConfig::default(),
            merkle: MerkleConfig::default(),
            shutdown: ShutdownConfig::default(),
        }
    }

    /// Build a test config with topological bias enabled.
    fn test_config_with_topological(tmp: &TempDir) -> StorageConfig {
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
                enabled: true,
                dim: 4,
                hnsw: tesseract_index::types::HnswConfig::default(),
                path: root.join("index.bin"),
            },
            lifecycle: LifecycleConfig::default(),
            topological: TopologicalConfig {
                enabled: true,
                categorical_fields: vec!["category".to_string()],
                numerical_fields: vec!["year".to_string()],
                numerical_buckets: [("year".to_string(), vec![2015.0, 2018.0, 2021.0, 2024.0])]
                    .into_iter()
                    .collect(),
            },
            merkle: MerkleConfig::default(),
            shutdown: ShutdownConfig::default(),
        }
    }

    /// Build a test config with Merkle tree enabled.
    fn test_config_with_merkle(tmp: &TempDir) -> StorageConfig {
        let root = tmp.path().to_path_buf();
        StorageConfig {
            wal: WalConfig {
                wal_dir: root.join("wal"),
                segment_size: 1024 * 1024,
                fsync_interval_ms: 100,
                fsync_interval_ops: 1000,
            },
            hot: HotStoreConfig { max_records: 200 },
            cold: ColdStoreConfig { data_dir: root.join("cold"), zstd_level: 0, max_rows_per_file: 100 },
            skeleton: SkeletonConfig { wake_threshold: 0.15 },
            cache: PageCacheConfig { capacity: 100 },
            index: IndexConfig {
                enabled: true,
                dim: 4,
                hnsw: tesseract_index::types::HnswConfig::default(),
                path: root.join("index.bin"),
            },
            lifecycle: LifecycleConfig::default(),
            topological: TopologicalConfig::default(),
            merkle: MerkleConfig {
                enabled: true,
                hot_buffer_capacity: 50,
                max_cluster_size: 100,
                merkle_tree_path: Some(root.join("merkle.bin")),
            },
            shutdown: ShutdownConfig::default(),
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

    // -----------------------------------------------------------------------
    // Topological bias integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn topological_bias_disabled_by_default() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();
        let query = vec![1.0, 2.0, 3.0, 4.0];

        // With no topological config, bias should return query unchanged
        let biased = engine.apply_topological_bias(&query, &[], 0.3).unwrap();
        assert_eq!(biased, query);
    }

    #[tokio::test]
    async fn topological_bias_updates_on_insert() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_topological(&tmp)).await.unwrap();

        // Insert vectors with categorical metadata
        engine
            .insert(VectorId(1), vec![10.0, 0.0, 0.0, 0.0], serde_json::json!({"category": "science", "year": 2020}), WriteMode::Fast)
            .await
            .unwrap();

        engine
            .insert(VectorId(2), vec![0.0, 10.0, 0.0, 0.0], serde_json::json!({"category": "art", "year": 1990}), WriteMode::Fast)
            .await
            .unwrap();

        // Apply categorical bias toward "science"
        use tesseract_core::topological::{BiasFilter, BiasKind};
        let filters = vec![BiasFilter {
            field: "category".to_string(),
            kind: BiasKind::Category("science".to_string()),
        }];
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let biased = engine.apply_topological_bias(&query, &filters, 0.5).unwrap();

        // science centroid = (10, 0, 0, 0), global centroid = (5, 5, 0, 0)
        // delta = (5, -5, 0, 0), bias = alpha * delta = (2.5, -2.5, 0, 0)
        assert!((biased[0] - 2.5).abs() < 1e-10, "dim0 should be 2.5, got {}", biased[0]);
        assert!((biased[1] - (-2.5)).abs() < 1e-10, "dim1 should be -2.5, got {}", biased[1]);
    }

    #[tokio::test]
    async fn topological_bias_with_enabled_index_and_search() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_topological(&tmp)).await.unwrap();

        // Insert 10 vectors: 5 in "science" cluster, 5 in "art" cluster
        for i in 0..5u64 {
            engine
                .insert(
                    VectorId(i),
                    vec![10.0 + i as f64 * 0.1, 0.0, 0.0, 0.0],
                    serde_json::json!({"category": "science", "year": 2020 + i}),
                    WriteMode::Fast,
                )
                .await
                .unwrap();
        }
        for i in 5..10u64 {
            engine
                .insert(
                    VectorId(i),
                    vec![0.0, 10.0 + (i - 5) as f64 * 0.1, 0.0, 0.0],
                    serde_json::json!({"category": "art", "year": 1990 + (i - 5)}),
                    WriteMode::Fast,
                )
                .await
                .unwrap();
        }

        // Search without bias — query at origin will find some of both
        let query = vec![0.0, 0.0, 0.0, 0.0];
        let unbiased = engine.search(&query, 10, None).await.unwrap();

        // Search with science bias
        use tesseract_core::topological::{BiasFilter, BiasKind};
        let filters = vec![BiasFilter {
            field: "category".to_string(),
            kind: BiasKind::Category("science".to_string()),
        }];
        let biased_vec = engine.apply_topological_bias(&query, &filters, 0.5).unwrap();
        let biased = engine.search(&biased_vec, 10, None).await.unwrap();

        // Both should return results (index is populated)
        assert!(!unbiased.is_empty(), "unbiased search should return results");
        assert!(!biased.is_empty(), "biased search should return results");

        // The biased search should have science vectors (IDs 0-4) higher ranked
        // than the unbiased search
        let biased_first_few: Vec<u64> = biased.iter().take(5).map(|(id, _)| id.0).collect();
        let science_count = biased_first_few.iter().filter(|id| **id < 5).count();
        assert!(
            science_count >= 3,
            "expected at least 3 science results in top 5 biased, got {science_count}: {biased_first_few:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Merkle tree integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn merkle_insert_with_hot_buffer_stores_in_buffer() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_merkle(&tmp)).await.unwrap();

        // Insert vectors — they should be stored in the hot buffer.
        for i in 0..10u64 {
            engine
                .insert(VectorId(i), vec![i as f64; 4], serde_json::json!({"idx": i}), WriteMode::Fast)
                .await
                .unwrap();
        }

        // The hot buffer should have vectors.
        let buffer = engine.hot_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buffer.len(), 10);
    }

    #[tokio::test]
    async fn merkle_insert_small_batch_does_not_overflow() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_merkle(&tmp)).await.unwrap();

        // Insert fewer vectors than the hot buffer capacity (50).
        for i in 0..20u64 {
            engine
                .insert(VectorId(i), vec![i as f32 as f64; 4], serde_json::json!({"idx": i}), WriteMode::Fast)
                .await
                .unwrap();
        }

        // Buffer should have 20 (not full, no merge triggered).
        let buffer = engine.hot_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buffer.len(), 20);
        assert!(!buffer.is_full());
    }

    #[tokio::test]
    async fn merkle_insert_triggers_merge_when_full() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_merkle(&tmp)).await.unwrap();

        // Insert more than hot_buffer_capacity (50) vectors.
        for i in 0..60u64 {
            engine
                .insert(VectorId(i), vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({"idx": i}), WriteMode::Fast)
                .await
                .unwrap();
        }

        // After many inserts, the buffer should have been drained at least once,
        // and the Merkle tree should have centroids.
        let tree = engine.merkle_tree.as_ref().unwrap().lock().unwrap();
        assert!(tree.num_centroids() > 0, "Merkle tree should have centroids after full buffer");

        // The hot buffer should have at most 50 vectors (capacity).
        let buffer = engine.hot_buffer.as_ref().unwrap().lock().unwrap();
        assert!(buffer.len() <= 50, "buffer should not exceed capacity, got {}", buffer.len());
    }

    #[tokio::test]
    async fn merkle_hybrid_search_returns_results() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config_with_merkle(&tmp)).await.unwrap();

        // Insert vectors that will go into hot buffer and trigger merge.
        for i in 0..60u64 {
            engine
                .insert(VectorId(i), vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({"idx": i}), WriteMode::Fast)
                .await
                .unwrap();
        }

        // Search should return results from both index and hot buffer.
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = engine.search(&query, 10, None).await.unwrap();
        assert!(!results.is_empty(), "hybrid search should return results");
        assert!(results.len() <= 10, "should respect k=10, got {}", results.len());
    }

    #[tokio::test]
    async fn merkle_disabled_has_no_buffer_or_tree() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();

        assert!(engine.hot_buffer.is_none(), "hot buffer should be None when merkle disabled");
        assert!(engine.merkle_tree.is_none(), "merkle tree should be None when merkle disabled");
    }

    // -----------------------------------------------------------------------
    // Existing tests (keep at the end)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn topological_bias_disabled_does_not_track() {
        let tmp = TempDir::new().unwrap();
        let engine = StorageEngine::open(test_config(&tmp)).await.unwrap();

        // Even with metadata, disabled topological should not affect results
        engine
            .insert(
                VectorId(1),
                vec![10.0, 0.0, 0.0, 0.0],
                serde_json::json!({"category": "science"}),
                WriteMode::Fast,
            )
            .await
            .unwrap();

        // apply_topological_bias should return query unchanged
        use tesseract_core::topological::{BiasFilter, BiasKind};
        let filters = vec![BiasFilter {
            field: "category".to_string(),
            kind: BiasKind::Category("science".to_string()),
        }];
        let query = vec![1.0, 2.0, 3.0, 4.0];
        let biased = engine.apply_topological_bias(&query, &filters, 0.5).unwrap();
        assert_eq!(biased, query, "disabled topological should not bias");
    }
}
