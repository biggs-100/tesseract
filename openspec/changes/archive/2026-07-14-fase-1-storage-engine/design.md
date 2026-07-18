# Design: Phase 1 — Storage Engine

## Technical Approach

Six subsystems layered in `tesseract-storage`: WAL (custom binary + CRC32) → hot tier (dashmap + scc::TreeIndex) → cold tier (Parquet + ZSTD), with vector skeleton (f32 centroids), page cache (LRU), and tier lifecycle (tokio background task). Every mutation flows WAL → hot tier before acknowledgement. Cold tier receives batched flushes via `spawn_blocking`. Dual-mode consistency: Durable (ack after `fsync`) vs Fast (ack after buffer write).

## Architecture Decisions

| Decision | Options | Tradeoff | Choice |
|----------|---------|----------|--------|
| WAL segment size | 64 MB / 256 MB / configurable | 256 MB delays compaction & recovery; configurable adds param surface | **64 MB fixed** — spec says 64 MB, adequate for recovery latency |
| Fsync batching | 100ms timer / 1000 ops / hybrid | Timer alone misses idle-burst patterns | **Hybrid: 100ms ∨ 1000 ops** — whichever fires first |
| Hot tier eviction | Oldest-first / LRU / configurable | LRU better recall but oldest-first aligns with WAL insert order | **Oldest-first** — natural fit for sequential WAL replay |
| Compression (ZSTD) | Level 3 / Level 6 / auto | L3 faster; L6 better ratio but 2× CPU on float data | **ZSTD level 3** — parquet default, good float compression |
| Skeleton threshold | Fixed / percentile / configurable | Fixed simplest; percentile adaptive but adds per-query cost | **Configurable f64** — start at `0.15`, tune per-deployment |

## Data Flow

```
Client ──→ insert(id, vec, meta, mode)
              │
          ┌───▼───┐
          │  WAL   │── append + fsync (durable) ──→ Segment file
          └───┬───┘         │
              │             ▼
              │        ack to client
              ▼
          ┌──────────┐
          │ HotStore  │── dashmap + scc::TreeIndex
          │ (f32 RAM) │
          └────┬─────┘
               │ 70% watermark flush
               ▼
          ┌──────────────┐
          │ TierLifecycle │── tokio::spawn, 60s interval
          └────┬─────┬────┘
               │     │
          ┌────▼─┐ ┌─▼──────────┐
          │Cold  │ │VectorSkel. │
          │Store │ │HashMap<Pid,│
          │Parq.+│ │Vec<f32>>   │
          │ ZSTD │ └────────────┘
          └──┬───┘
             │
         ┌───▼────┐
         │PageCache│── Mutex<LruCache<PageKey, Page>>
         └────────┘
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `tesseract-storage/Cargo.toml` | Modify | Add tokio, dashmap, scc, parquet, arrow, zstd, lru, crc32fast, parking_lot |
| `tesseract-storage/src/lib.rs` | Modify | Module declarations + `StorageEngine` facade |
| `tesseract-storage/src/types.rs` | Create | `StorageConfig`, `WalConfig`, `WriteMode`, `SegmentId`, `OpCode` |
| `tesseract-storage/src/wal.rs` | Create | `WriteAheadLog` — append, flush, recover, compact, rotate |
| `tesseract-storage/src/hot_store.rs` | Create | `HotStore` — dashmap + scc::TreeIndex for range scans |
| `tesseract-storage/src/cold_store.rs` | Create | `ColdStore` — Parquet batch writer/reader via spawn_blocking |
| `tesseract-storage/src/skeleton.rs` | Create | `VectorSkeleton` — `HashMap<PartitionId, Vec<f32>>` centroid cache |
| `tesseract-storage/src/page_cache.rs` | Create | `PageCache` — `Mutex<LruCache<PageKey, Page>>` |
| `tesseract-storage/src/lifecycle.rs` | Create | `TierLifecycle` — background loop with access counters |
| `tesseract-common/src/error.rs` | Modify | Add `IoError`, `CorruptWal`, `CrcMismatch`, `BatchRequired` |

## Interfaces / Contracts

```rust
pub enum WriteMode { Durable, Fast }
pub enum OpCode { InsertVector = 0x01, DeleteVector = 0x02, UpdateMetadata = 0x03 }

// Public facade — accepts f64 vectors, stores as f32 in hot/cold tiers
pub struct StorageEngine { wal: Arc<WriteAheadLog>, hot: Arc<HotStore>, cold: Arc<ColdStore>, skeleton: Arc<VectorSkeleton>, cache: Arc<Mutex<PageCache>> }
impl StorageEngine {
    pub async fn open(config: StorageConfig) -> Result<Self>;
    pub async fn insert(&self, id: VectorId, vec: Vec<f64>, meta: MetadataValue, mode: WriteMode) -> Result<()>;
    pub async fn get(&self, id: &VectorId) -> Result<Option<VectorRecord>>; // returns f64
    pub async fn search(&self, query: &[f64], filter: &MetadataPredicate, limit: usize) -> Result<Vec<ScoredRecord>>;
}
```

**WAL entry on disk**: `[txn_id:u64, op_code:u8, payload_len:u32, payload:[u8;N], crc32:u32]`. Payload = bincode(`VectorRecord`). Segments: `wal-{id:010}.log`. Checkpoint: `checkpoint.bin` (last_flushed_txn_id). Compaction: seal active segment, merge sealed segments by latest-txn-id wins, atomic rename `.tmp` → `.log`.

**Recovery**: Read checkpoint → scan segments in order → validate CRC per entry → skip flushed (txn_id ≤ checkpoint) → stop at first CRC mismatch → replay remainder into hot tier.

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Unit | WAL append + read + CRC | Temp file, roundtrip, corrupt byte → CRC fail |
| Unit | HotStore concurrent ops | 8 tokio tasks insert+get, verify consistency |
| Unit | ColdStore batch write+read | Temp dir, 100 vectors, Parquet roundtrip |
| Unit | Skeleton distance | Known centroids, verify nearest-match |
| Integration | Full engine lifecycle | Insert → WAL → flush → cold → read back |
| Stress | 16 concurrent WAL writers | 10K ops, verify ordering + no data loss |
| Recovery | Crash simulation | Write 500 entries, truncate last segment, recover |

## Threat Matrix

N/A — no routing, shell commands, subprocesses, VCS/PR automation, executable-file classification, or process-integration boundary. File I/O and concurrent access use production-safe crates with zero `unsafe` in application code.

## Migration / Rollout

No migration required. This is a new crate — no existing data or consumers.

## Open Questions

None — all decisions resolved per exploration analysis (ref: `exploration.md`).
