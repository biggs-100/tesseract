# Tasks: Phase 1 — Storage Engine

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Est. changed lines | 2000–3000 |
| 800-line budget risk | Medium |
| Chained PRs | Yes — 4 stacked to main |
| Delivery strategy | auto-chain |

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
800-line budget risk: Medium

| Unit | PR | Focused test | Rollback |
|------|----|-------------|----------|
| WAL engine | PR 1 | `cargo test wal::` | wal.rs + error.rs |
| Hot + cache | PR 2 | `cargo test hot_store:: page_cache::` | hot_store.rs + page_cache.rs |
| Cold + skeleton | PR 3 | `cargo test cold_store:: skeleton::` | cold_store.rs + skeleton.rs |
| Facade + lifecycle | PR 4 | `cargo test --test integration` | lifecycle.rs + lib.rs |

## Phase 1: WAL Engine [PR 1]

- [x] 1.1 Cargo.toml: add tokio (fs), crc32fast, bincode
- [x] 1.2 types.rs: SegmentId, TransactionId, Checkpoint, WriteMode, OpCode, WalConfig
- [x] 1.3 error.rs: add IoError, CorruptWal, CrcMismatch, PayloadTruncated, BincodeError
- [x] 1.4 wal.rs: WriteAheadLog — append, flush (100ms ∨ 1000 ops), recover, compact, rotate, CRC32 per entry
- [x] 1.5 WAL tests: entry roundtrip, corruption stops replay, segment rotation, checkpoint recovery, torn write, concurrent serialized writes, compaction dedup

## Phase 2: Hot Tier + Page Cache [PR 2]

- [x] 2.1 hot_store.rs: DashMap primary store, oldest-first eviction (scc::TreeIndex deferred to Phase 2 per design)
- [x] 2.2 page_cache.rs: Mutex<LruCache>, configurable capacity (number of pages)
- [x] 2.3 HotStore tests: 8-task concurrent ops, point lookup, duplicate insert, drain ordering, len after insert+delete
- [x] 2.4 PageCache tests: LRU eviction order, promoted entry survives, concurrent reads, zero-capacity panic, single-page eviction

## Phase 3: Cold Tier + Skeleton [PR 3]

- [x] 3.1 Cargo.toml: add zstd (file-based cold store; parquet deferred to optimization phase)
- [x] 3.2 cold_store.rs: File-based batch I/O with ZSTD compression, partition-aware layout, manifest + metadata persistence
- [x] 3.3 skeleton.rs: HashMap<Pid, Vec<f32>> centroids, Euclidean distance, wake threshold (default 0.5 for tests, 0.15 prod)
- [x] 3.4 ColdStore tests: write+read roundtrip, multiple batches, non-existent partition, metadata accuracy, reopen persistence
- [x] 3.5 Skeleton tests: centroid mean computation, distance scoring, incremental update, remove, error cases

## Phase 4: Engine Facade + Lifecycle [PR 4]

- [x] 4.1 lifecycle.rs: tokio background task, access counter decay, promotion/demotion
- [x] 4.2 lib.rs: StorageEngine — open, insert, get, search, wire WAL→hot→cold, WriteMode
- [x] 4.3 Integration tests: full lifecycle, 16-thread stress (10K ops), crash recovery (truncate + replay)

## Phase 5: Verification

- [x] 5.1 `cargo build --workspace`
- [x] 5.2 `cargo clippy --all-targets -- -D warnings`
- [x] 5.3 `cargo test --workspace`
- [x] 5.4 `cargo fmt --all`
