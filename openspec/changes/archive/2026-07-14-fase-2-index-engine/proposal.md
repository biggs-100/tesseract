# Proposal: Phase 2 — Index Engine (HNSW + Topological)

## Intent

Build the ANN index engine for `tesseract-index`. Without it, the storage engine can store vectors but cannot search them — only brute-force scans are possible. This change delivers custom HNSW with weighted distance, trait-based algorithm abstraction, graph persistence, and benchmarks against FAISS.

## Scope

### In Scope
- HNSW graph with weighted distance (`DistanceComputer` trait, fused weight application)
- `TopologicalIndex` trait with `HnswIndex<D>` implementation
- Graph serialization (bincode save/load with version prefix)
- Integration with `StorageEngine` (search method, index mutations via WAL)
- Microbenchmark suite (criterion, FAISS baseline comparison)
- Error types (`IndexNotBuilt`, `GraphCorrupt`, `UnsupportedDimension`)

### Out of Scope
- IVF or DiskANN backends (future — trait allows swapping)
- PQ compression (future optimization)
- Distributed index (Phase 4)
- GPU acceleration

## Capabilities

### New Capabilities
- `hnsw-graph`: Custom HNSW with weighted distance injection, configurable ef/M/ef\_construction, multi-layer navigation, lazy deletion
- `topological-index`: `TopologicalIndex` trait abstraction for swapping ANN algorithms
- `index-persistence`: Graph save/load via bincode with format version prefix (`u32 LE`)
- `index-benchmarks`: Criterion benchmarks — recall@k, latency P50/P95/P99, build time, memory; FAISS comparison via Python script

### Modified Capabilities
- `math-foundation`: Error enum gains `IndexNotBuilt`, `GraphCorrupt`, `UnsupportedDimension` variants

## Approach

Custom Rust HNSW (Malkov & Yashunin 2016) with static dispatch over `DistanceComputer` (f32-based, generic trait). Weight mask converted to dense `[f32; dim]` once per query, applied fused during distance (single O(d) pass). Bincode serialization with `u32` version prefix. RwLock for concurrent search. Lazy deletion via mark bitset. Integration: `StorageEngine` calls `HnswIndex::insert` on write path and `HnswIndex::search` on read path. Startup tries snapshot load first, falls back to rebuild from HotStore.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `tesseract-index/src/` | New | Full crate: hnsw, distance, serialization, types, topological\_index modules |
| `tesseract-index/Cargo.toml` | Modified | Dependencies: rand, bincode, serde, criterion (dev) |
| `tesseract-index/benches/` | New | Criterion benchmarks + FAISS comparison script |
| `tesseract-storage/src/engine.rs` | Modified | `StorageEngine` gains `search()` wrapper |
| `tesseract-storage/src/types.rs` | Modified | New `OpCode` variants: `IndexInsert(0x10)`, `IndexDelete(0x11)` |
| `tesseract-common/src/error.rs` | Modified | New error variants for index errors |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Graph corruption on partial insert failure | Low | Epoch-based insert with rollback; `rebuild_from_store()` recovery |
| Recall degradation under heavy masking | Medium | Document limitation; increase `ef` for masked queries |
| Memory pressure at scale (>4GB for 1M×768d) | Medium | Memory estimation helper; future PQ compression |
| FAISS baseline unavailable on Windows | Medium | Brute-force as primary baseline; `instant-distance` as fallback |

## Rollback Plan

1. Revert `tesseract-index/` to placeholder (delete new modules)
2. Revert `tesseract-common/src/error.rs` — remove new error variants
3. Revert `tesseract-storage/src/` — remove `OpCode` variants and `search()` method
4. Revert `tesseract-index/Cargo.toml` — remove added dependencies
5. Delete `tesseract-index/benches/`

## Dependencies

- `tesseract-common` — for `Error`, `Result`, `VectorId`
- `tesseract-core` — for `WeightMask` type (converted to dense f32)
- `tesseract-storage` — for `HotStore` vector reads, WAL append, `StorageEngine` facade
- External: `bincode` (serde), `rand` (layer assignment), `criterion` (benchmarks)

## Success Criteria

- [ ] HNSW returns correct nearest neighbors (verified vs brute-force on synthetic data)
- [ ] Weighted distance produces correct metadata-aware results
- [ ] Graph save/load roundtrip preserves search quality
- [ ] `cargo test` passes with zero clippy warnings
- [ ] Benchmark suite runs: recall@k, latency P50/P95/P99, build time, memory usage
- [ ] Index persists through StorageEngine integration
