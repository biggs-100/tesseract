# Exploration: Phase 1 — Storage Engine

**Change**: `fase-1-storage-engine`
**Project**: VQL (Tesseract)
**Date**: 2026-07-14

---

## Current State (Phase 0 Context)

Phase 0 ("fundación") established the workspace skeleton and core math layer:

| Crate | Status | Key Contents |
|-------|--------|-------------|
| `tesseract-common` | ✅ Done | `Error` enum (`DimensionMismatch`, `IndexOutOfBounds`, `ParseError`), `Result<T>` alias |
| `tesseract-core` | ✅ Done | `VectorId(u64)`, `Timestamp(i64)`, `MetadataValue` (serde enum), `NormalizedVector`, `CosineDistance`, `EuclideanDistance`, `WeightMask`, `Projection` trait |
| `tesseract-storage` | 🔲 Stub | Single comment `// Phase 2+ — storage layer` |
| `tesseract-vql` | ✅ Done | VQL parser (nom-based), AST, grammar |
| `tesseract-index` | 🔲 Stub | Placeholder for future index layer |
| `tesseract-api` | 🔲 Stub | Depends on `tesseract-vql` |

**Existing dependencies** (from Cargo.lock): `bincode 1.3`, `serde 1`, `thiserror 2`, `tracing 0.1`, `nom 7`, `once_cell`, `pin-project-lite`. No async runtime, no concurrent data structures, no compression libraries.

**Design conventions inherited from Phase 0**:
- Rust edition 2024, toolchain stable (MSRV 1.85)
- tokio for async runtime
- parking_lot for hot-path mutexes (not yet in Cargo.lock)
- Cargo.lock committed
- AGPL-3.0 license
- `#[derive(Serialize, Deserialize)]` via serde throughout

---

## Subsystem Analysis

### 1. Write-Ahead Log (WAL)

The WAL is the durability backbone. Every mutation (vector insert, metadata update, index mutation) goes through the WAL before it is acknowledged.

#### Options

##### Option A: Custom Binary Format

**Description**: Fixed-size header + variable payload. Entry format: `(txn_id: u64, op_code: u8, payload_len: u32, payload: [u8], crc32: u32)`. Segments are plain files with a 16-byte segment header (magic, version, segment_id). CRC32 via `crc32fast` or `crc` crate.

**Pros**:
- Full control over layout — no schema overhead for tiny entries
- Minimal dependencies (just a CRC crate + serde for payloads)
- Easy to version, easy to debug with hexdump
- Tiny entries (vector inserts may be < 1KB)
- No external schema registry needed

**Cons**:
- Write your own reader/writer/iterator
- Need to hand-roll segment indexing for compaction
- No standard tooling to inspect WAL files
- Must implement your own async I/O scheduling

**Effort**: Medium (core is ~600 lines, but recovery and compaction add complexity)

##### Option B: Protobuf via prost

**Description**: Define WAL entries as protobuf messages, use `prost` for encode/decode. Entries are length-delimited protobuf (`write_varint(len) + write_msg`).

**Pros**:
- Self-describing (`.proto` file serves as documentation)
- Schema evolution (add fields, deprecate old ones)
- Protobuf tooling ecosystem (protoc validation, lint)

**Cons**:
- Heavy compile-time dependency (protoc compiler or `prost-build`)
- Bloat for tiny entries — protobuf overhead adds several bytes per field tag
- Vector embeddings encoded as `repeated double` or `bytes` — adds unnecessary indirection
- No CRC built-in; still need external checksums
- Larger binary + longer compile times

**Effort**: Medium (protobuf integration adds setup cost, less control over disk format)

##### Option C: Apache Arrow Flight / IPC

**Description**: Use Arrow IPC streaming format as the WAL. Each entry is an Arrow record batch with fixed schema columns (txn_id, op_code, payload, etc.).

**Pros**:
- Zero-copy reads with Arrow format
- Natural interop with Parquet cold tier (same columnar format)
- Schema validation built in
- Batches can be large and efficient

**Cons**:
- Massive dependency tree (arrow, parquet, etc. for the WAL alone)
- Columnar is the WRONG format for a WAL — WALs append tiny entries, Arrow is optimized for batches
- Overkill for what is fundamentally a log of binary blobs
- 10x+ compile time increase for marginal benefit
- `arrow` crate adds ~50+ transitive dependencies

**Effort**: High (dependency complexity, wrong abstraction)

##### Recommendation: Option A (Custom Binary)

The WAL is a simple append-only log. Custom binary gives the best performance/complexity ratio. Vectors entries are already serde-serializable via `bincode`. The entire payload can be `(txn_id, op_code, bincode(payload))` with a trailing CRC32. No protobuf or Arrow overhead.

---

### 2. Hot Tier — Concurrent In-Memory Buffer

The hot tier is the front door for all writes and the first stop for reads. It needs point lookups by `VectorId`, range scans by metadata, and concurrent access from many tokio tasks.

#### Options

##### Option A: dashmap (sharded HashMap)

**Description**: `dashmap::DashMap<VectorId, HotEntry>`. Internally sharded across N locks, good concurrent performance for read-heavy workloads.

**Pros**:
- Battle-tested, 15M+ downloads, active maintenance
- Simple API — drop-in replacement for `RwLock<HashMap>`
- Good read performance (lock-free read path on latest versions)
- Iterators are safe (snapshot-based)
- Supports `entry()` API for atomic upsert

**Cons**:
- No range scan by metadata — need secondary indexes
- Memory overhead per entry (shard + hash table overhead)
- No built-in eviction or size tracking
- `entry()` API has some ergonomic rough edges with closures

**Effort**: Low

##### Option B: evmap (read-copy-update)

**Description**: `evmap` uses RCU semantics — readers see a consistent snapshot without locks. Writers clone the data structure and atomically swap.

**Pros**:
- Readers are completely wait-free (no locks, no atomics on read path)
- Ideal for read-heavy workloads (95%+ reads)
- No reader-writer contention at all

**Cons**:
- ⚠️ **Crate maintenance concern**: Last release Sept 2023, GitHub has open issues about tokio compatibility, no recent activity. Falling behind ecosystem.
- Write amplification: every write clones the entire map (O(n) memory per write)
- NOT suitable for write-heavy workloads — clone-on-write destroys performance at scale
- No range scan support
- Hard to reason about memory usage under write pressure

**Effort**: Low (integrating) but **high risk** (maintenance, write amplification)

##### Option C: scc crate (Scalable Concurrent Containers)

**Description**: `scc::HashMap` — lock-free linked-bucket array with per-bucket RW locks. Also provides `scc::HashIndex` (read-optimized) and `scc::TreeIndex` (B+ tree for range scans).

**Pros**:
- Both sync and async interfaces built-in
- `HashIndex` is read-optimized (perfect for hot tier reads)
- `TreeIndex` is a B+ tree — supports range scans natively
- Active maintenance (v3.4.x, frequent releases)
- SIMD-accelerated lookup (AVX2)
- `HashCache` variant for built-in eviction

**Cons**:
- Smaller ecosystem than dashmap
- Less battle-tested in production (though growing adoption)
- `TreeIndex` is relatively new
- API differs from standard HashMap (learning curve)

**Effort**: Medium (more options to evaluate, but well-documented)

##### Option D: Custom lock-free B-tree / skip list

**Description**: Roll your own concurrent data structure. Crossbeam epoch-based reclamation + custom B-tree or skip list.

**Pros**:
- Full control over memory layout and cache behavior
- Potentially optimal for the specific workload
- No dependency risk
- Great learning opportunity

**Cons**:
- **Months of work** to get right
- Concurrency bugs are the hardest to find and fix
- Skip lists have poor cache locality
- Proven impossible to get right without extensive fuzzing
- No business value at this stage

**Effort**: Very High (4-8 weeks for production quality)

##### Recommendation: Option A (dashmap) with Option C (scc) as close alternative

**Primary**: `dashmap` for `VectorId → HotEntry`. It's proven, simple, and immediately productive.

**Secondary**: `scc::TreeIndex` for metadata-based range scans. A B+ tree over metadata values enables ordered scans that dashmap cannot provide.

Do NOT use `evmap` — the maintenance status is concerning and write amplification will hurt as the hot tier fills.

Hold on custom structures — that's a premature optimization. Measure first.

---

### 3. Cold Tier — Parquet Disk Backend

The cold tier stores infrequently accessed vectors in compressed columnar format on disk.

#### Options

##### Option A: Apache Arrow `parquet` crate (official)

**Description**: The official Apache `parquet` Rust crate from `arrow-rs`. Supports row groups, column statistics, ZSTD/LZ4/Snappy compression, predicate pushdown via statistics.

**Pros**:
- Official Apache project, actively maintained
- Full Parquet format support (row groups, statistics, bloom filters)
- ZSTD compression via feature flag
- Column statistics (min/max/null count) for metadata pruning
- Arrow integration enables zero-copy reads into memory
- Row group selection (read only needed groups)

**Cons**:
- ⚠️ **Write path is batch-only**: Must construct Arrow `RecordBatch` and write through `ArrowWriter`. No row-by-row streaming append.
- File-level writes only (no in-place update — write new file, swap)
- Not designed for frequent small writes — batch write is expensive
- Adds ~40 transitive dependencies (arrow, parquet, flatbuffers, etc.)
- Build time impact: +60-90s on clean build
- Async write needs `spawn_blocking` — the Parquet writer is synchronous

**Effort**: Medium (the integration is straightforward but batch semantics require architectural care)

##### Option B: Write custom columnar format

**Description**: Design a simpler columnar format specific to vector storage (fixed-width embedding columns, variable-width metadata columns, custom min/max indexes).

**Pros**:
- Minimal dependencies
- Optimized for the specific vector workload
- Can support row-by-row appends
- Smaller binary size

**Cons**:
- Months of design + implementation
- No tooling ecosystem (no parquet-tools, no Spark/Pandas interop)
- Column statistics implementation from scratch
- Compression integration from scratch
- Will inevitably re-invent Parquet badly

**Effort**: Very High

##### Recommendation: Option A (Arrow Parquet)

The `parquet` crate is mature enough for Phase 1. The batch-write limitation actually maps well to the cold tier use case — we flush hot tier batches to cold tier in bulk. Key architectural rule: **cold tier writes happen in batch jobs (hot tier flushes, compaction), not on the hot path.**

For row group statistics, the [Parquet format stores column metadata](https://github.com/apache/arrow-rs/blob/main/parquet/src/file/metadata/column_chunk_metadata.rs) including `statistics` with min/max values. This enables query-time pruning at the row group level without reading data.

---

### 4. Vector Skeleton (Compressed Centroid)

The skeleton is an in-RAM compressed representation of each cold cluster. On query, compare the query vector to the skeleton — if close enough, wake the cluster to load from disk.

#### Options

##### Option A: Simple Mean Centroid

**Description**: Store the arithmetic mean of all vectors in the partition as a flat `Vec<f64>`. That's one floating-point vector per partition in RAM.

**Pros**:
- Trivial to compute and update
- O(dim) distance comparison — fast
- Tiny memory footprint (8 bytes per dimension per partition)
- Perfect recall for uniform distributions

**Cons**:
- Poor selectivity for skewed distributions — mean may be far from actual cluster hull
- High false-positive rate (wakes partitions unnecessarily)
- No fidelity to outliers

**Effort**: Low

##### Option B: Scalar Quantization (SQ)

**Description**: Quantize each dimension of the cluster's bounding box to 8-bit or 4-bit. Store min/max per dimension, then quantized representation of the centroid or several representative points.

**Pros**:
- 4x-8x memory reduction vs f64 centroids
- Good balance of accuracy vs memory
- Distance computation with integer arithmetic (SIMD-friendly)

**Cons**:
- More complex update on partition data change
- Must track per-dimension min/max
- Still needs mean centroid as fallback

**Effort**: Medium

##### Option C: Product Quantization (PQ)

**Description**: Split vectors into sub-vectors, quantize each sub-vector with its own codebook. Store PQ codes (typically 8-64 bytes per vector). Use asymmetric distance computation (ADC).

**Pros**:
- Highest compression ratio (16-64 bytes per vector regardless of dimensionality)
- ADC fast for approximate search
- Well-studied approach (IVF-PQ, HNSW-PQ in production at scale)

**Cons**:
- **Training phase required**: Need sample data to build codebooks (k-means clustering per subspace)
- Codebooks must be periodically retrained as data distribution shifts
- PQ introduces approximation error even for the centroid
- Overkill for cold-tier skeleton (which is just a single centroid per partition, not millions of vectors)
- External dependency or significant implementation work

**Effort**: High

##### Recommendation: Option A (Simple Mean Centroid) with Option B (SQ) as future upgrade

Phase 1 skeleton should be a **simple mean centroid** with `Vec<f32>` (half the memory of f64). This is enough for the tier-wake heuristic. PQ is over-engineering at this stage — reserve it for the actual vector compression in the index layer (Phase 2+).

If memory pressure becomes an issue, upgrade to scalar quantization (8-bit per dimension) by storing `i8` quantized deltas from the centroid mean.

---

### 5. Page Cache / Buffer Pool

Cache for cold tier reads. When a cold partition is "woken up," its decompressed data goes through the buffer pool.

#### Options

##### Option A: LRU via `lru` crate

**Description**: `lru::LruCache<PartitionId, CachedPartition>` with fixed capacity. Evicts least recently used entries when full.

**Pros**:
- Simple, proven algorithm
- Good general-purpose behavior
- `lru` crate is lightweight, no deps
- Deterministic eviction order

**Cons**:
- Cache pollution: one-off scans evict frequently used entries
- Not scan-resistant
- O(1) get but with doubly-linked list overhead (pointer chasing)

**Effort**: Low

##### Option B: Clock (Second-Chance) Algorithm

**Description**: Circular buffer with reference bits. On eviction, scan until finding an entry with clear reference bit. Clear bits as you pass them.

**Pros**:
- No pointer manipulation (array-based)
- Cache-friendly iteration
- Scan-resistant (reference bits protect frequently accessed entries)
- Simple to implement

**Cons**:
- Must implement yourself (no off-the-shelf crate for Clock)
- Reference bit maintenance requires careful concurrent handling
- Degrades to FIFO under full-scan workloads

**Effort**: Medium

##### Option C: LFU via `lfu-cache` or custom

**Description**: Evict least frequently used entries. Track access frequency counters.

**Pros**:
- Great for stable working sets
- Frequently accessed cold partitions stay cached

**Cons**:
- Frequency aging problem (old high-frequency entries never evict)
- Complex to implement correctly (frequency decay, Anytail LFU, etc.)
- Higher per-entry memory overhead (counter + timestamp)
- No strong off-the-shelf Rust LFU crate

**Effort**: Medium-High

##### Recommendation: Option A (LRU via `lru` crate) with clock-based eviction as iteration 2

Start with `lru` for Phase 1. For a cold tier buffer pool, cache misses are expensive (disk I/O + decompression) so even basic LRU provides good hit rates. If measurements show scan-based workloads cause thrashing, upgrade to Clock in Phase 2.

---

### 6. Tier Promotion / Demotion

Background tasks that monitor access frequency and move data between tiers.

**Approach**: Background tokio task with a configurable interval (default: 60s). Track access counters per `PartitionId` in the hot and cold tiers. Define promotion/demotion thresholds.

| Threshold | When | Action |
|-----------|------|--------|
| Hot entry NOT accessed in interval | Counter drops below threshold | Demote to cold tier (flush to Parquet) |
| Cold partition accessed multiple times | Counter exceeds threshold | Promote to hot tier (load into dashmap) |
| Hot tier memory exceeds threshold | Total size > configurable max | Evict coldest entries to cold tier |

**Effort**: Medium

---

### 7. Async I/O Strategy

#### Options

##### Option A: `tokio::fs` + `spawn_blocking`

**Description**: Use `tokio::fs` for basic file ops (open, read, write). For blocking operations (Parquet write, decompression), dispatch to `tokio::task::spawn_blocking` which runs on a dedicated thread pool.

**Pros**:
- Works everywhere (Linux, Windows, macOS)
- No special kernel support needed
- Proven pattern used by countless production systems
- Simple model: spawn_blocking for CPU-heavy or sync-I/O operations

**Cons**:
- Thread pool overhead (default 512 threads max)
- Parquet writes block a thread for the entire write duration
- Not zero-copy

**Effort**: Low

##### Option B: `tokio-uring` (Linux io_uring)

**Description**: Experimental tokio integration with Linux `io_uring`. True async file I/O without thread pool.

**Pros**:
- True zero-copy async file I/O
- No thread pool overhead
- Lower latency for mixed workloads

**Cons**:
- ⚠️ **Linux only** — Windows support nonexistent
- `tokio-uring` is experimental, API still evolving
- Immature ecosystem (fewer examples, less community support)
- Adds significant complexity for Phase 1's I/O volume
- Runtime incompatibility with standard tokio

**Effort**: Very High (and platform-locked)

##### Recommendation: Option A (`tokio::fs` + `spawn_blocking`)

Tokio's `spawn_blocking` pattern is production-proven. Phase 1's I/O throughput does not require io_uring. The `tokio-uring` path should be revisited when (1) io_uring is the default tokio runtime or (2) benchmarks show `spawn_blocking` as a bottleneck.

---

### 8. Serialization Format

Used for: WAL payloads, hot tier entries serialized to cold tier, metadata values.

| Crate | Pros | Cons | Size | Speed | Effort |
|-------|------|------|------|-------|--------|
| **bincode** (already in deps) | Zero deps beyond serde, very fast, compact | No schema evolution, platform-endian | ~same as raw | Fastest | **None** |
| **messagepack** (rmp-serde) | Self-describing, cross-language | Larger than bincode, schema overhead | +15-30% | Fast | Low |
| **protobuf** (prost) | Schema evolution, cross-language | Build script, heavy deps, nesting overhead | +10-20% | Moderate | Medium |
| **flatbuffers** | Zero-copy access, no decode step | Schema compilation, no Rust-native enums, complex build | +0% (access in place) | Fastest read | High |

##### Recommendation: **bincode** (already in tree)

It's already in the dependency tree, it's the fastest, and the WAL/cold tier don't need cross-language interop. Phase 1 uses bincode everywhere. If schema evolution becomes necessary in later phases, add a version byte prefix to enable format migration.

---

### 9. Compression for Embeddings

The cold tier stores embeddings compressed. Parquet's built-in compression applies at the page level.

| Codec | Ratio (f64 vec) | Speed (GB/s) | Parquet support | Notes |
|-------|-----------------|-------------|-----------------|-------|
| **ZSTD** (level 3) | 3-5x | ~500 MB/s enc, ~1 GB/s dec | ✅ Native (zstd feature) | Best compression for embeddings |
| LZ4 | 2-3x | ~2 GB/s enc, ~4 GB/s dec | ✅ Native (lz4 feature) | Fast but weaker on float data |
| Snappy | 2-2.5x | ~1.5 GB/s both | ✅ Native (snap feature) | Good for structured data, meh for floats |
| None | 1x | N/A | ✅ | Only for high-performance hot tier |

##### Recommendation: **ZSTD** for cold tier

Embeddings are floats — ZSTD's entropy coding exploits the structure of float representations better than LZ4 or Snappy. The Parquet `parquet` crate supports ZSTD natively via the `zstd` feature flag. For the hot tier, no compression (already in RAM).

---

## Approach Comparison Summary

| Subsystem | Recommended | Alternative | Key Risk |
|-----------|------------|-------------|----------|
| WAL Format | Custom binary | Protobuf (if schema evolution critical) | Recovery correctness under crash |
| Hot Tier Map | `dashmap` | `scc::HashMap` + `scc::TreeIndex` | Memory overhead under write load |
| Cold Tier | `parquet` crate | Custom columnar (if Parquet too heavy) | Write path immaturity, sync-only |
| Skeleton | Mean centroid (f32) | Scalar quantization (i8) | High false-positive wake rate |
| Page Cache | `lru` crate | Clock algorithm | Scan thrashing |
| Async I/O | `tokio::fs` + `spawn_blocking` | `tokio-uring` (future) | Windows compatibility |
| Serialization | bincode (existing) | — | No schema evolution |
| Compression | ZSTD | LZ4 (if speed critical) | — |

---

## Key Risks

### Risk 1: Parquet Write Path Immaturity (HIGH)

The `parquet` crate's `ArrowWriter` is **batch-only and synchronous**. This means:
- Each cold tier write must construct a full `RecordBatch` in memory before writing
- Writes must be dispatched via `spawn_blocking` — they block a thread
- Large batch writes memory-spike during construction
- The `ArrowWriter::close()` call also blocks — if it panics, data may be lost

**Mitigation**: Keep cold tier batches reasonably sized (e.g., 10K entries or 64MB). Use tokio `spawn_blocking` with a semaphore to limit concurrent write threads. Add integration tests that exercise the write-close-reopen-read cycle under memory pressure.

### Risk 2: Async I/O on Windows (MEDIUM)

`tokio::fs` on Windows uses IOCP (I/O Completion Ports), which has different semantics than Linux's epoll-based `tokio::fs`:
- File metadata operations (`len()`, `metadata()`) are not truly async on Windows — they block
- `tokio::fs::File::sync_all()` (`fsync`) on Windows flushes file metadata AND data, which is slow
- Rename-on-windows is not atomic across volumes

**Mitigation**: Test on Windows CI. Use `tokio::fs` consistently but verify WAL fsync behavior on Windows specifically. Consider `spawn_blocking` for all fsync calls on Windows.

### Risk 3: WAL Recovery Correctness (HIGH)

WAL recovery must handle every possible crash state:
- Partial write at end of segment (incomplete entry with valid CRC)
- Segment file truncated mid-write (no CRC for partial bytes)
- Missing segments (if file system reorders metadata writes)
- CRC mismatch (bit rot or torn write)
- Duplicate entries from replayed transactions

**Mitigation**: The WAL entry format includes `crc32` covering all fields. On recovery, scan forward through segments validating each entry. Stop at first invalid CRC or partial entry. Track `last_checkpoint_id` to skip already-flushed entries. Test with fault injection (random truncation, random byte flips).

### Risk 4: Compaction and Concurrent Writes (MEDIUM)

Segment compaction must not block the writer. If the writer is writing to the active segment while the compactor is merging older segments, there's a window where:
- The compactor reads a segment
- The writer writes new entries that reference data in that segment
- The compactor discards the old segment, losing the writer's reference

**Mitigation**: Use segment-level reference counting. The compactor only touches segments that are sealed (not the active segment). Use a manifest file that lists active segments; the compactor atomically swaps the manifest.

### Risk 5: dashing vs scc maintenance (LOW)

Both `dashmap` and `scc` are actively maintained. This is low risk. However, if the project needs range scans by metadata (not just `VectorId` lookups), `dashmap` alone won't suffice and `scc::TreeIndex` or a custom index becomes necessary.

---

## Open Questions for Proposal Phase

1. **Hot tier entry size limits**: What's the maximum number of entries in the hot tier before flush? Configurable or fixed? What triggers flush — memory bytes or entry count?

2. **Partition / cluster concept**: How does the cold tier organize data into partitions? By time range? By `VectorId` range? By metadata tag? This affects the skeleton design and the promotion/demotion API.

3. **Checkpoint strategy**: How often does the system checkpoint (write a point-in-time snapshot that allows truncating the WAL)? On every cold tier flush? On timer?

4. **Consistency model**: When does a write become visible to readers? After WAL fsync? After in-memory write? This affects the `WAL → hot tier` pipeline order.

5. **Recovery scope**: On restart, does the system rebuild the ENTIRE hot tier from the WAL, or only the portion not yet flushed to cold tier? The latter needs a checkpointer.

6. **Windows parity**: Is Windows a target platform for this phase, or Linux-only? This affects the `tokio-uring` consideration and `spawn_blocking` strategy.

7. **Skeleton for new partitions**: Are new cold partitions created with an empty skeleton (no centroid), or do we wait for enough entries to compute a meaningful mean?

8. **Bincode version**: Current dep is `bincode 1.3`. `bincode 2.0` exists with a different API. Should Phase 1 stick with 1.3 or upgrade to 2.0? (bincode 2.0 has a `config` trait that adds complexity).

---

## Ready for Proposal

**Yes**. The exploration covers all 8 key decisions with concrete recommendations, effort estimates, and risk analysis. Recommend moving to `sdd-propose` to formalize scope, approach, and rollback plan.

**Next**: `sdd-propose`
