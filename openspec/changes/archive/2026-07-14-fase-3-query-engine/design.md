# Design: Phase 3 — Query Engine

## Technical Approach

Bridge VQL parsing and storage execution: planner (AST→QueryPlan), executor (plan→scored results), pluggable embedding service, episodic memory (HotStore-backed), and dual API (axum primary, tonic feature-gated). Planner inside `tesseract-vql`. EmbeddingService trait + EpisodicMemory in `tesseract-core`. API layer in `tesseract-api`.

## Architecture Decisions

### Decision: Planner Location (Spec: query-planner)
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Inside tesseract-vql | Direct AST access, single import crate | **Chosen** |
| Separate crate | Cleaner isolation but unnecessary AST coupling | Rejected |

### Decision: Cost Model (Spec: query-planner R4–R5)
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Heuristic + 20% buffer | Simple, predictable, no profiling infra | **Chosen** |
| Learned | Adaptive but complex, warmup needed | Deferred |

### Decision: Embedding (Specs: embedding-service, query-executor R2–R3)
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Pre-computed only | Zero deps but limits VQL to raw vectors | Rejected |
| Pluggable trait (both forms) | Max flexibility, graceful degradation | **Chosen** |

### Decision: Episodic Storage (Spec: episodic-memory R1)
| Option | Tradeoff | Decision |
|--------|----------|----------|
| In-memory DashMap | Fast but no persistence across restarts | Rejected |
| HotStore (hash user→VectorId) | Reuses WAL, tier lifecycle, survives restart | **Chosen** |

### Decision: API Transport (Specs: http-api, grpc-api)
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Axum HTTP/JSON | Simple, testable, curl-friendly | **Chosen (primary)** |
| Tonic gRPC | Typed, streaming, codegen | Feature-gated secondary |

### Decision: Result Delivery
| Option | Tradeoff | Decision |
|--------|----------|----------|
| All at once | Matches HNSW ef-collection semantics | **Chosen** |
| Streaming | HNSW fills a min-heap before any result is ready | Rejected |

## Data Flow

```
POST /query {"vql":"..."}
 → Axum handler
   → tesseract_vql::parse() → Query AST
     → QueryPlanner::plan(ast, ctx) → QueryPlan
       [text query] → EmbeddingService::embed() → Vec<f64>
         → EpisodicMemory::get_footprint(user_id) → Option<Vec<f64>>
           → effective = normalize(query × footprint)
             → StorageEngine::search(effective, ef, mask) → Vec<(VectorId, f32)>
               → batch_get(ids) → Vec<VectorRecord>
                 → Score, sort, apply ORDER BY + LIMIT
                   → WITHIN deadline check → truncate if exceeded
                     → JSON { results: [...], timings: {…} }
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `tesseract-vql/src/planner.rs` | Create | QueryPlanner, QueryPlan, cost estimate, WeightMask derivation, budget optimization |
| `tesseract-vql/src/executor.rs` | Create | QueryExecutor, execute_plan, result assembly, WITHIN deadline enforcement |
| `tesseract-vql/src/ast.rs` | Modify | Extend SimilarityExpr: add Vector { field, vector } variant |
| `tesseract-vql/Cargo.toml` | Modify | Add tokio, tesseract-storage, serde_json |
| `tesseract-vql/src/lib.rs` | Modify | Add planner + executor modules, expose execute() |
| `tesseract-core/src/embedding.rs` | Create | EmbeddingService trait, NoopEmbedding, OpenAIEmbedding |
| `tesseract-core/src/episodic.rs` | Create | EpisodicMemory: footprint get/update/apply via HotStore |
| `tesseract-core/src/lib.rs` | Modify | Add embedding + episodic modules |
| `tesseract-core/Cargo.toml` | Modify | Add reqwest (OpenAI HTTP client), dashmap |
| `tesseract-storage/src/engine.rs` | Modify | Add `batch_get()` for N+1 metadata fetch mitigation |
| `tesseract-api/src/http.rs` | Create | Axum Router: POST /query, POST /insert, GET /health |
| `tesseract-api/src/grpc.rs` | Create | Tonic TesseractQuery service (feature = "grpc") |
| `tesseract-api/src/lib.rs` | Modify | Add http + grpc modules |
| `tesseract-api/Cargo.toml` | Modify | Add axum, tower-http, serde, tokio, tonic (opt) |

## Interfaces

```rust
// tesseract-vql: QueryPlan + FindClause
pub struct QueryPlan {
    pub find: FindClause,
    pub weight_mask: Option<WeightMask>,
    pub ef_search: usize,           // tuned by planner
    pub limit: usize,
    pub scoring_fn: ScoringFn,
    pub descending: bool,
    pub latency_budget_ms: Option<u64>,
    pub estimated_cost_ms: f64,
}
pub enum FindClause {
    Vector(Vec<f64>),
    Text { text: String, model: String },
}

// tesseract-vql: QueryExecutor
pub struct QueryExecutor {
    storage: Arc<StorageEngine>,
    embedder: Arc<dyn EmbeddingService>,
    episodic: Arc<EpisodicMemory>,
}

// tesseract-core: EmbeddingService trait
#[async_trait]
pub trait EmbeddingService: Send + Sync {
    async fn embed(&self, text: &str, model: &str) -> Result<Vec<f64>>;
}

// tesseract-core: EpisodicMemory — keys: hash(user_id) → VectorId(u64)
pub struct EpisodicMemory {
    store: Arc<HotStore>,       // VectorRecord stores footprint as vector
    config: EpisodicConfig,
}
impl EpisodicMemory {
    pub async fn get_footprint(&self, user_id: &str) -> Result<Option<Vec<f64>>>;
    pub async fn update_footprint(&self, user_id: &str, clicked: &VectorId, query: &[f64]) -> Result<()>;
    pub fn apply_footprint(query: &[f64], footprint: &[f64]) -> Vec<f64>;
}
```

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Unit | Planner plan generation | Parse VQL, verify QueryPlan fields match clauses |
| Unit | WeightMask derivation | Variety of WHERE clauses → correct (index, weight) pairs |
| Unit | Cost estimation | Known plan params → verify cost in expected range ±20% |
| Unit | Episodic memory | Insert→update→retrieve footprint, verify bias maths |
| Unit | Embedding noop | NoopEmbedding::embed → Err(EmbeddingNotConfigured) |
| Integration | Full query e2e | Insert vectors → FIND via VQL → verify scored results |
| Integration | HTTP API | axum::test on Router, POST /query → assert 200 + results |
| Integration | WITHIN enforcement | Query with tight budget → verify rejection or truncation |

## Threat Matrix

N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary. Axum uses static route paths only.

## Migration / Rollout

No migration — all changes are additive. SimilarityExpr::Vector variant is backward-compatible (existing `query_text` still works). StorageEngine gets new `batch_get()` method; existing callers unchanged. API crate is placeholder currently; no existing consumers to break.

## Open Questions

- None. All decisions resolved in exploration + spec phases.
