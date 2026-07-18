# Design: Phase 2 — Index Engine

## Technical Approach

Custom Rust HNSW (Malkov & Yashunin 2016) parameterized over `DistanceComputer<D>` via static dispatch. Weight masks pre-computed to dense `Vec<f32>` once per query, fused into distance loops. Bincode serialization with `u32` version prefix. RwLock for concurrent reads, tombstone deletions. FAISS baseline comparison via Python subprocess (feature-gated).

## Architecture Decisions

| Decision | Choice | Tradeoffs considered | Rationale |
|----------|--------|---------------------|-----------|
| Distance dispatch | Generic `DistanceComputer` trait | dyn dispatch, enum dispatch | Zero-cost for billion-distance hot path |
| Weight mask timing | Fused in distance loop | Pre-project query only, dual projection | Correct metric AND single O(d) pass |
| Graph lock | RwLock on entire graph | Per-node locks, COW | HNSW is CPU-bound, not I/O-bound |
| Deletion | Tombstone bitset | Hard deletion | Simple, correct; re-index at >20% tombstone ratio |
| Vector storage | Flat `Vec<f32>` (SoA) | `Vec<Vec<f32>>`, AoS | Cache-friendly sequential access during traversal |
| SIMD | Auto-vectorization | `wide` crate, intrinsics | LLVM auto-vectors f32 loops; opt later if needed |
| Serialization | bincode + u32 version prefix | MessagePack, custom binary | Already in dep tree; version prefix enables migration |
| Layer distribution | mL = 1/ln(M), L = max(1, ceil(log₂(n))) | Fixed depth, auto-tune | HNSW paper standard; configurable cap |
| Entry point | First inserted node | Centroid heuristic | Paper default; evolves naturally to highest-layer node |

## Data Flow

**Insert**: f64 vector → f32 conversion → random level via `-ln(uniform)·mL` → greedy search top→l+1 → collect ef_construction candidates at layers l..0 → connect to M nearest (bi-directional). **Search**: f64 query → f32 → optional `mask_to_dense(mask, dim)` once → HNSW traversal (greedy descent, ef-sized min-heap at layer 0) → return top-k sorted ascending. **Persistence**: `[u32 LE version] [bincode(HnswSnapshot)]` — load validates version prefix, deserializes, rebuilds graph.

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `tesseract-index/Cargo.toml` | Modify | Add rand, serde, bincode, criterion (dev) |
| `tesseract-index/src/lib.rs` | Modify | Module declarations + re-exports |
| `tesseract-index/src/hnsw.rs` | Create | `HnswIndex<D>` — insert, search, delete, layer navigation |
| `tesseract-index/src/distance.rs` | Create | `DistanceComputer` trait, `CosineComputer`, `EuclideanComputer`, `mask_to_dense` |
| `tesseract-index/src/topological_index.rs` | Create | `TopologicalIndex` trait |
| `tesseract-index/src/types.rs` | Create | `HnswConfig`, index errors |
| `tesseract-index/src/serialization.rs` | Create | `HnswSnapshot`, save/load |
| `tesseract-index/benches/hnsw_bench.rs` | Create | Criterion benchmarks |
| `tesseract-common/src/error.rs` | Modify | Add `IndexNotBuilt`, `GraphCorrupt`, `UnsupportedDimension`, `IncompatibleVersion` |

## Interfaces / Contracts

```rust
pub trait TopologicalIndex: Send + Sync {
    fn insert(&mut self, id: VectorId, vector: &[f32]) -> Result<()>;
    fn search(&self, query: &[f32], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>>;
    fn remove(&mut self, id: &VectorId) -> Result<()>;
    fn len(&self) -> usize;
    fn save(&self, writer: &mut dyn Write) -> Result<()>;
    fn load(&mut self, reader: &mut dyn Read) -> Result<()>;
}
pub struct HnswIndex<D: DistanceComputer>;
pub fn mask_to_dense(mask: &WeightMask, dim: usize) -> Vec<f32>;
pub fn f64_slice_to_f32(v: &[f64]) -> Vec<f32>;
```

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Unit | Distance f32 accuracy | Compare vs brute-force f64 |
| Unit | HNSW recall | 100 vectors, verify vs brute-force top-10 |
| Unit | Weighted search | Insert with tags, weight mask query, verify |
| Unit | Serialization roundtrip | Save → load → compare search results |
| Unit | Tombstone | Insert → delete → verify excluded |
| Unit | Re-insert after tombstone | Delete → insert → verify restored |
| Integration | StorageEngine → index | Insert via engine, search via facade |
| Bench | Recall@1/10/100 | Criterion, synthetic + SIFT1M |
| Bench | Latency P50/P95/P99 | 10K queries, μs measurement |
| Bench | Build time scaling | 1K / 10K / 100K vectors |
| Bench | Memory | RSS during construction |
| Bench | FAISS comparison | Python subprocess (feature-gated) |

## Threat Matrix

| Boundary | Applicability | Reason |
|----------|--------------|--------|
| Documentation-like paths | N/A | No doc-execution patterns |
| Git repository selection | N/A | No git operations |
| Commit / Push state | N/A | No commit or push ops |
| PR commands | N/A | No PR automation |

The FAISS benchmark script is feature-gated (`bench-faiss`) with compile-time-constant paths — no user-controlled input. The feature gate is the sole boundary control.

## Migration / Rollout

None — greenfield. The placeholder `lib.rs` is replaced with module declarations.

## Open Questions

None.
