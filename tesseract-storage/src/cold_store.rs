// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tesseract_common::error::Result;

use crate::hot_store::VectorRecord;

/// A partition ID maps to a set of batch files on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PartitionId(pub u64);

/// Configuration for the cold store.
#[derive(Debug, Clone)]
pub struct ColdStoreConfig {
    /// Root directory for all cold store data.
    pub data_dir: PathBuf,
    /// ZSTD compression level (default: 3).
    pub zstd_level: i32,
    /// Maximum records per batch file (default: 10_000).
    pub max_rows_per_file: usize,
}

impl Default for ColdStoreConfig {
    fn default() -> Self {
        Self { data_dir: PathBuf::from("cold_store"), zstd_level: 3, max_rows_per_file: 10_000 }
    }
}

/// Persistent metadata about a single partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionMeta {
    /// Total number of records in this partition.
    pub record_count: usize,
    /// Number of batch files.
    pub batch_count: usize,
    /// Total on-disk size in bytes (compressed).
    pub size_bytes: u64,
}

impl PartitionMeta {
    fn new() -> Self {
        Self { record_count: 0, batch_count: 0, size_bytes: 0 }
    }
}

/// Global manifest — list of all known partition IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    partitions: Vec<u64>,
}

/// File-based cold store with ZSTD-compressed bincode batch files.
///
/// Writes batches of `VectorRecord` as compressed files organized by
/// partition. Each partition is a subdirectory under `data_dir`:
///
/// ```text
/// data_dir/
/// ├── manifest.json
/// ├── partition_{id}/
/// │   ├── meta.json
/// │   ├── batch_000001.zstd
/// │   ├── batch_000002.zstd
/// │   └── ...
/// └── ...
/// ```
///
/// This is a placeholder for a future Parquet-backed implementation.
/// The file-based approach avoids the complex Parquet/Arrow build
/// dependencies while preserving the same I/O boundary and partitioning
/// semantics.
pub struct ColdStore {
    config: ColdStoreConfig,
    /// In-memory partition metadata; kept in sync with per-partition
    /// `meta.json` files on every write.
    writers: Arc<Mutex<HashMap<PartitionId, PartitionMeta>>>,
}

impl ColdStore {
    /// Open or create a cold store at `config.data_dir`.
    ///
    /// If a manifest already exists, all known partitions and their
    /// metadata are loaded into memory.
    pub async fn open(config: ColdStoreConfig) -> Result<Self> {
        tokio::fs::create_dir_all(&config.data_dir).await?;
        let writers = Self::load_manifest(&config).await?;
        Ok(Self { config, writers: Arc::new(Mutex::new(writers)) })
    }

    /// Write a batch of records to a partition.
    ///
    /// Creates a new batch file (`batch_{n:06}.zstd`) inside the
    /// partition directory. Updates the partition metadata and global
    /// manifest after writing.
    ///
    /// # Locking
    ///
    /// The internal `Mutex` is never held across an `.await` point.
    /// Lock acquisitions are brief (HashMap lookups / updates only).
    /// Under concurrent writes to the same partition, batch numbers
    /// are best-effort — the tier lifecycle serialises flushes in
    /// practice.
    pub async fn write_batch(&self, partition: &PartitionId, records: &[VectorRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        // 1. Serialise to JSON (bincode cannot handle serde_json::Value).
        let encoded =
            serde_json::to_string(records).map_err(|e| tesseract_common::error::Error::BincodeError(e.to_string()))?;
        let compressed = zstd::encode_all(std::io::Cursor::new(encoded.as_bytes()), self.config.zstd_level)?;

        let part_dir = self.config.data_dir.join(format!("partition_{}", partition.0));
        tokio::fs::create_dir_all(&part_dir).await?;

        // 2. Peek batch number under a brief lock.
        let batch_num = {
            let writers = self.writers.lock().expect("cold store mutex poisoned");
            writers.get(partition).map(|m| m.batch_count + 1).unwrap_or(1)
        };

        // 3. Write the batch file (no lock held).
        let batch_path = part_dir.join(format!("batch_{batch_num:06}.zstd"));
        tokio::fs::write(&batch_path, &compressed).await?;

        // 4. Update metadata under lock and serialise JSON (no await while locked).
        let (meta_json, manifest_json) = {
            let mut writers = self.writers.lock().expect("cold store mutex poisoned");
            let meta = writers.entry(partition.clone()).or_insert_with(PartitionMeta::new);
            meta.record_count += records.len();
            meta.batch_count = batch_num;
            meta.size_bytes += compressed.len() as u64;

            let meta_json = serde_json::to_string(&*meta)
                .map_err(|e| tesseract_common::error::Error::BincodeError(e.to_string()))?;
            let manifest = Manifest { partitions: writers.keys().map(|p| p.0).collect() };
            let manifest_json = serde_json::to_string(&manifest)
                .map_err(|e| tesseract_common::error::Error::BincodeError(e.to_string()))?;
            (meta_json, manifest_json)
        }; // MutexGuard dropped here — before the awaits below

        // 5. Write JSON files (no lock held).
        tokio::fs::write(part_dir.join("meta.json"), &meta_json).await?;
        tokio::fs::write(self.config.data_dir.join("manifest.json"), &manifest_json).await?;

        Ok(())
    }

    /// Read all records from a partition.
    ///
    /// Iterates over all batch files in sequential order (001, 002, ...)
    /// and concatenates the deserialised records.
    pub async fn read_partition(&self, partition: &PartitionId) -> Result<Vec<VectorRecord>> {
        let part_dir = self.config.data_dir.join(format!("partition_{}", partition.0));

        if !tokio::fs::try_exists(&part_dir).await.unwrap_or(false) {
            return Ok(Vec::new());
        }

        let mut all_records = Vec::new();
        let mut batch_num = 1u32;

        loop {
            let batch_path = part_dir.join(format!("batch_{batch_num:06}.zstd"));
            if !tokio::fs::try_exists(&batch_path).await.unwrap_or(false) {
                break;
            }

            let compressed = tokio::fs::read(&batch_path).await?;
            let decompressed = zstd::decode_all(std::io::Cursor::new(&compressed))?;
            let batch: Vec<VectorRecord> = serde_json::from_slice(&decompressed)
                .map_err(|e| tesseract_common::error::Error::BincodeError(e.to_string()))?;
            all_records.extend(batch);
            batch_num += 1;
        }

        Ok(all_records)
    }

    /// Return metadata for a partition, or `None` if unknown.
    pub fn partition_metadata(&self, partition: &PartitionId) -> Option<PartitionMeta> {
        let writers = self.writers.lock().expect("cold store mutex poisoned");
        writers.get(partition).cloned()
    }

    /// List all partition IDs tracked by this store.
    pub fn partitions(&self) -> Vec<PartitionId> {
        let writers = self.writers.lock().expect("cold store mutex poisoned");
        writers.keys().cloned().collect()
    }

    // ─── helpers ──────────────────────────────────────────────────────

    /// Load the global manifest and all partition metadata from disk.
    async fn load_manifest(config: &ColdStoreConfig) -> Result<HashMap<PartitionId, PartitionMeta>> {
        let manifest_path = config.data_dir.join("manifest.json");
        if !tokio::fs::try_exists(&manifest_path).await.unwrap_or(false) {
            return Ok(HashMap::new());
        }

        let content = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest: Manifest =
            serde_json::from_str(&content).map_err(|e| tesseract_common::error::Error::BincodeError(e.to_string()))?;

        let mut map = HashMap::new();
        for pid in manifest.partitions {
            let meta_path = config.data_dir.join(format!("partition_{pid}")).join("meta.json");
            if tokio::fs::try_exists(&meta_path).await.unwrap_or(false) {
                if let Ok(content) = tokio::fs::read_to_string(&meta_path).await {
                    if let Ok(meta) = serde_json::from_str::<PartitionMeta>(&content) {
                        map.insert(PartitionId(pid), meta);
                    }
                }
            }
        }
        Ok(map)
    }
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tesseract_core::types::VectorId;

    fn make_record(id: u64) -> VectorRecord {
        VectorRecord {
            id: VectorId(id),
            vector: vec![id as f64; 4],
            metadata: serde_json::json!({"label": format!("vec_{}", id)}),
            created_at: id,
            access_count: 0,
        }
    }

    #[tokio::test]
    async fn write_and_read_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        let partition = PartitionId(1);
        let records: Vec<VectorRecord> = (0..10).map(make_record).collect();

        store.write_batch(&partition, &records).await.unwrap();

        let result = store.read_partition(&partition).await.unwrap();
        assert_eq!(result.len(), 10);
        for (i, r) in result.iter().enumerate() {
            assert_eq!(r.id, VectorId(i as u64));
        }
    }

    #[tokio::test]
    async fn multiple_batches_same_partition() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        let partition = PartitionId(42);
        let batch_a: Vec<VectorRecord> = (0..5).map(make_record).collect();
        let batch_b: Vec<VectorRecord> = (5..10).map(make_record).collect();

        store.write_batch(&partition, &batch_a).await.unwrap();
        store.write_batch(&partition, &batch_b).await.unwrap();

        let result = store.read_partition(&partition).await.unwrap();
        assert_eq!(result.len(), 10);
    }

    #[tokio::test]
    async fn non_existent_partition_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        let result = store.read_partition(&PartitionId(999)).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn partition_metadata_correct_after_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        let partition = PartitionId(1);
        let records: Vec<VectorRecord> = (0..7).map(make_record).collect();
        store.write_batch(&partition, &records).await.unwrap();

        let meta = store.partition_metadata(&partition);
        assert!(meta.is_some());
        let meta = meta.unwrap();
        assert_eq!(meta.record_count, 7);
        assert_eq!(meta.batch_count, 1);
        assert!(meta.size_bytes > 0);
    }

    #[tokio::test]
    async fn non_existent_partition_metadata_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        assert!(store.partition_metadata(&PartitionId(999)).is_none());
    }

    #[tokio::test]
    async fn partitions_list_after_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        store.write_batch(&PartitionId(1), &(0..3).map(make_record).collect::<Vec<_>>()).await.unwrap();
        store.write_batch(&PartitionId(2), &(0..3).map(make_record).collect::<Vec<_>>()).await.unwrap();

        let partitions = store.partitions();
        assert_eq!(partitions.len(), 2);
        assert!(partitions.contains(&PartitionId(1)));
        assert!(partitions.contains(&PartitionId(2)));
    }

    #[tokio::test]
    async fn reopen_persists_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("cold");

        // First session.
        {
            let store =
                ColdStore::open(ColdStoreConfig { data_dir: data_dir.clone(), ..Default::default() }).await.unwrap();

            store.write_batch(&PartitionId(1), &(0..5).map(make_record).collect::<Vec<_>>()).await.unwrap();
        }

        // Re-open and verify data survives.
        {
            let store = ColdStore::open(ColdStoreConfig { data_dir, ..Default::default() }).await.unwrap();

            let parts = store.partitions();
            assert_eq!(parts.len(), 1);
            assert!(parts.contains(&PartitionId(1)));

            let meta = store.partition_metadata(&PartitionId(1));
            assert!(meta.is_some());
            assert_eq!(meta.unwrap().record_count, 5);

            let records = store.read_partition(&PartitionId(1)).await.unwrap();
            assert_eq!(records.len(), 5);
        }
    }

    #[tokio::test]
    async fn empty_write_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ColdStore::open(ColdStoreConfig { data_dir: dir.path().join("cold"), ..Default::default() }).await.unwrap();

        let partition = PartitionId(1);
        store.write_batch(&partition, &[]).await.unwrap();

        // No batch files written, metadata unchanged.
        assert!(store.partition_metadata(&partition).is_none());
    }
}
