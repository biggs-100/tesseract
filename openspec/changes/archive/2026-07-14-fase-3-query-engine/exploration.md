# Exploration: Phase 3 — Query Engine

**Change**: `fase-3-query-engine`
**Project**: VQL (Tesseract)
**Date**: 2026-07-14

---

## Current State

Phase 3 is where Tesseract "comes to life" — the VQL parser, storage engine, and HNSW index all wire together into a working query engine. Today they exist as independent crates with no integration path.

### Existing Components

| Crate | Delivered | Key interfaces relevant to Phase 3 |
|-------|-----------|-----------------------------------|
| `tesseract-vql` | ✅ Parser + AST + Grammar | `parse(&str) → Query`, `Query` struct with `similarity`, `metadata_where`, `order_by`, `limit`, `within` |
| `tesseract-storage` | ✅ Full storage layer | `StorageEngine::search(f64[], k, Option<WeightMask>) → Vec<(VectorId, f32)>`, `StorageEngine::get(id) → Option<VectorRecord>`, `HotStore` (in-memory DashMap) |
| `tesseract-index` | ✅ Full HNSW index | `TopologicalIndex` trait, `HnswIndex<D>`, `AnyIndex`, `WeightMask` support, weighted distance, serialization |
| `tesseract-core` | ✅ Core types | `VectorId(u64)`, `WeightMask(Vec<(usize, f32)>)`, `Projection` trait, `Distance` trait (f64) |
| `tesseract-common` | ✅ Error types + Result | `Error` enum with `ParseError`, `IndexNotBuilt`, `NotFound`, etc. |
| `tesseract-api` | **🔲 Placeholder** | `// Phase 2+ — API layer`. Only depends on `tesseract-vql`. No endpoints. |

### Current Data Flow

```
CLIENT:
  VQL string → tesseract_vql::parse() → Query AST
  (AST is never executed)

STORAGE ENGINE:
  StorageEngine::search(f64_vec, k, mask) → Vec<(VectorId, f32)>
  (search returns raw distance-sorted (id, score) — no metadata, no scoring)

GAP:
  There is no bridge between parsing and execution.
  There is no API layer.
  There is no query planning, cost estimation, or latency enforcement.
  There is no episodic memory.
```

### Search API Surface (current `StorageEngine::search`)

```rust
pub async fn search(
    &self,
    query: &[f64],       // raw f64 vector
    k: usize,            // number of results (serves as both ef and LIMIT)
    mask: Option<&WeightMask>,  // optional weight mask
) -> Result<Vec<(VectorId, f32)>>  // raw (id, distance) pairs
```

Key limitations:
- `k` serves double duty as both HNSW `ef` parameter AND result `LIMIT` — they should be separate
- No metadata attached in results (caller must do N+1 `get()` calls)
- No post-search scoring or re-ranking
- No latency budget enforcement
- No text-to-embedding conversion

---

## Affected Areas

| Path | Action | Why |
|------|--------|-----|
| `tesseract-vql/src/lib.rs` | MODIFY | Add `planner` and `executor` modules. Add `execute()` public function. |
| `tesseract-vql/src/planner.rs` | CREATE | Query planner: AST → `QueryPlan`, cost estimation, weight mask computation, latency budget optimization |
| `tesseract-vql/src/executor.rs` | CREATE | Query executor: plan execution against `StorageEngine`, result assembly, ORDER BY + LIMIT + WITHIN |
| `tesseract-vql/src/scoring.rs` | CREATE | Built-in scoring functions (`relevance_clicks`, `recency`, identity) |
| `tesseract-vql/src/episodic.rs` | CREATE | Per-user footprint vectors, implicit feedback, convergence |
| `tesseract-vql/src/types.rs` | CREATE | `QueryPlan`, `PlanNode`, `ScoredRecord`, `QueryContext` etc. |
| `tesseract-vql/Cargo.toml` | MODIFY | Add deps: `tesseract-storage`, `serde_json`, `tracing` |
| `tesseract-storage/src/engine.rs` | MODIFY | Add `search_with_metadata()` or extend `search()` to return `VectorRecord` for result assembly |
| `tesseract-storage/src/hot_store.rs` | MODIFY (optional) | Add episotic memory storage (if stored as `VectorRecord`) |
| `tesseract-api/src/lib.rs` | MODIFY | Replace placeholder with axum HTTP server. Define POST endpoints. |
| `tesseract-api/src/routes.rs` | CREATE | `POST /query`, `POST /insert` route handlers |
| `tesseract-api/src/lib.rs` or `src/main.rs` | CREATE | Server startup, wiring, graceful shutdown |
| `tesseract-api/Cargo.toml` | MODIFY | Add deps: `axum`, `tokio`, `serde`, `tesseract-storage`, `tesseract-vql` |

---

## Decision 1: Planner Location

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **Inside `tesseract-vql`** (co-located with parser) | Direct AST consumption; no new crate; single crate to import for query capability; parser → planner is natural pipeline | Creates dependency `tesseract-vql → tesseract-storage` (types); couples planning to storage concepts | **Medium** |
| **Separate `tesseract-planner` crate** | Clean separation of concerns; can be replaced independently; no dependency on storage types for planning | New crate to maintain; planner must still understand storage concepts indirectly; adds workspace bloat | **High** |
| **Inside `tesseract-core`** | Keeps core concepts together; no planner-specific crate | `tesseract-core` is supposed to be foundational types; planning is not foundational; would pull in storage dependencies | **High** |

### Recommendation

**Inside `tesseract-vql`**, as a new module. Rationale:

1. The planner consumes the VQL `Query` AST directly — they are already in the same crate. Keeping them together avoids a public API for internal AST traversal.
2. `tesseract-vql` already depends on `tesseract-core` (types). Adding `tesseract-storage` as a dependency gives access to `VectorRecord`, `WeightMask`, etc. — precisely what the planner needs for cost estimation and mask computation.
3. This is the established pattern: `tesseract-storage` already depends on `tesseract-index`. One more dependency chain (`vql → storage`) is reasonable.
4. A `tesseract-vql` that can both parse AND execute is the most useful consumer API. Users import one crate and get the full pipeline.

The crate will expose:
```rust
// New public API
pub fn execute(query: &str, engine: &StorageEngine) -> Result<Vec<ScoredRecord>>;
```

---

## Decision 2: Cost Model

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **Heuristic** (fixed costs per node type) | Simple to implement; zero runtime overhead; predictable behavior; easy to debug | May be inaccurate (does not adapt to hardware or data distribution) | **Low** |
| **Learned** (online learning from actual latencies) | Adapts to hardware, data, and workload; potentially better latency/recall tradeoffs | Complex profiling infrastructure; needs warm-up period; risk of feedback loops | **Very High** (Phase 5+) |
| **Hybrid** (heuristic + online calibration) | Combines predictability with adaptivity | Most complex; calibration adds surface area for bugs | **High** |

### Recommendation

**Heuristic model for Phase 3**. The cost model is straightforward:

```
cost(HNSW search) = ef × distance_computations_per_node × avg_layers_traversed × cost_per_distance
cost(metadata filter) = estimated_selectivity × total_candidates
cost(scoring + ORDER BY) = n_candidates × cost_per_scoring_fn
cost(LIMIT) = O(1) — just truncation
cost(WITHIN enforcement) = check elapsed time against budget
```

Assumptions:
- Average nodes visited per HNSW search layer: ~`ef` at layer 0, ~1 for greedy descent
- Average layers traversed: `max(1, ceil(log₂(N)))` per paper
- `cost_per_distance = O(dim)` — same for all metric types with auto-vectorization
- Metadata filter selectivity estimated from predicate type (equality: low, range: medium, IN: variable)

The planner computes a simple additive cost and checks it against the WITHIN budget. If the estimated cost exceeds the budget, it reduces `ef` to meet the constraint (recall-for-latency tradeoff).

---

## Decision 3: Embedding Generation

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **Client-side** (user sends pre-computed embedding vector) | Zero server overhead; no embedding dependency; user controls the model; simpler server architecture | User must handle embedding themselves; requires sending raw vectors over the wire; VQL `SIMILARITY(text)` syntax becomes misleading if text isn't actually accepted | **Low** |
| **Server-side** (tesseract calls embedding API/model) | Clean user experience — send text, get results; `SIMILARITY(text)` matches the VQL syntax; single client call instead of two | Requires embedding API integration (OpenAI, Cohere, or local model); adds latency, cost, and dependency; embedding model quality affects search quality | **Medium** |
| **Both** (pluggable embedding — accept either text or vector) | Maximum flexibility; graceful degradation; users can choose their tradeoff | More API surface; the `SIMILARITY` expression needs two forms: `SIMILARITY(field, 'text')` and `SIMILARITY(field, [...])` | **Medium** |

### Recommendation

**Both — pluggable embedding**. The extension is to the AST: `SimilarityExpr` currently has `query_text: String`. Extend it to support an alternative form:

```rust
pub enum SimilarityExpr {
    Text { field: String, query_text: String },
    Vector { field: String, vector: Vec<f64> },  // new
}
```

The planner/executor checks which form:
- **Text**: calls a pluggable `EmbeddingGenerator` trait to convert text → vector. The trait has a single method `generate(&self, text: &str) -> Result<Vec<f64>>`. The default implementation returns `Err(Error::EmbeddingNotConfigured)` — the user must provide an embedding client.
- **Vector**: uses the vector directly, no embedding call needed.

This keeps Phase 3 clean (no hard embedding dependency) while providing the extension point. The user adds embedding support by implementing the trait (e.g., `OpenAiEmbedding`, `OllamaEmbedding`, or `NoopEmbedding` for testing).

For Phase 3, the `EmbeddingGenerator` trait lives in `tesseract-vql` (the executor uses it). A no-op fallback allows executing queries that already have vector data.

---

## Decision 4: Episodic Memory Storage

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **In HotStore as `VectorRecord`** | No new storage infrastructure; reuses existing persistence, tier lifecycle, WAL | Footprint vectors pollute the document vector space; they are not documents and should not be searchable; tier lifecycle doesn't apply to user footprints; 1KB per user footprint adds up differently | **Low** (code reuse) but wrong semantics |
| **Separate in-memory map** (`HashMap<UserId, Vec<f64>>`) | Clean separation from document vectors; simple API; cheap lookups | No persistence across restarts; must be rebuilt from query history; no crash recovery | **Low** |
| **Separate in-memory map with WAL persistence** | Combines simplicity with durability; WAL already supports arbitrary payloads (serde_json) | WAL is designed for document writes, not user preference state; checkpoint semantics don't map cleanly to per-user footprints | **Medium** |
| **Separate `FootprintStore`** (new simple storage, memory-only + optional snapshot) | Purpose-built API; simple footprint convergence logic; easy to parallelize with main storage | New component; additional crate surface if not kept inside tesseract-vql | **Medium** |

### Recommendation

**Separate in-memory map inside `tesseract-vql`'s executor module, optionally persisted via a simple snapshot**. Rationale:

1. Episodic memory is a **user-context feature**, not a storage-tier feature. It lives with the query engine, not the document store.
2. Footprint vectors are ~1KB/user (768D f32 = 3KB, 384D = 1.5KB). For 10K concurrent users, that's ~15-30 MB — easily fits in memory.
3. The convergence logic (5-6 interactions) and implicit-feedback update are closely coupled with the query execution flow. Keeping them in the executor avoids cross-crate coordination.
4. Optional persistence: `FootprintStore::save(path)` and `FootprintStore::load(path)` using bincode on the entire map. Called at server shutdown/startup. No WAL integration needed.

```rust
pub struct FootprintStore {
    // UserId → footprint vector (pre-combined, ready to inject into query)
    footprints: HashMap<String, Vec<f64>>,
    // Track interaction count per user for convergence
    interaction_count: HashMap<String, u8>,
}

impl FootprintStore {
    pub fn get_or_default(&self, user_id: &str, dim: usize) -> Vec<f64>;
    pub fn record_click(&mut self, user_id: &str, click_vector: &[f64]);
    pub fn save(&self, path: &Path) -> Result<()>;
    pub fn load(path: &Path) -> Result<Self>;
}
```

Footprint update formula:
```
α = min(1.0, interaction_count / 6.0)  // convergence after ~6 interactions
new_footprint = (1 - α) × old_footprint + α × click_vector
```

The footprint is combined with the query vector at execution time:
```
effective_query = normalize(query_vector + footprint_vector)
```

---

## Decision 5: API Transport

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **HTTP/JSON (axum)** | Simple, well-established ecosystem; easy debugging (curl); no codegen; fast build times; axum is ergonomic Rust | No typed schemas over the wire; no streaming (SSE required for streaming); larger payloads than binary | **Low** |
| **gRPC (tonic)** | Typed schemas via protobuf; bidirectional streaming; polyglot clients; efficient binary wire format | Build complexity (protoc compiler, codegen); heavier dependency; debugging requires grpcurl or similar tools; slower Rust compile times | **Medium** |
| **Both** | Maximum client flexibility; HTTP for simple use cases, gRPC for high-performance clients | Double maintenance; two code paths to test; configuration complexity | **Very High** |

### Recommendation

**HTTP/JSON via axum for Phase 3**. Rationale:

1. Phase 3 is about proving the query engine works end-to-end. HTTP/JSON is the fastest path to a working API.
2. Axum is idiomatic Rust, integrates well with tokio (which the storage engine already uses), and has excellent performance.
3. A simple JSON API is easier to iterate on during development.
4. gRPC can be added in a dedicated phase when the API surface stabilizes and there's a concrete need for typed schemas or streaming.

### API Design (initial sketch)

```
POST /query
{
  "vql": "FIND SIMILARITY(emb, 'quantum computing') LIMIT 20 WITHIN 200ms",
  "user_id": "user-abc123"           // optional: for episodic memory
}

→ 200
{
  "results": [
    { "id": "vec_001", "score": 0.92, "metadata": { "title": "...", "year": 2024 } },
    ...
  ],
  "stats": { "latency_ms": 45, "candidates_evaluated": 240 }
}

POST /insert
{
  "id": "vec_999",
  "vector": [0.1, 0.2, ..., 0.768],
  "metadata": { "title": "...", "year": 2024 },
  "mode": "durable"
}

→ 201
{ "id": "vec_999" }
```

---

## Decision 6: Scoring Functions

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **Built-in only** (relevance_clicks, recency, identity/score) | Simple implementation; no runtime overhead; type-safe; predictable behavior | Users cannot define custom scoring; limited to what we ship | **Low** |
| **User-defined plugins (WASM)** | Maximum extensibility; users write in any WASM-compilable language; sandboxed execution | WASM runtime dependency (wasmtime); complex ABI design; host function interface; security review needed; cold start for WASM compilation | **Very High** (Phase 6+) |
| **Lua scripting** (mlua) | Embeddable; low overhead; simple syntax | Dependency + security concerns; adds ~1MB to binary; not Rust-native | **Medium** |
| **User-provided function pointer** (Rust API only) | Zero overhead; Rust-native; type-safe | Only works when using the Rust API directly; not applicable over HTTP; every user recompiles | **Low** (for library consumers) |

### Recommendation

**Built-in scoring functions for Phase 3**. Define a `ScoringFn` enum and a `Score` trait:

```rust
pub enum BuiltinScoringFn {
    /// Cosine similarity (default: returns 1 - distance as score so higher = better).
    Score,
    /// Relevance boosted by implicit feedback clicks.
    RelevanceClicks { user_id: String },
    /// Recency: newer = higher score.
    Recency { half_life_days: u64 },
    /// Weighted combination of multiple scoring functions.
    Weighted(Vec<(f64, BuiltinScoringFn)>),
}

impl BuiltinScoringFn {
    pub fn compute(&self, record: &VectorRecord, context: &QueryContext) -> f64;
}
```

The default scoring is `Score` (identity — return the HNSW distance converted to a similarity score). `RelevanceClicks` reads the episodic footprint. `Recency` uses `created_at` from `VectorRecord`.

For HTTP clients, the scoring function is selected by the `ORDER BY` clause in VQL. The parser already supports `ORDER BY relevance_clicks(current_user) DESC` — the executor maps this to the `BuiltinScoringFn` enum.

---

## Decision 7: Result Streaming

### Options

| Approach | Pros | Cons | Effort |
|----------|------|------|--------|
| **All results at once** | Simple API; matches existing HNSW behavior (collects all candidates then sorts); easy to test | Higher memory for large result sets; first-result latency includes full HNSW search time | **Low** |
| **Stream via tokio channels** (async sender/receiver) | Lower first-result latency; efficient for large result sets | Complex API for clients (HTTP streaming requires SSE or gRPC); HNSW doesn't naturally stream (it collects ef candidates before sorting); streaming adds complexity without meaningful benefit | **High** |
| **Paginated** (offset/limit with cursor) | Good UX for large result sets; bounded memory per request | Client must make multiple requests; cursor management adds complexity | **Medium** |

### Recommendation

**All results at once for Phase 3**. The HNSW algorithm inherently collects `ef` candidates before returning them — the search is not incremental. Streaming from the HNSW layer doesn't help because:

1. HNSW descent at layer 0 collects all ef candidates concurrently via a min-heap
2. The heap must fill to `ef` before you know which are the top-k
3. Sorting is O(ef log ef) which is negligible compared to distance computations

Result streaming would only make sense if we implemented a different search algorithm (e.g., IVF with sequential scan, where you can stream partition results). For HNSW at Phase 3 scale, returning all results is the right call.

**Pagination is already handled by VQL's `LIMIT` clause** — clients specify how many results they want. If they need more, they issue a new query with a cursor (future feature).

---

## Query Planning Model

### AST → QueryPlan

```
Query AST                                 QueryPlan
┌──────────────────┐                    ┌─────────────────────────┐
│ FIND SIMILARITY   │                    │ QueryPlan {              │
│   field: "emb"    │ ──[parse]────────→ │   plan_type: HnswSearch, │
│   query: "text"   │                    │   field: "emb",          │
│ WITH METADATA     │                    │   query_vector: f64[],   │
│   WHERE year > 20 │                    │   weight_mask: WeightMask│
│ ORDER BY score()  │                    │   ef: usize,             │
│ LIMIT 20          │                    │   limit: usize,          │
│ WITHIN 200ms      │                    │   scoring_fn: Score,     │
└──────────────────┘                    │   desc: bool,            │
                                         │   estimated_cost: f64,   │
                                         │   latency_budget_ms: 200 │
                                         │ }                        │
                                         └─────────────────────────┘
```

For Phase 3, the query plan is a flat struct (not a tree) because VQL queries are straightforward: SIMILARITY search + metadata filter + ordering + limit. A tree-based plan (with JOINs, subqueries, etc.) is future work.

### Cost Estimation Formula

```
estimated_cost = hnsw_search_cost + metadata_filter_cost + scoring_cost

hnsw_search_cost = ef * avg_edges * avg_layers * dist_compute_cost
  where avg_edges ≈ M (connections per node)
        avg_layers ≈ max(1, log2(N))
        dist_compute_cost ≈ dim * 10ns (auto-vectorized f32)

metadata_filter_cost = estimated_selectivity * candidates_before_filter
  where estimated_selectivity:
    - Eq predicate: ~0.001 (0.1%)
    - Range predicate: ~0.1 (10%)
    - IN predicate: ~0.05 * values.len (5% per value)
    - BETWEEN: ~0.2 (20%)

scoring_cost = limit * cost_per_fn
  where cost_per_fn:
    - Score: ~1ns (just read the distance)
    - RelevanceClicks: ~100ns (footprint vector dot product)
    - Recency: ~10ns (timestamp arithmetic)
```

### Latency Budget Optimization

When `WITHIN Nms` is specified, the planner must select the `ef` value that maximizes recall while staying within budget:

```
target_ef = max(budget_ms - fixed_overhead_ms) / cost_per_ef_step
```

Where `fixed_overhead_ms` includes: embedding generation, metadata filtering, scoring, serialization.
`cost_per_ef_step` is estimated from: `M * log2(N) * dim * 10ns` (the marginal cost of increasing ef by 1).

This is a **constrained optimization**: maximize recall (higher ef = higher recall, diminishing returns) such that `total_cost <= budget`. The planner estimates `ef` from the budget and clamps it to `[min_ef, max_ef]`.

If no WITHIN clause is given, the planner uses a default `ef` from configuration (typically 200, matching the HNSW paper default).

---

## Query Execution Flow

```
execute(query_str, engine)
  │
  ├─ 1. Parse VQL string → Query AST
  │     (existing tesseract_vql::parse)
  │
  ├─ 2. Plan query → QueryPlan
  │     a. Extract field, query_text from SIMILARITY
  │     b. Generate embedding if needed (text → vector via EmbeddingGenerator)
  │     c. Convert metadata_where → WeightMask
  │     d. Estimate cost from dimension, index size, ef, limit
  │     e. Select ef from WITHIN budget (default if absent)
  │     f. Resolve ORDER BY → ScoringFn
  │
  ├─ 3. Apply episodic memory (if user_id present)
  │     a. Load user footprint vector
  │     b. Combine with query vector: query += α * footprint
  │     c. Normalize combined vector
  │
  ├─ 4. Execute HNSW search
  │     a. engine.search(query_vector, ef, weight_mask)
  │     b. Returns Vec<(VectorId, f32)> — raw distance-sorted candidates
  │
  ├─ 5. Fetch metadata for candidates
  │     a. For each candidate: engine.get(VectorId) → VectorRecord
  │     b. Could be optimized with batch get in future
  │
  ├─ 6. Apply ORDER BY scoring
  │     a. For each candidate: score = scoring_fn.compute(record, context)
  │     b. Sort by score, take top limit
  │
  ├─ 7. Enforce WITHIN latency
  │     a. If elapsed > budget, truncate remaining candidates
  │     b. Log warning about budget exceeded
  │
  └─ 8. Return Vec<ScoredRecord>
```

### New Types

```rust
/// A scored search result returned by the executor.
pub struct ScoredRecord {
    pub id: VectorId,
    pub score: f64,                    // similarity score (higher = better)
    pub raw_distance: f32,             // original HNSW distance (lower = better)
    pub metadata: serde_json::Value,   // from VectorRecord
}

/// The query plan produced by the planner.
pub struct QueryPlan {
    pub query_vector: Vec<f64>,
    pub weight_mask: Option<WeightMask>,
    pub ef: usize,
    pub limit: usize,
    pub scoring_fn: BuiltinScoringFn,
    pub descending: bool,
    pub latency_budget_ms: Option<u64>,
    pub estimated_cost_ms: f64,
}

/// A pluggable embedding generator.
pub trait EmbeddingGenerator: Send + Sync {
    fn generate(&self, text: &str) -> Result<Vec<f64>>;
}

/// No-op embedding generator (returns an error).
pub struct NoopEmbedding;
impl EmbeddingGenerator for NoopEmbedding {
    fn generate(&self, _text: &str) -> Result<Vec<f64>> {
        Err(Error::EmbeddingNotConfigured)
    }
}

/// VectorRecord extended with score for the executor pipeline.
pub struct ScoredRecord {
    pub id: VectorId,
    pub score: f64,
    pub raw_distance: f32,
    pub metadata: serde_json::Value,
}
```

---

## Detailed Approach Comparison

### Decision Matrix

| Decision | Option Chosen | Alternative | Key Reason |
|----------|--------------|-------------|------------|
| **Planner location** | Inside `tesseract-vql` | Separate `tesseract-planner` crate | Direct AST consumption; single crate API |
| **Cost model** | Heuristic (fixed per node) | Learned (online profiling) | Simple, predictable; learned is overengineering for Phase 3 |
| **Embedding generation** | Pluggable (both text and vector) | Client-only | Flexibility; users choose their embedding strategy |
| **Episodic memory storage** | Separate in-memory `FootprintStore` | HotStore as VectorRecord | Clean separation; no document-vector pollution |
| **API transport** | HTTP/JSON via axum | gRPC (tonic) | Fastest path to working API; simpler than gRPC |
| **Scoring functions** | Built-in (enum dispatch) | WASM plugins | Simple, fast, type-safe; WASM for later |
| **Result streaming** | All at once | Tokio channels / pagination | HNSW doesn't stream naturally; pagination via LIMIT |

---

## Risks

### Risk 1: Embedding dependency management (MEDIUM)

If users choose server-side embedding, they need an embedding service running. This is an operational concern, not a code concern. The pluggable trait approach means users can start without embedding (send vectors directly) and add it later.

**Mitigation**: Default `NoopEmbedding` that returns a clear error message. Document embedding setup requirements.

### Risk 2: N+1 metadata fetches (MEDIUM)

Current `StorageEngine::search()` returns `Vec<(VectorId, f32)>`. The executor must fetch metadata for each candidate via `engine.get()` — that's N+1 round trips (one search + N gets). For ef=200, that's 200 `get()` calls per query.

**Mitigation**:
- Implement `engine.batch_get(ids: &[VectorId]) -> Result<Vec<Option<VectorRecord>>>` in `StorageEngine` — batches hot store lookups and cold store partition reads.
- Long-term: modify `search()` to return metadata directly from the index (store a metadata key alongside each vector).

### Risk 3: Latency budget estimation inaccuracy (LOW-Medium)

The heuristic cost model may over- or under-estimate actual latency. Over-estimation leaves budget on the table (lower recall than possible). Under-estimation causes WITHIN violations.

**Mitigation**:
- Conservative estimation (over-estimate by 20%) to avoid violations.
- Log actual vs. estimated latency for every query so operators can calibrate.
- The WITHIN budget is a best-effort constraint — if the engine exceeds it, results are truncated and a warning is logged. It's not a hard deadline.

### Risk 4: Episodic memory convergence tuning (LOW)

The convergence formula `α = min(1.0, interaction_count / 6.0)` and combining strategy `effective = normalize(query + footprint)` are reasonable starting points but may need adjustment for real usage patterns.

**Mitigation**:
- Make the convergence rate configurable.
- Log convergence metrics for tuning.
- Document that Phase 3 convergence is a first attempt and may need calibration.

### Risk 5: API crate compile time (LOW)

Adding axum + tokio + serde to the workspace shouldn't significantly affect compile time since these are standard ecosystem crates and `tesseract-storage` already depends on `tokio`.

**Mitigation**: None needed. Standard Rust ecosystem deps compile quickly on modern hardware.

---

## Delivery Forecast

| Component | Effort | Dependencies | Risk |
|-----------|--------|-------------|------|
| Query planner (`tesseract-vql::planner`) | Medium | None beyond existing types | Low |
| Query executor (`tesseract-vql::executor`) | Medium | planner, scoring, episodic, engine | Medium |
| Scoring functions (`tesseract-vql::scoring`) | Low | None | Low |
| Episodic memory (`tesseract-vql::episodic`) | Low | None | Low |
| `execute()` public API | Low | All above, StorageEngine::batch_get | Low |
| StorageEngine extensions (batch_get, search returning metadata) | Low-Medium | StorageEngine | Low |
| tesseract-api HTTP server | Medium | All above, axum, tokio | Low |
| Total Phase 3 | **Medium-High** | — | — |

---

## Ready for Proposal

**Yes**. All 7 key decisions are analyzed with clear recommendations. The query planning model, execution flow, cost estimation, and API design are specified. The component decomposition is clear.

**Next**: `sdd-propose`

**Skill Resolution**: paths-injected — 4 skills loaded (_shared, sdd-phase-common.md, openspec-convention.md, sdd-explore SKILL.md)
