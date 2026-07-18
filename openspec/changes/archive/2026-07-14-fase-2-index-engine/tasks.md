# Tasks: Phase 2 — Index Engine (HNSW)

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~2200 |
| 400-line budget risk | High |
| Chained PRs recommended | Yes |
| Suggested split | 5 stacked PRs |
| Delivery strategy | auto-chain |
| Chain strategy | stacked-to-main |

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: High

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Distance types + config | PR 1 | `cargo test -p tesseract-index distance` | `cargo test` tesseract-index + common | Revert distance.rs, types.rs, Cargo.toml deps |
| 2 | HNSW insert/search/multi-layer | PR 2 | `cargo test -p tesseract-index hnsw` | 100-vector recall vs brute-force | Revert hnsw.rs + unit tests |
| 3 | TopologicalIndex + persistence | PR 3 | `cargo test -p tesseract-index topological` | Save/load roundtrip test | Revert topological_index.rs, serialization.rs |
| 4 | Criterion benchmarks + FAISS | PR 4 | `cargo bench --bench hnsw_bench` | `cargo bench` 1K synthetic | Revert benches/ directory |
| 5 | StorageEngine search + WAL ops | PR 5 | `cargo test -p tesseract-storage` | Insert→search via engine facade | Revert engine.rs, types.rs OpCodes |

## Phase 1: Distance + Types (PR 1 — ~300 lines)

- [x] 1.1 Add `IndexNotBuilt`, `GraphCorrupt`, `UnsupportedDimension` to `tesseract-common/src/error.rs`
- [x] 1.2 Add `rand`, `serde`, `bincode`, `tracing`, `thiserror` to `tesseract-index/Cargo.toml`
- [x] 1.3 Create `tesseract-index/src/types.rs` — `HnswConfig`, `DistanceMetric`, serde roundtrip
- [x] 1.4 Create `tesseract-index/src/distance.rs` — `DistanceComputer` trait, `CosineComputer`, `EuclideanComputer`, `mask_to_dense()`, `f64_slice_to_f32()`
- [x] 1.5 Update `tesseract-index/src/lib.rs` — module decls + re-exports
- [x] 1.6 Unit tests: distance f32 vs f64 brute-force, mask_to_dense correctness, weighted identity

## Phase 2: HNSW Core (PR 2 — ~800 lines)

- [x] 2.1 Create `tesseract-index/src/hnsw.rs` — `HnswIndex<D>` with config, `RwLock<GraphState>`, entry point, layer tracking
- [x] 2.2 Level generation: mL = 1/ln(M), L = max(1, ceil(log2(n)))
- [x] 2.3 `insert()` — greedy search top→l+1, ef_construction candidates at layers l..0, bidirectional M-nearest
- [x] 2.4 `search()`(unweighted) — greedy descent + ef min-heap at layer 0, top-k ascending
- [x] 2.5 `search()`(weighted) — `mask_to_dense()` once, fused distance loop
- [x] 2.6 `remove()` — tombstone bitset, excluded from results
- [x] 2.7 Idempotent insert — detect duplicate `VectorId`, replace vector
- [x] 2.8 Unit tests: recall vs brute-force 100 vecs, weighted search, tombstone/re-insert, concurrent reads

## Phase 3: TopologicalIndex + Persistence (PR 3 — ~400 lines)

- [x] 3.1 Create `tesseract-index/src/topological_index.rs` — `TopologicalIndex` trait (insert/search/remove/len/save/load)
- [x] 3.2 Implement `TopologicalIndex` for `HnswIndex<D>` — delegate all methods
- [x] 3.3 Create `tesseract-index/src/serialization.rs` — `HnswSnapshot` with serde, u32 LE version prefix
- [x] 3.4 `save()` — bincode serialize with version prefix
- [x] 3.5 `load()` — validate version prefix → deserialize → rebuild graph
- [x] 3.6 Unit tests: roundtrip search identity, invalid version rejection, all persistence spec scenarios

## Phase 4: Benchmarks (PR 4 — ~400 lines)

- [x] 4.1 Create `tesseract-index/benches/hnsw_bench.rs` — criterion groups, 10 warmup / 100 measure
- [x] 4.2 Recall@k (k ∈ {1, 10, 100}) vs brute-force on 10K synthetic 128-d
- [x] 4.3 Latency P50/P95/P99 at ef ∈ {64, 128, 256}
- [x] 4.4 Build time scaling — 1K / 10K / 100K (O(N log N) check)
- [ ] 4.5 Peak RSS memory during construction
- [x] 4.6 Weighted vs unweighted latency (within 20%)
- [ ] 4.7 FAISS comparison (Python subprocess, feature-gated `bench-faiss`)
- [ ] 4.8 SIFT1M fixture download + benchmark

## Phase 5: StorageEngine Integration (PR 5 — ~300 lines)

- [x] 5.1 Add `IndexInsert(0x10)`, `IndexDelete(0x11)` to `tesseract-storage/src/types.rs` OpCode
- [x] 5.2 `StorageEngine::search()` — delegate to `HnswIndex`, return sorted `Vec<(VectorId, f32)>`
- [x] 5.3 Wire index mutations through WAL — `IndexInsert`/`IndexDelete` entries
- [x] 5.4 Integration tests: insert→search via facade, index persists through restart
