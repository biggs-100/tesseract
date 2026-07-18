# Tasks: Phase 3 — Query Engine

## Review Workload Forecast

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: High

| Field | Value |
|-------|-------|
| Estimated changed lines | ~2000 (5 PRs: ~300+500+500+400+300) |
| 400-line budget risk | High |
| Chained PRs recommended | Yes |
| Suggested split | PR 1 (Core) → PR 2 (Planner) → PR 3 (Executor) → PR 4 (HTTP) → PR 5 (gRPC) |
| Delivery strategy | auto-chain |
| Chain strategy | stacked-to-main |

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | Core Services — EmbeddingService trait, EpisodicMemory, error variants, Cargo.toml | PR 1 | `cargo test -p tesseract-core` | N/A — library traits only | Revert embedding.rs, episodic.rs, Cargo.toml, lib.rs |
| 2 | Planner — QueryPlanner, QueryPlan, cost estimation, WeightMask | PR 2 | `cargo test -p tesseract-vql planner` | `cargo test` — parse-plan roundtrip | Remove planner.rs, revert ast.rs + lib.rs |
| 3 | Executor — QueryExecutor, execute_plan, e2e pipeline, integration tests | PR 3 | `cargo test -p tesseract-vql executor` | `cargo test` — full pipeline | Remove executor.rs, revert lib.rs |
| 4 | HTTP API — Axum router, handlers, JSON serde, test client integration | PR 4 | `cargo test -p tesseract-api` | `cargo run` + `curl POST /query` | Remove http.rs, revert api Cargo.toml + lib.rs |
| 5 | gRPC API — Tonic service, proto, feature gate | PR 5 | `cargo build -p tesseract-api --features grpc` | Compilation check only | Remove grpc.rs, proto/, build.rs |

## Phase 1: Core Services (PR 1)

- [x] 1.1 Create `tesseract-core/src/embedding.rs` — `EmbeddingService` trait (Send+Sync, `embed()`), `NoopEmbedding`
- [x] 1.2 Add `OpenAIEmbedding` impl with configurable endpoint/key/model via reqwest (feature-gated: `openai-embedding`)
- [x] 1.3 Create `tesseract-core/src/episodic.rs` — `EpisodicMemory` with in-memory footprints, weighted blend update
- [x] 1.4 Add `batch_get(ids) -> HashMap<VectorId, VectorRecord>` to `tesseract-storage/src/engine.rs`
- [x] 1.5 Add `ServiceError` error variant to `tesseract-common/src/error.rs`
- [x] 1.6 Update Cargo.toml: core (async-trait, reqwest optional, serde_json, tokio dev)
- [x] 1.7 Update lib.rs: core (embedding, episodic)
- [ ] 1.8 Extend `ast.rs` SimilarityExpr with `Vector { field, vector: Vec<f64> }` variant

## Phase 2: Planner (PR 2)

- [x] 2.1 Create `tesseract-vql/src/planner.rs` — `QueryPlan`, `FindClause`, `PlannerConfig`, `QueryPlanner`
- [x] 2.2 Implement `QueryPlanner::plan(ast)` — AST→QueryPlan with all clauses
- [x] 2.3 Derive WeightMask from WHERE predicates (equality=1.0, range=0.5, neq=0.8; IN/Between skipped)
- [x] 2.4 Implement cost: `ef × dim × 2 × ln(N) × cost_per_distance_ms × (1 + buffer)`
- [x] 2.5 WITHIN enforcement: scale ef by budget; reject if minimum cost still exceeds budget
- [x] 2.6 Default ef=50 when no WITHIN; unit tests for all planner scenarios

## Phase 3: Executor (PR 3)

- [x] 3.1 Create `tesseract-vql/src/executor.rs` — `QueryExecutor` with storage, embedder, episodic
- [x] 3.2 Implement `execute_plan`: HNSW search with mask, episodic footprint biasing, result assembly
- [x] 3.3 Text→embedding via `EmbeddingService`; skip for pre-computed vectors
- [x] 3.4 Sort ascending, apply LIMIT, build `ScoredResult` with metadata (N+1 mitigation deferred)
- [x] 3.5 WITHIN deadline check — truncate remaining candidates if elapsed > budget
- [x] 3.6 Add executor module + exports to `tesseract-vql/src/lib.rs`
- [x] 3.7 Integration: insert vectors → storage search → verify scored, episodic biasing

## Phase 4: HTTP API (PR 4)

- [x] 4.1 Create `tesseract-api/src/http.rs` — Axum Router with POST /query, POST /insert, GET /health
- [x] 4.2 POST /query handler: parse JSON → execute → return `{ results: [...] }`
- [x] 4.3 POST /insert handler: validate → insert → 201 + `{ id }`
- [x] 4.4 GET /health handler → `{ "status": "ok" }`; graceful shutdown via SIGINT/SIGTERM
- [x] 4.5 Integration tests with `axum::test` — assert 200/400/500 for all routes
- [x] 4.6 Update api Cargo.toml (axum, tower-http, serde, tokio) and lib.rs

## Phase 5: gRPC API (PR 5)

- [ ] 5.1 Create `tesseract-api/proto/tesseract.proto` — Query/Insert RPCs, ScoredRecord message
- [ ] 5.2 Create `tesseract-api/build.rs` — compile proto with prost
- [ ] 5.3 Create `tesseract-api/src/grpc.rs` — Tonic impl behind `#[cfg(feature = "grpc")]`
- [ ] 5.4 Gate deps: tonic + prost behind `grpc` feature in Cargo.toml
- [ ] 5.5 Verify: `cargo build --features grpc` compiles; default build skips gRPC
