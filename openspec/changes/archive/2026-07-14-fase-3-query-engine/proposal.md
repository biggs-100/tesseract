# Proposal: Phase 3 — Query Engine

## Intent

Bridge VQL parsing and storage execution into a working query engine. Today `tesseract-vql` parses to an AST that nobody executes, and `tesseract-storage` searches raw vectors without metadata or scoring. Phase 3 wires them together with query planning, cost-based latency enforcement, episodic memory, and an HTTP API.

## Scope

### In Scope
- Full stack: planner + executor + API + episodic memory
- `query-planner`: AST → QueryPlan, cost estimation, WeightMask derivation, WITHIN budget optimization
- `query-executor`: Plan execution, result assembly, ORDER BY + LIMIT + WITHIN
- `episodic-memory`: FootprintStore, implicit feedback, preference biasing
- `embedding-service`: Pluggable EmbeddingService trait (noop default, OpenAI-compatible)
- `http-api`: Axum REST API (POST /query, POST /insert, health check)
- `grpc-api`: Tonic gRPC (feature-gated, secondary)
- Scoring: built-in (score, relevance_clicks, recency)

### Out of Scope
- WASM plugin scoring (deferred)
- Streaming results (all-at-once for Phase 3)
- Learned cost model (heuristic + 20% conservative buffer)
- gRPC as primary transport (tonic behind feature flag)

## Capabilities

### New Capabilities
- `query-planner`: QueryPlan construction from VQL AST, cost estimation, WeightMask derivation, latency budget optimization
- `query-executor`: Plan execution against StorageEngine, result assembly, LIMIT enforcement, WITHIN deadline
- `episodic-memory`: Per-user footprint vectors, implicit feedback update, preference biasing at query time
- `http-api`: Axum REST API (POST /query, POST /insert, health check, graceful shutdown)
- `grpc-api`: Tonic gRPC Query RPC (feature-gated, behind `grpc` feature flag)
- `embedding-service`: Pluggable `EmbeddingGenerator` trait (noop returns error, OpenAI-compatible impl)

### Modified Capabilities
None. All capabilities are additive — no existing spec behavior changes.

## Approach

Co-locate planner + executor inside `tesseract-vql` (extends the crate from parser to full query engine). Add `execute(query_str, &StorageEngine) -> Vec<ScoredRecord>` as the public entry point. Episodic memory lives as an in-memory `FootprintStore` inside `tesseract-vql`. The `tesseract-api` crate gets an axum HTTP server that wires storage, VQL, and footprint store together. gRPC is added behind a feature flag but is secondary.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `tesseract-vql/src/lib.rs` | Modified | Add `execute()` public fn, export planner/executor modules |
| `tesseract-vql/src/planner.rs` | **New** | Query planner module |
| `tesseract-vql/src/executor.rs` | **New** | Query executor module |
| `tesseract-vql/src/scoring.rs` | **New** | Built-in scoring functions |
| `tesseract-vql/src/episodic.rs` | **New** | FootprintStore + convergence |
| `tesseract-vql/src/types.rs` | **New** | QueryPlan, ScoredRecord, etc. |
| `tesseract-vql/Cargo.toml` | Modified | Add deps: tesseract-storage, serde_json, tracing |
| `tesseract-core/src/lib.rs` | Modified | Add VectorRecord metadata accessors |
| `tesseract-storage/src/engine.rs` | Modified | Add `batch_get()` for N+1 mitigation |
| `tesseract-api/src/lib.rs` | Modified | Replace placeholder with axum server |
| `tesseract-api/src/routes.rs` | **New** | POST /query, POST /insert handlers |
| `tesseract-api/Cargo.toml` | Modified | Add axum, tokio, serde, tracing |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Embedding service is complex to integrate | Medium | Pluggable trait; noop returns clear error; embedder is user-provided |
| N+1 metadata fetches hurt perf | Medium | Add `batch_get()` to StorageEngine |
| Cost model underestimates latency | Low-Medium | 20% conservatism buffer; log actual vs estimated; truncate on budget miss |
| Footprint convergence wrong for real usage | Low | Make convergence configurable; log metrics for tuning |

## Rollback Plan

Revert the `execute()` public API surface — keep `tesseract-vql` as parse-only crate. Remove `tesseract-api` changes. The proposal is purely additive; no existing functionality is removed, so rollback is crate-scoped deletion.

## Dependencies

- `tesseract-storage` with `batch_get()` support
- `tesseract-vql` parser (existing — Phase 2)
- External: axum, tokio, serde+json, tonic (feature-gated), prost (feature-gated)

## Success Criteria

- [ ] `FIND SIMILARITY(...) WITH METADATA WHERE ... LIMIT N WITHIN Nms` works end-to-end
- [ ] Metadata WHERE clauses correctly produce WeightMasks
- [ ] WITHIN latency budget: planner rejects queries that can't meet budget
- [ ] Episodic memory: user footprint biases search results
- [ ] Axum API: POST /query returns JSON results
- [ ] Tonic gRPC: Query RPC compiles (feature-gated)
- [ ] Embedding service trait: pluggable, noop returns error with message
- [ ] All tests pass with zero clippy warnings
