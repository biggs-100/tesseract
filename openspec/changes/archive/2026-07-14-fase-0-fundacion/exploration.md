# Phase 0 Exploration: Foundation and Key Concepts — Tesseract

> **Change**: `fase-0-fundacion`
> **Project**: VQL (Vector Query Language)
> **Date**: 2026-07-14
> **State**: Exploration complete → ready for proposal

---

## 1. Mathematical Core — Topological Projection

### 1.1 The Core Problem

Traditional vector databases store embeddings in an ANN index and metadata in a separate store (PostgreSQL, SQLite). Querying means:

1. Run ANN search on the vector index → get N candidates
2. Filter candidates by metadata predicates → discard mismatches
3. Re-rank survivors

This is **bounded recall** — if the ANN top-K doesn't contain the right answer, filtering can't save you. The fundamental issue: **metadata filtering happens after vector search, not during it**.

### 1.2 Proposed Formulation: Projection onto Metadata Subspaces

Let $v \in \mathbb{R}^d$ be an embedding vector. Let $M = \{m_1, m_2, ..., m_k\}$ be metadata dimensions where each $m_i$ is either:

- **Ordinal** (dates, numeric ranges): $m_i \in \mathbb{R}$
- **Categorical** (tags, categories): $m_i \in \{c_1, c_2, ..., c_n\}$ with a learned or hand-crafted embedding $\phi(m_i) \in \mathbb{R}^{d_i}$

The **topological projection** of $v$ onto metadata subspace $S \subseteq M$ is:

$$P_S(v) = v \odot w_S \quad \text{where } w_S \in [0,1]^d$$

Where $w_S$ is a **dimension-weighting mask** learned or derived from the metadata constraints. In practice, instead of a binary mask, we use a **soft projection**:

$$w_S[i] = \sigma\left(\sum_{m \in S} \alpha_m \cdot f_m(v[i], \theta_m)\right)$$

Where:
- $\sigma$ is a sigmoid or softmax squashing function
- $\alpha_m$ is the learned importance weight for metadata dimension $m$
- $f_m$ is a compatibility function between the $i$-th embedding dimension and the metadata constraint
- $\theta_m$ are learned parameters per dimension

### 1.3 Mapping Metadata Types to Dimensional Constraints

**Ordinal metadata (dates, numbers):**
- Represent as a learned scaling vector $s_m \in \mathbb{R}^d$
- $f_m(v[i], [a,b]) = -(v[i] - \text{proj}_{[a,b]}(v[i]))^2$ — quadratic penalty for values outside range
- The projection effectively "zeroes out" dimensions incompatible with the date range

**Categorical metadata (tags, categories):**
- Each category $c$ has a learned **category vector** $u_c \in \mathbb{R}^{d_c}$ 
- Project category vectors into the embedding space via a learned linear map $W_m \in \mathbb{R}^{d \times d_c}$
- $f_m(v[i], c) = \langle v[i], (W_m \cdot u_c)[i] \rangle$ — dot product similarity in the projected space
- Multiple active categories sum their projections: $w_S = \sum_{c \in S_{cat}} \text{softmax}(W_m \cdot u_c)$

### 1.4 Complexity Reduction: From O(n) to O(log n)

**How the math enables this:**

Standard ANN search is already O(log n) with HNSW (hierarchical navigable small world graphs). The innovation here is **dimensional pruning**:

1. The weight mask $w_S$ is computed once per query (constant time with number of metadata constraints)
2. During HNSW traversal, the distance function becomes:
   $$\text{dist}_S(v_q, v_p) = \| P_S(v_q) - P_S(v_p) \|_2 = \| (v_q - v_p) \odot w_S \|_2$$
3. This is a **weighted distance** computation — same O(d) cost per distance calc, no extra index traversal cost
4. No post-filtering needed — the index already prunes by metadata during traversal

**True O(log n) emerges when:**
- The metadata mask $w_S$ is pre-computed into a **projection index** that partitions the HNSW graph by metadata region
- Each region maintains its own HNSW sub-graph
- Query routing selects the sub-graph based on $w_S$ sparsity pattern
- Worst case: full graph traversal (no metadata filter) — same as standard HNSW

### 1.5 Precision Tradeoffs

| Tradeoff | Impact | Mitigation |
|----------|--------|------------|
| **Soft mask ≠ hard filter** | Non-zero weight on filtered-out dimensions leaks noise | Train with a sparsity regularizer; apply top-k thresholding on $w_S$ |
| **Learned embeddings for categories** | Cold start for new categories | Use hierarchical category embeddings (WordNet-style); fallback to one-hot |
| **Weighted distance ≠ true distance** | May distort relative ordering | Benchmark against oracle (brute-force + filter); report recall@k drop |
| **Pre-computed sub-graphs** | Storage multiplier | Sub-graphs are lightweight (node IDs + edges), not full vector copies |

### 1.6 Relevant Research

| Area | Key Work | Relevance |
|------|----------|-----------|
| **Product Quantization (PQ)** | Jégou et al., 2011 | Compress projected vectors; subspace decomposition aligns with our dimension groups |
| **Space-Filling Curves** | Z-order, Hilbert (Morton, 1966) | Map multi-dimensional metadata to 1D ordering; could accelerate range pruning |
| **Metric Spaces** | Ciaccia, Patella, Zezula (M-tree, 1997) | Distance-based indexing without vector space assumptions — relevant for categorical |
| **HNSW** | Malkov & Yashunin, 2016 | The de facto ANN standard; our primary base index |
| **Filtered ANN Search** | Groh et al. (2021), GSA (Graph Struggling Algorithm) | HybrID approach — integrate filter into graph traversal |
| **Learned Sparse Representations** | SparTerm (Bai et al., 2020) | Learning sparse masks over vocabulary — analogous to our dimension masks |
| **ColBERT** | Khattab & Zaharia (2020) | Late interaction scoring — relevant for VQL SIMILARITY() function design |

### 1.7 Mathematical Risk

The biggest risk is that **soft projection through learned masks** may not generalize across all query patterns. A user filtering by a never-before-seen category combination will get a mask assembled from composable primitives — whether that degrades recall is the core empirical question for Phase 2 (prototyping).

---

## 2. Technology Stack Decisions

### 2.1 Rust Async Runtime: tokio vs async-std

| Aspect | tokio | async-std |
|--------|-------|-----------|
| **Ecosystem** | Dominant — hyper, tonic, axum, sqlx all use tokio | Smaller — surf, tide (mostly abandoned) |
| **WASI/WebAssembly** | Limited (tokio with wasi) | Better out of box |
| **Runtime model** | Work-stealing scheduler, multi-threaded by default | Similar, but fewer tuning knobs |
| **I/O drivers** | mio-based, battle-tested | mio-based, less battle-tested |
| **Database ecosystem** | sqlx, sea-orm, cassandra-rs all on tokio | Minimal |

**Recommendation**: **tokio**. In a database engine, every I/O path (storage, networking, WAL) is an async hot path. tokio has the ecosystem depth, especially for:
- `tokio::sync` channels for internal IPC
- `tokio::fs` for async file I/O
- `tonic` for gRPC API surface

### 2.2 Synchronization: parking_lot vs std::sync::Mutex

| Aspect | parking_lot | std::sync::Mutex |
|--------|-------------|-------------------|
| **Performance** | ~3-5x faster in contention | Slower, does syscall on every lock |
| **Poisoning** | No poisoning | Has poisoning (controversial benefit) |
| **Size** | Smaller (1 word vs 2+ words) | Larger |
| **Fairness** | Unfair (faster) | Can be fair |
| **Debugging** | Less instrumentation | Built-in deadlock detection on some platforms |

**Recommendation**: **parking_lot** for hot paths (buffer pool, page cache, index locks), **std** for cold paths and public API boundaries where poisoning is a safety concern.

### 2.3 ANN Index Choice: FAISS bindings, IVF, or HNSW?

| Approach | Pros | Cons |
|----------|------|------|
| **FAISS bindings** (via `faiss-rs` or FFI) | Battle-tested, GPU support, many index types | C++ FFI pain on Windows; build complexity; version coupling |
| **Pure Rust HNSW** (implement or use `instant-distance`) | No FFI; full control over weighted distance; SIMD via `wide` crate | Requires implementing HNSW from scratch or heavily modifying existing crates |
| **DiskANN-inspired design** | Disk-aware by design; large-scale; product quantization built-in | Most complex to implement; Rust ecosystem not ready |
| **Start with HNSW + PQ** (custom) | Full control over projection integration; Rust-native | Engineering effort upfront; must validate correctness |

**Recommendation**: **Start with a custom Rust HNSW implementation** that natively supports weighted distance functions. Reasons:
1. The entire Tesseract thesis depends on injecting $w_S$ into the distance computation — this is impossible with FAISS opaque index structures
2. FAISS's `SearchParameters` allows some customization, but the projection mask needs per-query distance modification that FAISS doesn't expose well
3. A Rust-native HNSW can use `rayon` for parallelism, `simd` for vector ops, and integrates seamlessly with the rest of the tokio-based engine
4. DiskANN patterns can be layered on top later (PQ compression, disk-resident graphs)

**Hybrid path (recommended)**: Build a Rust-native `TopologicalIndex` trait in Phase 2 with an HNSW implementation. In Phase 4+, add a FAISS backend via FFI for GPU-accelerated scenarios. The trait abstraction makes both possible.

### 2.4 Parquet Integration: arrow/parquet crates

Create | Version | Status |
|-------|---------|--------|
| `arrow` | 52+ | Active — core columnar format |
| `parquet` | 52+ | Active — read/write Parquet files |
| `datafusion` | 40+ | Active — query engine (too heavy for us, but reference) |

**Integration strategy for Tesseract:**
- **Cold tier**: Use `parquet` crate directly to serialize/deserialize vector batches + metadata columns
- **Projection pushdown**: Parquet's row group statistics (min/max/null counts for each column) can accelerate metadata pruning at the file level before decompression
- **Compression**: Parquet supports ZSTD, Snappy, etc. For embeddings, ZSTD gives 3-5x compression with acceptable decompression speed
- **Page index**: Parquet v2 page-level indexes can skip individual data pages — ideal for our dimensional pruning during cold reads

**Risk**: The `parquet` crate's write path is still maturing. Concurrent writers need external coordination. For the WAL, use a custom format (or Apache Arrow Flight).

### 2.5 WAL (Write-Ahead Log)

| Approach | Pros | Cons |
|----------|------|------|
| **Custom WAL** (append-only log with crc32 checksums) | Full control; simple; fast | Boilerplate; must implement recovery, compaction |
| **Use `pagecache` / `sled` internals** | Battle-tested log-structured storage | sled is a full key-value store; pulling it apart is hard |
| **Use `Kafka` / `Pulsar`** | Durable, distributed, replicated | Overkill for single-node WAL; operational complexity |

**Recommendation**: **Lightweight custom WAL** using `tokio::fs` with the following design:
- Append-only segments (64 MB each) with CRC32 checksums
- Each entry: `(txn_id, op_code, payload_len, payload, crc32)`
- Async fsync at configurable intervals (default: every 100ms or every 1000 ops)
- Compaction: background task merges segments, discarding overwritten entries
- Recovery: replay from last checkpoint, rebuild in-memory state

The WAL should be **generic** — the same log format works for vector inserts, metadata updates, and index mutations.

---

## 3. VQL Language Design

### 3.1 Parser Approach

| Approach | Pros | Cons |
|----------|------|------|
| **nom** | Fast, zero-copy, combinator-based, excellent Rust integration | Steep learning curve; complex error messages |
| **pest** | PEG grammar — declarative, easy to read/audit | Less performant than nom; grammar files can become unwieldy |
| **lalrpop** | LR(1) grammar; mature; good error recovery | Requires grammar→code generation step; harder to integrate with procedural code |
| **Custom recursive descent** | Full control; best error messages; easiest to debug | Most code to write; must manually handle precedence |

**Recommendation**: **nom** for the VQL parser. Reasons:
- The language is small (not SQL-sized) — a combinator approach keeps the parser close to the AST definitions
- Zero-copy parsing is important for a database query path (avoiding allocations on every parse)
- The `nom_locate` crate gives excellent error reporting with line/column tracking
- `nom` composes well — the VQL grammar naturally decomposes into clause parsers (FIND, WITH, WHERE, ORDER BY, LIMIT)

### 3.2 Grammar Design

**Tokens:**
```
Keywords: FIND, SIMILARITY, WITH, METADATA, WHERE, ORDER, BY, LIMIT, WITHIN
          AND, OR, NOT, IN, BETWEEN, ASC, DESC
Literals: STRING ('...'), NUMBER (42, 3.14), BOOL (true, false)
Identifiers: [a-zA-Z_][a-zA-Z0-9_]*
Operators: =, !=, <, >, <=, >=, AND, OR, IN, BETWEEN
Punctuation: (, ), ',', .
Special: SIMILARITY(expr, 'text') is a built-in function
        relevance_clicks(user) is an example of a user-defined scoring function
```

**Grammar (PEG-style):**

```
query        = find_clause, with_clause?, where_clause?, order_clause?, limit_clause?, within_clause?;

find_clause  = "FIND", similarity_expr;

similarity_expr = "SIMILARITY", "(", embedding_ref, ",", string_literal, ")";

embedding_ref = identifier;  -- field name, parameter, or subquery

with_clause  = "WITH", "METADATA", metadata_spec;

metadata_spec "WHERE", predicate;

predicate    = comparison_expr, { ("AND" | "OR"), comparison_expr };

comparison_expr = field, ("=" | "!=" | "<" | ">" | "<=" | ">="), (literal | identifier)
                | field, "IN", "(", literal_list, ")"
                | field, "BETWEEN", literal, "AND", literal;

order_clause = "ORDER", "BY", scoring_expr, [("ASC" | "DESC")];

scoring_expr = identifier, "(", [arg_list], ")";   -- user-defined scoring functions

limit_clause = "LIMIT", integer_literal;

within_clause = "WITHIN", integer_literal, ("ms" | "s");
```

**What makes VQL different from SQL + JSON hybrids (like PostgreSQL pgvector):**

| SQL + JSON approach | VQL approach |
|---------------------|--------------|
| `SELECT ... ORDER BY embedding <=> '[vec]' LIMIT 10` | `FIND SIMILARITY(embedding, 'text') ...` — query by semantic intent, not vector literal |
| WHERE clause runs AFTER vector search (bounded recall problem) | WHERE metadata is a first-class clause that constrains the topological projection |
| Must write `metadata->>'category'` for JSON fields | `WITH METADATA WHERE category = 'science'` — metadata is a named, structured dimension |
| No built-in response time budget | `WITHIN 150ms` — declarative latency contract, the planner uses it to choose index depth, PQ precision, and cold/hot tier |
| Scoring = ORDER BY distance | Scoring is extensible via pluggable functions (`relevance_clicks(current_user)`) that can combine vector similarity, recency, popularity, etc. |
| No concept of "dimensional pruning" | The entire query plan revolves around projecting vectors onto the metadata subspace |

### 3.3 Response Time Budget (`WITHIN` clause)

The `WITHIN 150ms` clause is a unique VQL feature that makes latency a **compile-time constraint** rather than a runtime observation:

1. The planner reads the budget
2. Estimates cost of each plan node (HNSW depth × distance computations × decompression cost)
3. Selects an execution plan that fits the budget — may reduce HNSW ef_search, use PQ approximation, or skip cold tier
4. If no plan fits, returns an error BEFORE execution (fail fast)

This turns the optimizer into a **constrained optimization problem**: maximize recall subject to latency budget.

---

## 4. Project Structure

### 4.1 Workspace Layout

```
vql/
├── Cargo.toml                    # Workspace root
├── openspec/                     # SDD artifacts
├── tesseract-core/               # Engine kernel (traits, types, mathematical primitives)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── vector.rs             # Embedding types, distance functions
│   │   ├── projection.rs         # Topological projection math (mask computation)
│   │   ├── metadata.rs           # Metadata dimension types and mapping
│   │   ├── distance.rs           # Weighted distance functions, SIMD
│   │   └── types.rs              # Core types: VectorId, MetadataValue, Timestamp
│   └── tests/
│
├── tesseract-storage/            # Hot/cold tier storage
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── wal.rs                # Write-ahead log
│   │   ├── hot_store.rs          # In-memory buffer + indexed store (RAM)
│   │   ├── cold_store.rs         # Parquet-backed tier (disk)
│   │   ├── page_cache.rs         # Page/buffer cache for cold reads
│   │   └── compaction.rs         # WAL compaction, tier promotion
│   └── tests/
│
├── tesseract-index/              # Topological index
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── hnsw.rs               # HNSW graph with weighted distance
│   │   ├── topological_index.rs  # TopologicalIndex trait
│   │   ├── projection_index.rs   # Metadata-routed sub-graphs
│   │   └── quantization.rs       # PQ compression for cold tier
│   └── tests/
│
├── tesseract-vql/                # VQL parser and planner
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── parser.rs             # nom-based parser
│   │   ├── ast.rs                # Abstract syntax tree types
│   │   ├── grammar.rs            # Grammar combinators
│   │   ├── planner.rs            # Query planning + optimization
│   │   └── executor.rs           # Plan execution
│   └── tests/
│
├── tesseract-api/                # Public API surface
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── grpc.rs               # gRPC service (tonic)
│   │   ├── http.rs               # REST/HTTP endpoints (axum)
│   │   └── cli.rs                # CLI tool
│   └── tests/
│
└── tesseract-common/             # Shared utilities
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── error.rs              # Error types (thiserror)
        ├── config.rs             # Configuration types (serde)
        └── telemetry.rs          # Tracing/observability (tracing, opentelemetry)
```

### 4.2 Dependency Injection Pattern

Each crate depends downward only:

```
tesseract-api
    → tesseract-vql (parser + planner)
    → tesseract-core (types + projections)
    → tesseract-storage (persistence)
    → tesseract-index (ANN search)
```

Cross-cutting: `tesseract-common` is used by all crates (errors, config, telemetry).

### 4.3 Initial Cargo.toml Dependencies (per crate)

**tesseract-core**: `ndarray`, `rand`, `serde`, `thiserror`, `tracing`
**tesseract-storage**: `tokio` (fs sync), `parquet`, `arrow`, `crc32fast`, `serde`, `bincode`
**tesseract-index**: `rayon`, `wide` (SIMD), `rand`, `serde`, `tesseract-core`
**tesseract-vql**: `nom`, `nom_locate`, `tesseract-core`, `tesseract-index`, `tesseract-storage`
**tesseract-api**: `tonic`, `prost`, `axum`, `tower`, `tesseract-vql`
**tesseract-common**: `thiserror`, `serde`, `tracing`, `serde_yaml`, `opentelemetry`

---

## 5. Key Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| **Topological projection degrades recall on unseen metadata combos** | High | Phase 2 must include a microbenchmark suite with held-out combinations; plan B is a hybrid scorer that falls back to post-filter |
| **Custom Rust HNSW performance** | Medium | Validate against FAISS HNSW in Phase 2; use `criterion` benchmarks; consider SIMD via `wide` or `intel-mkl` |
| **Parquet write path immaturity** | Medium | Keep WAL as primary write path; cold tier writes are batch/background — latency-tolerant |
| **No existing VQL benchmark** | Medium | Must create a synthetic dataset and query workload in Phase 2; borrow from BEIR, MS MARCO, and SIFT1M |
| **Rust async + hot path contention** | Low | Use `tokio-console` and `flamegraph` early; profile buffer pool and index traversal |
| **Project complexity for single-phase implementation** | Medium | Phase 0 ends here — all subsequent phases are scoped small enough for single sessions |

---

## 6. Approach Comparison Summary

| Area | Recommended | Phase |
|------|-------------|-------|
| Projection method | Learned soft mask $w_S$ over embedding dimensions | Phase 2 (prototype) |
| ANN Index | Custom Rust HNSW with weighted distance | Phase 2 |
| Async runtime | tokio | Phase 2 |
| Mutex strategy | parking_lot for hot path, std for cold | Phase 2 |
| Parser | nom combinators | Phase 2 |
| Storage format | Parquet (cold) + custom WAL (hot) | Phase 2 |
| API surface | gRPC (primary) + HTTP/REST (secondary) | Phase 2 |
| Query budget | `WITHIN` clause → constrained planner | Phase 2 |
| Workspace | 6-crate modular layout | Phase 2 |

---

## 7. What Phase 0 Enables

With this foundation established, the next phases can proceed confidently:

| Phase | What it consumes from Phase 0 |
|-------|-------------------------------|
| **Proposal** | The recommended approaches for each domain |
| **Spec** | Formal requirement definitions for each crate |
| **Design** | Detailed architecture diagrams, data flow, module boundaries |
| **Tasks** | Concrete implementation tasks broken down by crate |
| **Apply** | Actual code written against the crate structure |
| **Verify** | Benchmarks against the recall/latency tradeoffs described here |

---

## 8. Open Questions for the Proposal Phase

1. **Cold start strategy**: How do we bootstrap category embeddings when the first vectors arrive?
2. **Transactional semantics**: Snapshot isolation? Serializability? What consistency model?
3. **Multi-tenancy**: Does the projection mask account for per-user/tenant embeddings?
4. **Hardware target**: AVX2-only or also AVX-512? GPU integration timeline?
5. **License**: AGPL like most vector DBs, or Apache/MIT?
