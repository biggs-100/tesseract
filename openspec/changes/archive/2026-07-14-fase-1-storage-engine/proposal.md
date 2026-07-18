# Proposal: Phase 1 — Storage Engine

## Intent

Build the foundational storage layer for Tesseract — a dual-tier vector database with WAL-backed durability, an in-memory hot tier for fast writes, a Parquet-backed cold tier for compression, and a lifecycle manager to move data between them. This enables VQL to persist and query vectors with configurable consistency.

## Scope

### In Scope
- All 6 storage subsystems: WAL, hot tier, cold tier, vector skeleton, page cache, tier lifecycle
- Dual-mode consistency (Durable + Fast)
- Windows + Linux parity
- `tesseract-storage` crate implementation

### Out of Scope
- Distributed/replicated storage
- Index layer (Phase 2+)
- Cross-language serialization or schema evolution
- Full-text or metadata search

## Capabilities

### New Capabilities
- `wal-engine`: Write-Ahead Log with CRC32 validation, segment rotation, async fsync, crash recovery, and compaction
- `hot-tier`: In-memory vector buffer using dashmap for point lookups + scc::TreeIndex for range scans
- `cold-tier`: Parquet-backed persistent storage with ZSTD compression and row group pruning via min/max statistics
- `vector-skeleton`: Compressed mean centroids in RAM for cold partition awakening on query
- `page-cache`: LRU buffer pool over lru::LruCache for cold tier reads
- `tier-lifecycle`: Background tokio task for promotion/demotion between hot and cold tiers with configurable thresholds

### Modified Capabilities
None

## Approach

All 6 subsystems built into `tesseract-storage` crate. WAL sits before the hot tier — every mutation goes WAL → hot tier. Cold tier receives flushes from hot via `spawn_blocking` Parquet writes. Vector skeleton computed on partition creation. Lifecycle runs as a background tokio task scanning access counters. All I/O via `tokio::fs`; synchronous work dispatched to `spawn_blocking`.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `tesseract-storage/src/` | New | Full crate — 6 subsystem modules |
| `Cargo.toml` | Modified | deps: dashmap, scc, parquet, lru, crc32fast, tokio, zstd |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Parquet write path batch-only + sync | Medium | spawn_blocking + semaphore, batch at 10K entries |
| Windows async I/O semantics differ | Medium | Windows CI, spawn_blocking for fsync |
| WAL recovery correctness under crash | High | CRC32 per entry, fault-injection tests |
| Compaction + concurrent writer race | Medium | Sealed segment ref counting, atomic manifest |
| 6-subsystem size may exceed PR budget | Medium | Split into chained PRs if >400 lines per slice |

## Rollback Plan

Remove `tesseract-storage` crate and its deps from `Cargo.toml`. Revert to Phase 0 stub.

## Dependencies

- New crates: `dashmap`, `scc`, `parquet` (zstd feature), `lru`, `crc32fast`, `tokio` (fs feature)
- Already present: `bincode`, `serde`, `thiserror`, `tracing`

## Success Criteria

- [ ] WAL writes and recovers correctly — insert 1000 entries, simulate crash, replay
- [ ] Hot tier supports concurrent readers + writers via dashmap
- [ ] Cold tier flushes hot data to Parquet and reads back with correct VectorId and metadata
- [ ] Vector skeleton accelerates cold queries — skeleton hit faster than full scan
- [ ] LRU page cache evicts correctly under memory pressure
- [ ] Tier lifecycle promotes/demotes based on access frequency
- [ ] All tests pass on both Linux and Windows
- [ ] `cargo clippy --all-targets -- -D warnings` passes
