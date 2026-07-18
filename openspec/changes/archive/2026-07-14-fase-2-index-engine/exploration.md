# Exploration: Phase 2 — Index Engine (HNSW + Topological)

**Change**: `fase-2-index-engine`
**Project**: VQL (Tesseract)
**Crate**: `tesseract-index`
**Date**: 2026-07-14

---

## Current State

The `tesseract-index` crate is a placeholder (`// Phase 2+ — index layer`). All upstream foundations are in place:

| Crate | Delivered | Key interfaces for Phase 2 |
|-------|-----------|---------------------------|
| `tesseract-common` | ✅ | `Error` enum, `Result<T>` alias. New error variants will be added (e.g., `IndexNotBuilt`, `GraphCorrupt`, `UnsupportedDimension`) |
| `tesseract-core` | ✅ | `WeightMask(Vec<(usize, f32)>)`, `Projection` trait (on `Vec<f64>`), `VectorId(u64)`, `Distance` trait (`f64`-based), `CosineDistance`, `EuclideanDistance` |
| `tesseract-storage` | ✅ | `HotStore` (dashmap), `ColdStore` (Parquet/ZSTD), `WriteAheadLog`, `StorageEngine` facade. Index will read vectors from `HotStore` and persist graph state via `StorageEngine` or direct file I/O. |
| `tesseract-index` | 🔲 Placeholder | No code. This phase builds it out. |

**Existing `Distance` trait** (extend — do not break):
```rust
pub trait Distance {
    fn distance(&self, other: &Self) -> Result<f64>;
}
```

This is symmetric, f64-based, and does not accept a weight mask. Phase 2 needs a **separate, f32-optimized distance abstraction** for the index hot path. The existing trait stays for backward compat and is used by the projection layer.

## What Phase 2 Must Deliver

1. **HNSW graph** (custom Rust-native) with weighted distance injection, configurable `ef`/`M`/`ef_construction`
2. **`TopologicalIndex` trait** — abstraction for swapping index algorithms
3. **Integration with storage engine** — reads vectors from `HotStore`, writes graph mutations through WAL
4. **Microbenchmarks** — recall, latency P50/P95/P99, build time, memory

## Affected Areas

- `tesseract-index/Cargo.toml` — add dependencies (rand, wide?)
- `tesseract-index/src/lib.rs` — module declarations + re-exports
- `tesseract-index/src/topological_index.rs` — `TopologicalIndex` trait (CREATE)
- `tesseract-index/src/hnsw.rs` — HNSW graph (CREATE, largest file)
- `tesseract-index/src/distance.rs` — f32-optimized `DistanceComputer` trait + cosine/euclidean weighted variants (CREATE)
- `tesseract-index/src/types.rs` — `HnswConfig`, index-level error types (CREATE)
- `tesseract-index/src/serialization.rs` — bincode save/load (CREATE)
- `tesseract-index/benches/` — criterion benchmarks (CREATE)
- `tesseract-storage/src/types.rs` — new `OpCode` variants for index mutations (MODIFY)
- `tesseract-storage/src/engine.rs` — `StorageEngine` gains `search()` wrapper (MODIFY)
- `tesseract-common/src/error.rs` — new error variants (MODIFY)
- `tesseract-core/src/distance.rs` — possibly extend with weighted distance method (MODIFY, optional)

---

## Key Decisions

### Decision 1: HNSW Layer Count — `ceil(log₂(n))` with configurable max

| Approach | Pros | Cons |
|----------|------|------|
| **Standard `max(1, ⌈log₂(n)⌉)`** (paper default) | Proven recall/latency; matches literature baselines | Cannot cap for latency-sensitive workloads |
| **Fixed depth** (e.g., 6 layers always) | Simpler code; deterministic memory | Poor recall at 1M+ scale (too few layers) |
| **Configurable, default = standard** | **Most flexible — best for Phase 2** | Slight API surface increase |

**Recommendation**: **Configurable** with default `L = max(1, ⌈log₂(n)⌉)` matching the HNSW paper. The config struct exposes `max_layer: Option<usize>` — `None` = auto from count, `Some(v)` = cap at `v`. This lets the user trade recall for latency without changing algorithm code.

### Decision 2: Distance Function Dispatch — Static Dispatch via Trait

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **Static dispatch (generics over `DistanceComputer` trait)** | Zero-cost abstraction; monomorphized hot path; weight mask as optional parameter | Larger binary; trait must be `Sized` for storage in `Vec` | **Medium** |
| **Dynamic dispatch (`Box<dyn DistanceComputer>`)** | Smaller binary; runtime flexibility to swap | Vtable cost on every distance call (millions of calls during traversal) | Low |
| **Enum dispatch (`match`)** | Predictable performance; no vtable | Hard to extend (must recompile for new distance); awkward API | Low |
| **Closure capture** | Rust-idiomatic; simple per-query customization | Cannot be stored easily; lifetime issues; poor ergonomics for serialization | Low |

**The weight mask changes per query, not per index.** This is the critical architectural constraint. With static dispatch:

```rust
pub trait DistanceComputer: Send + Sync + Clone {
    /// Standard distance (no mask).
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;
    /// Weighted distance (with mask). `weights` is a dense `[f32; dim]`.
    fn distance_weighted(&self, a: &[f32], b: &[f32], weights: &[f32]) -> f32;
}

pub struct HnswIndex<D: DistanceComputer> {
    distance: D,
    // ...
}

impl<D: DistanceComputer> HnswIndex<D> {
    /// Search with optional weight mask.
    pub fn search(
        &self,
        query: &[f32],
        ef: usize,
        mask: Option<&WeightMask>,
    ) -> Result<Vec<(VectorId, f32)>> {
        let weights = mask.map(|m| m.to_dense(self.dim));
        // During traversal:
        //   if let Some(ref w) = weights { self.distance.distance_weighted(a, b, w) }
        //   else { self.distance.distance(a, b) }
    }
}
```

**Recommendation**: **Static dispatch via `DistanceComputer` trait**. Vtable indirection on billions of distance calls per benchmark is measurable. Generics let the compiler inline and specialize. The weight mask is an **optional parameter at query time**, not a property of the distance function type. This lets the same index serve masked and unmasked queries.

For storage (e.g., `Vec<Box<dyn TopologicalIndex>>`), wrap in `HnswEnum`:

```rust
pub enum AnyIndex {
    Cosine(HnswIndex<CosineComputer>),
    Euclidean(HnswIndex<EuclideanComputer>),
}
impl TopologicalIndex for AnyIndex { /* delegate */ }
```

### Decision 3: Graph Serialization — bincode

| Approach | Pros | Cons |
|----------|------|------|
| **bincode** | Already in dependency tree; serde support; fast; compact | No schema evolution; platform-endian concerns |
| **Custom binary format** | Could optimize for partial deserialization | Significant implementation time; no tooling |
| **MessagePack (rmp-serde)** | Self-describing; cross-language | +15-30% size vs bincode |

**Recommendation**: **bincode**. The graph save/load path is at startup/shutdown — not on the hot path. bincode handles `Vec<f32>`, `Vec<Vec<u32>>`, etc. trivially. Add a format version prefix (`u32`) at the start of the file so future format migrations are detectable.

Serialization layout:
```
┌──────────┬─────────────────────────────────────┐
│ version  │  bincode(HnswSnapshot)              │
│ (u32 LE) │  (nodes, vectors, entry_point, ...) │
└──────────┴─────────────────────────────────────┘
```

### Decision 4: Weight Mask Application Timing — Inline during distance with dense pre-computation

| Approach | Correct? | Cost | Notes |
|----------|----------|------|-------|
| **Pre-project query only** | ❌ Wrong | O(d) once | Breaks metric (asymmetric projection) |
| **Project both vectors per distance** | ✅ Correct | 3× O(d) | Projects both vectors + distance in separate loops |
| **Apply mask inline during distance** (fused loop) | ✅ Correct | 1× O(d) | **Single fused pass**: `Σ(w² × (a-b)²)` or `Σ(w·a · w·b)` for cosine |
| **Pre-compute all projected vectors** | ✅ Correct | O(n×d) | Only for tiny datasets |

**Math correctness**: The topological projection specifies:
$$\text{dist}_S(q, v) = \| (q - v) \odot w_S \|_2$$

Expanded: `Σ(w_i² × (q_i - v_i)²)`. This is NOT the same as `‖q⊙w - v‖` (pre-project query only) — the stored vector must also be projected.

**Implementation**:
```rust
/// Convert sparse mask to dense weight vector [0..1]^dim once per query.
fn mask_to_dense(mask: &WeightMask, dim: usize) -> Vec<f32> {
    let mut dense = vec![1.0f32; dim];
    for &(idx, weight) in &mask.0 {
        dense[idx] = weight;
    }
    dense
}

/// Weighted Euclidean: sqrt(Σ w_i² × (a_i - b_i)²)
fn weighted_euclidean(a: &[f32], b: &[f32], w: &[f32]) -> f32 {
    a.iter().zip(b).zip(w)
        .map(|((&x, &y), &w_sq)| { let d = x - y; w_sq * w_sq * d * d })
        .sum::<f32>()
        .sqrt()
}
```

**Recommendation**: **Inline fused weight application** with dense mask pre-computed once per query. Two fast paths:
- **No mask**: call `distance()` directly (zero extra work)
- **With mask**: `mask_to_dense()` (O(mask.len())) then `distance_weighted()` (single fused O(d) loop)

This is the most efficient correct approach. The dense mask is pre-computed once and reused across hundreds of distance calls during a single HNSW traversal. The `w_sq = w * w` is pre-computed at conversion time.

**Pre-compute `w_sq` in the dense mask to avoid repeated multiplication.**

### Decision 5: Concurrent Search — Read-Write Lock on Entire Graph

| Approach | Pros | Cons |
|----------|------|------|
| **RwLock on entire graph** | Simple; correct; safe | One writer blocks all readers; one reader blocks all writers |
| **Fine-grained (per-node locks)** | High concurrency for writes | Complex; easy to deadlock; unlikely to benefit HNSW |
| **Copy-on-write (atomic swap)** | Readers never blocked | O(n) write cost; memory double; complex epoch management |

**HNSW traversal is CPU-bound, not I/O-bound.** A single query saturates one core for the duration (~1-10ms). Contention is from multiple concurrent queries competing for CPU, not for the lock. The RwLock protects only the graph structure (neighbor lists), which is read during search and written during inserts.

**Recommendation**: **`std::sync::RwLock` wrapping the entire `HnswData`**. Simple and correct. In a later phase, if benchmarks show contention at >1000 QPS, consider either:
- Sharding the index (partition by `VectorId` range)
- Batch inserting (accumulate inserts, rebuild graph periodically)
- Making the index read-only during search with a separate write queue

For Phase 2, documents 10-100 QPS, the RwLock will never be the bottleneck. The distance computations dominate latency by orders of magnitude.

### Decision 6: SIMD for Distance — Auto-vectorization first, `wide` crate if needed

| Approach | Stability | Effort | Speedup over scalar |
|----------|-----------|--------|---------------------|
| **Auto-vectorization** (LLVM) | ✅ Stable Rust | None | ~4-6× on SSE2, ~8× on AVX2 (for f32) |
| **`wide` crate** (`f32x4`, `f32x8`) | ✅ Stable Rust | Low | ~same as auto-vec for simple loops |
| **`core::arch` intrinsics** | ✅ Stable Rust | High | Architecture-specific; manual cleanup |
| **`std::simd`** (nightly) | ❌ Nightly only | Medium | Future option |

LLVM auto-vectorizes simple loops like dot products and L2 distance extremely well. The weighted Euclidean loop:
```rust
a.iter().zip(b).zip(w).map(|((&x, &y), &w)| { let d = x - y; w * d * d }).sum::<f32>()
```
This compiles to `vmovups` + `vsubps` + `vmulps` + `vfmadd231ps` + `vsqrtss` on AVX2 targets — no manual intrinsics needed. The loop is branchless, memory is contiguous (flat `Vec<f32>`), and LLVM recognizes the reduction pattern.

**Recommendation**: **Auto-vectorization first**. Write the distance functions with simple iterator patterns in `#[inline]` functions. Add `-C target-feature=+avx2` to release profile for x86 targets. Only add `wide` crate if **criterion benchmarks** show distance computation as the bottleneck with auto-vec. The `wide` crate or explicit intrinsics can be added as a non-breaking optimization layer under the same `DistanceComputer` trait.

### Decision 7: Memory Layout — Flat `Vec<f32>` for vectors, AoS for graph nodes

| Approach | Cache locality during search | Memory overhead | Implementation complexity |
|----------|------------------------------|-----------------|--------------------------|
| **`Vec<Vec<f32>>`** | ❌ Poor (heap per vector) | ~24 bytes per vec overhead | Low |
| **Flat `Vec<f32>` (SoA)** | ✅ Excellent (sequential access) | ~0 overhead | Medium |
| **Flat + alignment** (`repr(align(64))`) | ✅ Best (cache line alignment) | Wasted bytes (align padding) | Medium |
| **AoS: `Vec<Node>`** with fixed-size array | ✅ Good | Only if D is known at compile time | High (const generics for D) |

**Recommendation**: **Flat `Vec<f32>` for vector data + `Vec<NodeData>` for graph structure**.

```rust
pub struct HnswData<D: DistanceComputer> {
    /// All vectors stored contiguously: node i at [i * dim .. (i+1) * dim].
    vectors: Vec<f32>,
    dim: usize,
    /// Per-node metadata (flat arrays for neighbor lists).
    nodes: Vec<NodeData>,
    /// Entry point (node index at the topmost layer).
    entry_point: Option<u32>,
    /// Current number of layers.
    max_layer: usize,
    /// Config.
    config: HnswConfig,
    /// Distance function (zero-sized after monomorphization).
    distance: D,
}

struct NodeData {
    id: VectorId,
    /// Neighbor lists per layer. Only one allocation per node for all layers.
    neighbors: Vec<Vec<u32>>,  // [layer_0_neighbors, layer_1_neighbors, ...]
    enter_layer: usize,
}
```

**Why flat for vectors**: During HNSW traversal, every edge traversal computes the distance between the query and a candidate node. This means accessing `vectors[node_idx * dim ..]`. With flat storage, this is a single sequential memory read — optimal cache behavior. With `Vec<Vec<f32>>`, each access hits a different heap allocation, causing TLB and cache misses.

**Why AoS for nodes**: The neighbor lists are accessed per-node during traversal, and the access pattern is irregular (graph traversal jumps between nodes). Grouping `(id, neighbors, enter_layer)` per node keeps related data together. The `neighbors` field uses `Vec<Vec<u32>>` internally, but each node has one allocation per layer — a future optimization could flatten these into a single `Vec<u32>` with layer offsets.

### Decision 8: EF-Construction Default — 200 (paper standard)

| Value | Recall@10 (SIFT1M, M=16) | Build time (relative) | Notes |
|-------|--------------------------|----------------------|-------|
| **100** | ~92% | 0.5× | Acceptable for explorative, lower recall |
| **200** | ~97% | 1.0× | Paper default — **recommended** |
| **400** | ~99% | 2.0× | Diminishing returns above 200 |
| Auto-tune | N/A | Very high | Not for Phase 2 |

**Recommendation**: **Configurable with default = 200**. This is the standard in the Malkov & Yashunin paper and gives ~97% recall@10 at reasonable build time. The user can increase to 400 for higher recall at 2× build cost. Do NOT implement auto-tuning in Phase 2 — that's a Phase 4+ learning-system feature.

### Decision 9: Entry Point Selection — Standard (first node insertion)

| Approach | Correctness | Performance | Complexity |
|----------|-------------|-------------|------------|
| **Standard: first inserted node** | ✅ Well-known | ✅ Paper default | Low |
| **Heuristic: centroid of first N** | Unnecessary for small N | May improve early build quality | Medium |
| **Random sample → best hub** | Potential improvement | Marginal at scale | Medium |
| **User-provided** | N/A | Most flexible | Adds API surface |

**Recommendation**: **Standard HNSW paper behavior**. The entry point is the first inserted node. When inserting subsequent nodes, the entry point is only updated if the new node reaches a higher layer (i.e., `enter_layer > max_layer`). This naturally evolves the entry point to be a node in the highest layer. Standard, proven, correct.

---

## HNSW Algorithm Adaptation for Weighted Distance

### Standard HNSW (Malkov & Yashunin 2016)

1. **Insert**: Determine node's `enter_layer` via exponential distribution (`-ln(uniform(0,1)) * mL`). For each layer from `max_layer` down to `enter_layer+1`, find nearest neighbor from entry point. For `enter_layer` down to 0, find `ef_construction` nearest neighbors, prune to `M` using heuristic, connect bidirectionally.
2. **Search**: Start at entry point, top layer. Greedily descent to nearest neighbor. At bottom layer, maintain a result set of `ef` nearest neighbors (min-heap with max-distance tracking). Return top-K.

### Weighted Distance Adaptation

The only change is in the **distance computation** used during traversal. The algorithm structure remains identical:

```
At query time:
  1. Compute dense_weights = mask_to_dense(query_mask, dim)  (once)
  2. Run standard HNSW search with distance_weighted(q, v, dense_weights)
     instead of standard distance(q, v)
```

The graph structure itself is **oblivious to the weight mask** — the same graph serves both masked and unmasked queries. This is the key design insight: the topological projection affects distance computation, not the graph topology.

**Implications**:
- Graph build does NOT use weight masks (no mask at insert time)
- Graph structure is metadata-agnostic (pure vector similarity)
- Mask only affects search-time distance
- This means the graph is universal — any mask can be applied to any query against the same index

**Potential issue**: If the weight mask zeroes out many dimensions, the effective dimensionality during search is reduced. This doesn't cause correctness issues but means the HNSW graph built on full dimensions may have suboptimal structure for heavily masked queries. Mitigation: this is a research question for Phase 3+ (metadata-aware graph construction). For Phase 2, the universal-graph approach is correct, efficient, and well-understood.

### Removal Adaptation

HNSW removal is not in the original paper. Standard approaches:

**Lazy deletion** (recommended for Phase 2):
- Mark the node as deleted in a bitset `Vec<AtomicBool>`
- During search, skip deleted nodes when collecting results
- Graph edges remain (some extra traversal, minor perf impact)
- Memory is not reclaimed

**Hard deletion** (future):
- Remove node from all neighbor lists of connected nodes
- Repair connectivity by re-linking neighbors
- Complex, error-prone, and can fragment the graph

**Recommendation**: **Lazy deletion for Phase 2**. Mark-and-skip is simple, correct, and the performance impact is bounded (deleted nodes are just extra distance computations). Hard deletion can be added in a later phase.

---

## Integration with Existing Storage Engine

### Current Insert Flow (Phase 1)

```
client → StorageEngine::insert(id, vec, metadata, mode)
  → 1. WAL.append(InsertVector)   [durability]
  → 2. HotStore.insert(VectorRecord) [in-memory cache]
  → 3. Return OK
```

### Phase 2 Insert Flow

```
client → StorageEngine::insert(id, vec, metadata, mode)
  → 1. WAL.append(InsertVector)       [durability]
  → 2. HotStore.insert(VectorRecord)  [in-memory cache]
  → 3. TopologicalIndex.insert(id, &vec)  [NEW - HNSW graph update]
  → 4. Return OK
```

For Phase 2, the index update is **synchronous** (happens on the insert path). This guarantees consistency: the index always reflects the store. The HNSW insert is fast (~µs for graph manipulation, not counting distance computations during descent).

### WAL Entries for Index Mutations

New `OpCode` variants for index-specific operations:

```rust
pub enum OpCode {
    InsertVector = 0x01,
    DeleteVector = 0x02,
    UpdateMetadata = 0x03,
    // Phase 2 additions:
    IndexInsert  = 0x10,  // id + vector (redundant with InsertVector, but explicit)
    IndexDelete  = 0x11,  // id
}
```

The `IndexInsert`/`IndexDelete` opcodes are redundant with existing vector operations but provide an explicit audit trail for index recovery. During WAL replay:
1. Replay `InsertVector` → `HotStore.insert()`
2. Replay `IndexInsert` → `HnswIndex.insert()` (rebuilds graph)

### Search Flow

```
client → StorageEngine::search(query, ef, mask, limit)
  → 1. HnswIndex.search(query, ef, mask)  [ANN search with optional weight mask]
  → 2. Resolve VectorId → VectorRecord (fetch metadata from HotStore + ColdStore)
  → 3. Re-rank by metadata relevance (optional, future)
  → 4. Return top-k ScoredResult(id, score, metadata)
```

### Snapshot / Recovery

**At startup**:
1. `StorageEngine::open()` — standard WAL recovery into HotStore
2. Try to load HNSW snapshot from disk (`index.hnsw` via bincode)
3. If no snapshot exists: build index from HotStore vectors (alternative: rebuild from WAL)
4. If vectors in HotStore are newer than snapshot: re-index only the delta

**Snapshot**:
- Triggered on `StorageEngine::shutdown()` or periodic checkpoint
- Serializes `HnswData` to `index.hnsw.bin` via bincode
- Logs last-txn-id in the snapshot header for delta recovery

---

## Proposed Module Structure — `tesseract-index/src/`

```
src/
├── lib.rs                  # Crate root, re-exports
│   pub use topological_index::TopologicalIndex;
│   pub use hnsw::{HnswIndex, HnswConfig};
│   pub use distance::{DistanceComputer, CosineComputer, EuclideanComputer};
│   pub use types::*;
│
├── topological_index.rs    # TopologicalIndex trait (facade for all index types)
│   pub trait TopologicalIndex {
│       fn insert(&mut self, id: VectorId, vector: &[f32]) -> Result<()>;
│       fn search(&self, query: &[f32], ef: usize, mask: Option<&WeightMask>) -> Result<Vec<(VectorId, f32)>>;
│       fn remove(&mut self, id: &VectorId) -> Result<()>;
│       fn len(&self) -> usize;
│       fn save(&self, writer: &mut impl Write) -> Result<()>;
│       fn load(reader: &mut impl Read, config: HnswConfig) -> Result<Self> where Self: Sized;
│   }
│
├── hnsw.rs                 # HNSW graph implementation
│   pub struct HnswConfig { ... }       // M, ef_construction, max_layer, ml, ...
│   pub struct HnswIndex<D> { ... }     // Generic over DistanceComputer
│   
│   Node selection:          // SEARCH_LAYER, SELECT_NEIGHBORS_HEURISTIC
│   Edge pruning:            // standard heuristic (keep closest + diversity)
│   Bidirectional linking:   // connect both directions per layer
│   Lazy deletion:           // BitVec for deleted nodes
│
├── distance.rs             # f32-optimized distance functions for index use
│   pub trait DistanceComputer { fn distance(a, b) -> f32; fn distance_weighted(a, b, weights) -> f32 }
│   pub struct CosineComputer;    // 1.0 - dot(norm(a), norm(b))
│   pub struct EuclideanComputer; // sqrt(Σ (a-b)²)
│   
│   Functions are implemented on &[f32] slices, NOT on trait objects.
│   The trait exists for HNSW generics.
│
├── types.rs                # Index-specific types
│   pub type NodeIndex = u32; // storage-efficient node references
│   pub struct HnswConfig { ... }
│   pub enum IndexError { ... }
│
├── serialization.rs        # bincode save/load
│   pub struct HnswSnapshot<'a> { version: u32, nodes: &'a [NodeData], vectors: &'a [f32], ... }
│   pub fn save_snapshot(...) -> Result<Vec<u8>>;
│   pub fn load_snapshot(...) -> Result<(Vec<NodeData>, Vec<f32>, EntryPoint)>;
│
└── quantization.rs         # Placeholder — PQ compression for future phases
```

---

## Testing Strategy

### Unit Tests (in `#[cfg(test)] mod`, per file)

| Test Area | What | Verification |
|-----------|------|-------------|
| **Distance correctness** | cosine/euclidean weighted vs brute-force reference on random data | Within 1e-6 f32 tolerance |
| **Distance no-mask = standard** | `distance_weighted(a, b, all_ones)` == `distance(a, b)` | Exact match |
| **Mask zeroes dimension** | Weighted with w_i = 0 for dim i . Changed dim i has no effect | Distance equals original dim-i removed |
| **HNSW insert + recall** | Insert 1000 random vectors, search each, verify nearest neighbor in results | Recall@1 >= 90% vs brute-force |
| **HNSW entry point** | After N inserts, entry point is node with max_layer == max | Correct invariant |
| **Lazy deletion** | Insert, delete, search — deleted vector not in results | Excluded |
| **Serialization roundtrip** | Build index, save, load, search same query | Results identical (within tolerance) |
| **Config validation** | M=0 returns error. ef=0 returns error. dim mismatch returns error | Error returned |
| **Multi-layer navigation** | Insert sequence checks layer assignment | Correct layer per node |

### Integration Tests (in `tests/` directory)

| Test | Setup | Assertion |
|------|-------|-----------|
| **HotStore → HNSW build** | Insert 100 vectors via StorageEngine, build HNSW from HotStore | Index.len() == 100 |
| **StorageEngine::search** | Insert, search via engine facade | Returns results with metadata |
| **Search with mask** | Insert with metadata, search with predicate mask | Results respect metadata filter |
| **Recovery: snapshot → search** | Build index, snapshot, drop, reload, same query | Results match pre-snapshot |
| **Recovery: WAL replay** | Insert via WAL, crash (drop index), rebuild from WAL | Index identical |
| **Concurrent search** | 8 tokio tasks search simultaneously on RwLock-protected index | No panics, all return results |

---

## Benchmarks Plan

### Datasets

| Dataset | Dimensions | Size | Source | Purpose |
|---------|-----------|------|--------|---------|
| **Synthetic uniform** | 128d | 100K / 1M vectors | `rand` | Base recall/latency |
| **Synthetic clusters** | 128d | 100K | 10 Gaussian clusters | Recall with cluster-specific masks |
| **SIFT1M** | 128d | 1M vectors | Public benchmark | Standard comparison |
| **GIST1M** (if feasible) | 960d | 1M vectors | Public benchmark | High-dim performance |

### Metrics

| Metric | Measurement | Tool |
|--------|------------|------|
| **Recall@k** | `len(intersection(top-k_hnsw, top-k_brute)) / k` | Custom Rust harness |
| **Latency P50/P95/P99** | Per-query wall time, 1000 queries | `criterion` + `stats` |
| **Build time** | Total time to build index from vectors | `criterion` |
| **Memory usage** | `capacity() * sizeof<T>` + heap profiling | `alloc` counter / `dhat` |
| **Throughput** | Queries per second (concurrent) | Custom multithreaded harness |

### Comparison Targets

| Target | How | Notes |
|--------|-----|-------|
| **Brute-force** (O(n×d)) | Exhaustive scan — `Σ distance` | Correctness ground truth |
| **FAISS HNSW** | Python script via `faiss` + `pyo3` or subprocess | Baseline for recall/build-time |
| **`instant-distance` crate** | If compatible, run same benchmark | Rust HNSW comparison |
| **Self (no-mask vs mask)** | Same query with/without mask | Weight penalty measurement |

### Benchmark Scenarios

```
Scenario 1: Recall@10, no mask, various ef (16/32/64/128/256)
    → Compare: Tesseract HNSW vs FAISS HNSW vs brute-force
    
Scenario 2: Recall@10, with mask, various sparsity (10%/50%/90% dims masked)
    → Compare: weighted HNSW vs brute-force with same mask
    → Measured: recall degradation from mask sparsity
    
Scenario 3: Latency P50/P95/P99, ef=64
    → Compare: no-mask vs mask-sparse vs mask-dense
    → Measured: weight application overhead
    
Scenario 4: Build time vs FAISS HNSW
    → Vary: N (10K, 100K, 500K, 1M), ef_construction (100/200/400)
    
Scenario 5: Memory scaling
    → Vary: N, M (4/8/16/32), D (128/512/768)
```

### Benchmark Tooling

Phase 2 uses **`criterion`** for microbenchmarks and a **custom Python script** for FAISS comparison:

```
# Criterion benchmarks (Rust-native)
cargo bench --bench hnsw_recall
cargo bench --bench hnsw_latency
cargo bench --bench hnsw_build

# FAISS comparison script (external, Python)
python benches/compare_faiss.py --dataset sift1m --index hnsw
```

---

## Risk Analysis

### Risk 1: Graph Corruption on Insert Failure (HIGH)

If an insert fails mid-way (OOM, panic at unlucky moment), the graph enters an inconsistent state — some layers have the new node, others don't. Neighbor references are half-set.

**Likelihood**: Low (Rust panics are rare in normal operation)
**Impact**: High (corrupted graph produces wrong search results or panics)

**Mitigation**:
- Wrap insert in a generation counter. Epoch-based: increment on insert start, commit on success, rollback on failure.
- Before any insert, clone the affected neighbor list entries. On failure, restore.
- On detected corruption, provide `rebuild_from_store()` method.

### Risk 2: Performance Cliff Under Heavy Masking (MEDIUM)

A mask that zeroes 90% of dimensions doesn't reduce distance computation cost (still O(d)). Worse, the graph structure was built for full-dimensional similarity — a heavily masked query may see degraded recall because the HNSW graph optimized for full-dimension distances.

**Likelihood**: Medium (common for selective metadata predicates)
**Impact**: Medium (recall drop of 5-15% under extreme masks)

**Mitigation**:
- Phase 2 baseline: document this as a known limitation.
- Future work: metadata-aware graph construction (Phase 3+), where the graph is partitioned by metadata region and each partition maintains its own HNSW sub-graph.
- Immediate: `ef` can be increased for masked queries to compensate (recall-vs-latency tradeoff).

### Risk 3: Memory Pressure at Scale (MEDIUM)

For 1M vectors × 768d (CLIP/OpenAI scale):
- Vectors: 1M × 768 × 4 bytes = **~3 GB**
- Neighbor lists (M=16, L=~20): ~1M × 16 × 20 × 4 bytes ≈ **~1.3 GB**
- Node metadata: ~1M × 48 bytes ≈ **~48 MB**
- **Total: ~4.4 GB**

For 10M vectors: ~44 GB. This exceeds typical RAM.

**Likelihood**: Certain at scale
**Impact**: OOM on large datasets

**Mitigation**:
- Document memory budget in the config and API docs
- Provide `memory_estimate(count, dim, M)` helper function
- Future: PQ compression for stored vectors (4-8× reduction via sub-quantization)
- Future: DiskANN-style hybrid (keep graph in RAM, vectors on disk)

### Risk 4: HNSW Removal Degrades Recall Over Time (MEDIUM)

Lazy deletion (mark-and-skip) accumulates dead nodes. As more nodes are deleted, the effective neighbor connectivity decreases — the graph becomes sparser, and recall drops.

**Likelihood**: Medium (depends on workload; deletion-heavy workloads accelerate degradation)
**Impact**: Medium (linear degradation with deleted ratio)

**Mitigation**:
- Track deleted ratio. Warn at >20% deleted.
- Provide `reindex()` that rebuilds graph from live vectors only.
- Hard deletion can be added in a future phase.

### Risk 5: No FAISS Baseline Binary (LOW)

FAISS requires a C++ toolchain and FFI. Building FAISS for benchmarks adds complexity. If we cannot easily run FAISS locally, we lack a reference performance target.

**Likelihood**: Moderate (FAISS on Windows is harder than Linux)
**Impact**: Low (brute-force provides correctness baseline; FAISS results can be referenced from literature)

**Mitigation**:
- Primary baseline = brute-force (for correctness)
- Secondary baseline = `instant-distance` crate (if compatible)
- FAISS comparison as a Python script (optional, runnable on Linux CI)
- Document that SIFT1M recall should approximate FAISS HNSW figures from the original paper

### Risk 6: Stable Rust Toolchain Limits (LOW)

Cannot use `std::simd` or nightly features. The `-C target-feature=+avx2` compilation flag requires user's CPU support AVX2 (Haswell+, ~2013). ARM NEON auto-vectorization is also supported by LLVM.

**Likelihood**: Low (most modern CPUs support AVX2)
**Impact**: Low (auto-vectorization is sufficient for Phase 2; `wide` crate as fallback)

**Mitigation**: Fallback scalar path for old CPUs. Feature-detect at runtime (or let the user choose target features).

---

## Open Questions for Proposal Phase

1. **Snapshot frequency**: On shutdown only? Periodic background checkpoint? After every N inserts?

2. **HotStore/HNSW consistency**: Should the HNSW hold ALL vectors (including those in cold storage) or only the hot tier? For Phase 2, answer is "all" — but this impacts memory at scale.

3. **Concurrent insert behavior**: Should multiple concurrent inserts be batched for efficiency, or serialized through the RwLock? The RwLock serializes writes anyway, but batching could improve throughput.

4. **WeightMask normalization**: Should masks be automatically L2-normalized when applied? A mask with all 0.5 weights produces a different distance scale than one with all 1.0. This affects result ranking.

5. **Dimension type**: `f32` for storage, `f64` for computation? The current `Distance` trait uses `f64`, but `f32` is standard in the ANN ecosystem. Phase 2 uses `f32` for everything (half the memory, SIMD-friendly). The conversion boundary is at the `TopologicalIndex` trait.

6. **Benchmark dataset sourcing**: Should the project bundle a tiny synthetic dataset or provide a download script for SIFT1M/GIST1M?

7. **`no_std` support**: Is the index crate ever `no_std`? Likely no (depends on alloc + rand + std::sync). Close as "not in scope."

---

## Ready for Proposal

**Yes**. All 9 key decisions are analyzed with clear recommendations. The algorithm adaptation is specified. The storage integration flow is defined. The test and benchmark strategy is laid out.

**Next**: `sdd-propose`

**Skill Resolution**: paths-injected — 3 skills loaded (`_shared` shared, `sdd-phase-common.md`, `openspec-convention.md`, `sdd-explore` SKILL.md)
