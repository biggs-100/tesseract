# Design: Production Readiness

## Technical Approach

4 stacked PRs (stacked-to-main) addressing 12 issues across correctness, security/ops, quality, and performance. Each PR is independently revertable. No data migration or schema changes.

---

## PR1 — Core Correctness

### ADR-001: Panics → Result with typed errors

| Aspect | Decision |
|--------|----------|
| **Choice** | Extend the existing `tesseract_common::error::Error` enum (no `anyhow`). Add `InvalidVector(String)` for normalization failures. `NormalizedVector::new(Vec<f64>)` returns `Result<Self>`. `PageCache::new(0)` returns `Err(Error::InvalidConfig(...))`. `register_field` returns `Result<()>`. |
| **Rationale** | The workspace already uses `thiserror` in `tesseract-common`. A unified typed enum is consistent with existing variants like `DimensionMismatch`, `IoError`. |
| **Why not anyhow** | `anyhow` loses type information. Callers in the query pipeline need to match specific variants for error responses. |
| **Consequences** | Serde's `#[serde(try_from)]` already calls `TryFrom<Vec<f64>>` — with `Result` return, deserialization returns errors instead of panicking. All call sites of `NormalizedVector::new` and `PageCache::new` (production + tests) change from `unwrap()`/`expect()` to `?`. Tests for the panic path (`#[should_panic]`) change to `assert!(result.is_err())`. |

### ADR-002: Lock poisoning → explicit map_err

| Aspect | Decision |
|--------|----------|
| **Choice** | Add `Error::LockPoisoned` variant. Replace ALL `.lock().unwrap()` and `.lock().expect()` in production code with `.lock().map_err(|_| Error::LockPoisoned)?`. `PageCache` methods change return types from `Option<Page>`/`usize` to `Result<...>` because they internally use `std::sync::Mutex` (poisonable). |
| **Rationale** | `EpisodicMemory` already does this correctly with `map_err`. A poisoned `Mutex` means a thread panicked while holding the lock — the system state is potentially corrupt. Propagating is safer than silently continuing. |
| **Why not `.ok()?`** | `.ok()?` discards the error context — the caller doesn't know a panic occurred. `LockPoisoned` preserves the information for diagnostics. |
| **Consequences** | 20+ lock sites in `engine.rs`, `page_cache.rs`, `cold_store.rs`, `episodic.rs`, `hnsw.rs` need updating. `PageCache` API changes: `get()` → `Result<Option<Page>>`, `len()` → `Result<usize>`. |

### ADR-003: WAL serialization — rename + split

| Aspect | Decision |
|--------|----------|
| **Choice** | Rename `Error::BincodeError(String)` → `Error::SerializationError(String)`. Add `Error::JsonError(String)` for `serde_json` failures. Update `From<bincode::Error>` to produce `SerializationError`. Update all `serde_json::to_vec`/`from_slice` call sites in `engine.rs` from `.map_err(|e| Error::BincodeError(...))` to `Error::JsonError`. |
| **Rationale** | The spec says "dual format" — bincode for checkpoint/metadata, JSON for WAL payloads. Having distinct variants makes the error accurate: a JSON serialization failure should not report as a bincode error. |
| **Consequences** | `OpCode::try_from` maps unknown opcodes to `SerializationError`. All match arms on `BincodeError` in tests need updating. The `From<bincode::Error>` impl changes. |

### ADR-004: Graceful shutdown via axum::serve + tokio::signal

| Aspect | Decision |
|--------|----------|
| **Choice** | Use `axum::serve(listener, router).with_graceful_shutdown(shutdown_signal)` where `shutdown_signal` waits on `tokio::signal::ctrl_c()` and (on Unix) `tokio::signal::unix::signal(SIGTERM)`. On signal: (1) axum stops accepting new connections, (2) drain HotBuffer, (3) flush WAL, (4) persist HNSW index. Timeout via `tokio::time::timeout` wrapping `storage.shutdown()`, configurable via `TESSERACT_SHUTDOWN_TIMEOUT_SECS` (default 30). |
| **Rationale** | `with_graceful_shutdown` is the idiomatic axum pattern. `tokio::signal` is already available (tokio features = `["full"]`). On Windows, `ctrl_c()` works; SIGTERM is Unix-only. |
| **Why not manual `tokio::select!`** | `with_graceful_shutdown` handles connection draining and in-flight request completion for free. |
| **Consequences** | `main.rs` adds signal handling. `StorageEngine::shutdown()` adds hot_buffer drain. `_lifecycle_handle` cancelled on shutdown. `#[cfg(unix)]` for SIGTERM. |

---

## PR2 — Security/Ops

### ADR-005: Embedding timeout + retry

| Aspect | Decision |
|--------|----------|
| **Choice** | `reqwest::Client::builder().timeout(Duration::from_secs(30))` for per-request timeout. Manual retry loop with exponential backoff (base 1s, double, max 3 retries) for HTTP 429/5xx. Config via `OpenAIEmbeddingConfig { timeout_secs, max_retries, base_delay_ms }`. |
| **Rationale** | Manual loop avoids boxing into Tower's `Service` trait — the embedding call is a plain async fn behind `#[async_trait]`. Exponential backoff is ~20 lines; no dependency needed. |
| **Why not tower::retry** | `tower::retry::RetryLayer` operates on `Service` impls. The embedder is used directly, not in a Tower stack. |
| **Consequences** | `OpenAIEmbeddingService` gets a config struct. `reqwest::Client::new()` changes to builder. `embed()` returns `Error::ServiceError` after timeout/retry exhaustion. |

### ADR-006: Auth — inline module in tesseract-api

| Aspect | Decision |
|--------|----------|
| **Choice** | Auth as `tesseract-api::auth` module (NOT a separate crate). Trait `AuthProvider` with `ApiKeyAuth` and `JwtAuth` (HS256). Axum middleware layer for HTTP, tonic interceptor for gRPC. Config via `TESSERACT_AUTH_MODE` (none/api-key/jwt/both), default `"none"`. JWT secret from `TESSERACT_JWT_SECRET`; API keys from `TESSERACT_API_KEYS` (comma-separated, in-memory HashMap). Health endpoints exempt from auth. |
| **Rationale** | A separate crate would be extracted if auth grows or is reused outside `tesseract-api`. For MVP, inline reduces complexity. HMAC-HS256 over RSA: no PKI needed. In-memory API keys: no disk I/O on auth checks. |
| **Consequences** | Add `jsonwebtoken` to `tesseract-api` deps. Non-health routes wrapped in auth middleware. gRPC wraps in tonic interceptor. HTTP 401 / gRPC UNAUTHENTICATED. Default `"none"` preserves existing behaviour. No migration. |

### ADR-007: Per-IP rate limiting with custom Tower layer

| Aspect | Decision |
|--------|----------|
| **Choice** | Custom per-IP `RateLimiter` using `tokio::sync::RwLock<HashMap<IpAddr, SlidingWindow>>`. Default 100 req/min via `TESSERACT_RATE_LIMIT_RPM`. IP from `X-Forwarded-For` or socket addr. |
| **Rationale** | `tower::limit::RateLimitLayer` is global-only. Sliding-window counter is simple, correct, no external deps beyond tokio. |
| **Why not governor** | `governor` adds a dependency for ~100 lines of custom code. Not worth it for MVP. |
| **Consequences** | `rate_limiter.rs` in `tesseract-api`. Custom `tower::Layer`. Returns HTTP 429 with `Retry-After`. Query timeout (`TESSERACT_QUERY_TIMEOUT_SECS`, default 30) in `QueryExecutor::execute()` via `tokio::time::timeout`. |

### ADR-008: OpenTelemetry with health differentiation

| Aspect | Decision |
|--------|----------|
| **Choice** | SDK: `opentelemetry` + `opentelemetry-prometheus` + `tracing-opentelemetry`. Liveness (`GET /health/liveness`) = 200 always (lightweight, no deps). Readiness (`GET /health/ready`) checks WAL open, index loaded, HotBuffer responding (timeout 5s). Metrics (`GET /metrics`) via Prometheus exporter. Query pipeline: `#[instrument]` on parse, plan, execute, search, apply_topological_bias. Structured JSON logging via `tracing-subscriber::fmt().json()`. |
| **Rationale** | Liveness should never depend on downstream services (k8s restart loop). Readiness validates storage engine is operational. OTel keeps the door open for OTLP exporters later. |
| **Why not raw prometheus** | `prometheus` crate works but locks into Prometheus format. OTel provides abstraction + Prometheus exporter in one line. |
| **Consequences** | Add 3 OTel crates to `tesseract-api`. `main.rs` initialises OTel SDK, shuts down during graceful shutdown. New routes: `/metrics`, `/health/liveness`, `/health/ready`. Readiness calls `StorageEngine::is_ready()` (new method). `TESSERACT_LOG_FORMAT = "json"` or `"text"`. |

---

## PR3 — Quality

### ADR-009: TestEmbeddingService

| Aspect | Decision |
|--------|----------|
| **Choice** | `TestEmbeddingService` in `tesseract-core/src/test_embedding.rs` behind `#[cfg(feature = "test-embedding")]`. Feature added to `tesseract-core/Cargo.toml`. Algorithm: SHA-256(input) → first N bytes → `[f64; dim]` → L2 normalize. Default dim 128. Deterministic by construction. |
| **Rationale** | `#[cfg(test)]` restricts to unit tests within tesseract-core only. Feature gate allows workspace E2E tests to enable via `[dev-dependencies]`. |
| **Consequences** | Implements `EmbeddingService` trait. E2E test in `tesseract-api/tests/` spins up server, inserts via test-embedding, queries via HTTP, verifies scored results. |

### ADR-010: CI hardening (advisories + coverage)

| Aspect | Decision |
|--------|----------|
| **Choice** | Add `cargo deny check advisories` to CI (advisories + licenses in one tool). Add `cargo llvm-cov --workspace --exclude tesseract-pg` with 70% warning threshold (non-blocking). Coverage report as CI artifact. |
| **Rationale** | `cargo-deny` is more comprehensive than `cargo-audit`. `tesseract-pg` excluded (needs PostgreSQL dev environment to compile). |
| **Consequences** | GitHub Actions CI gets two new jobs: `audit` and `coverage`. Both run on every push and PR. Coverage <70% emits warning only, does not block. |

---

## PR4 — Performance

### ADR-011: HNSW concurrency — parking_lot + tokio::sync::RwLock

| Aspect | Decision |
|--------|----------|
| **Choice** | Replace `std::sync::RwLock<()>` with `parking_lot::RwLock<()>` in `HnswIndex`. Replace `tokio::sync::Mutex<AnyIndex>` with `tokio::sync::RwLock<AnyIndex>` in `StorageEngine`. Feature flag `legacy-locking` to revert. |
| **Rationale** | `parking_lot` is 2–5x faster than `std::sync::RwLock`, doesn't poison (safe release on thread panic), fair writer policy. `tokio::sync::RwLock<AnyIndex>` allows concurrent reads (searches) while serializing writes — `Mutex` serialized everything. |
| **Deadlock risk** | Two lock layers: outer `tokio::sync::RwLock` (async, StorageEngine) + inner `parking_lot::RwLock` (sync, HnswIndex). `search()` acquires read on both; `insert()` acquires write (outer) + uses `&mut self` (inner). No inversion path. |
| **Consequences** | Add `parking_lot = "0.12"` to `tesseract-index` deps. `HnswIndex::lock` type changes. `StorageEngine.index` from `Option<Mutex<AnyIndex>>` to `Option<RwLock<AnyIndex>>`. All `idx.lock().await` for searches become `idx.read().await`; for inserts `idx.write().await`. Feature flag in `Cargo.toml`. |

### ADR-012: Dead code removal

| Aspect | Decision |
|--------|----------|
| **Choice** | Audit each `#[allow(dead_code)]` / `#[expect(dead_code)]`: (1) `StorageEngine` struct attr → remove (struct IS used). (2) `HotStore.config` → remove `#[allow]`, keep field with comment. (3) `SegmentWriter.path` → remove unused field. (4) `tesseract-cluster::replication` → investigate and remove or implement. |
| **Rationale** | Each suppression hides a real warning. Removing forces conscious decisions about unused code. |
| **Consequences** | 4 files modified. `SegmentWriter` loses the `path` field. No behavioral change. |

---

## Data Flow

```
Client → HTTP/gRPC → Auth Layer → Rate Limiter → Router
                                                     │
                                           ┌─────────┼─────────┐
                                           ▼         ▼         ▼
                                      /query      /insert  /health/*
                                           │         │
                                           ▼         ▼
                                     QueryExec.  StorageEngine
                                           │         │
                                     ┌─────┼──┐  ┌──┼───────┐
                                     ▼     ▼  ▼  ▼  ▼       ▼
                                   Parse Plan Exec WAL Hot  Index
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `tesseract-common/src/error.rs` | Modify | Add LockPoisoned, SerializationError, JsonError, InvalidVector, InvalidConfig |
| `tesseract-core/src/distance.rs` | Modify | NormalizedVector::new returns Result |
| `tesseract-core/src/topological.rs` | Modify | register_field returns Result<()> |
| `tesseract-core/src/embedding.rs` | Modify | OpenAIEmbeddingService: timeout + retry |
| `tesseract-core/src/test_embedding.rs` | Create | TestEmbeddingService (feature-gated) |
| `tesseract-core/Cargo.toml` | Modify | Add test-embedding feature |
| `tesseract-storage/src/page_cache.rs` | Modify | PageCache methods return Result; remove .expect() |
| `tesseract-storage/src/engine.rs` | Modify | Lock poisoning, shutdown extended, serialization variants, RwLock for index |
| `tesseract-storage/src/wal.rs` | Modify | Remove unused path field from SegmentWriter |
| `tesseract-storage/src/hot_store.rs` | Modify | Remove #[allow(dead_code)] from config |
| `tesseract-storage/src/cold_store.rs` | Modify | Lock .expect() → map_err |
| `tesseract-index/src/hnsw.rs` | Modify | parking_lot::RwLock; legacy-locking feature flag |
| `tesseract-index/Cargo.toml` | Modify | Add parking_lot dep, legacy-locking feature |
| `tesseract-api/src/http.rs` | Modify | Auth middleware, rate limiter, health endpoints |
| `tesseract-api/src/grpc.rs` | Modify | Auth interceptor |
| `tesseract-api/src/auth.rs` | Create | Auth module: AuthProvider trait, ApiKeyAuth, JwtAuth |
| `tesseract-api/src/rate_limiter.rs` | Create | Per-IP rate limiter |
| `tesseract-api/src/main.rs` | Modify | Signal handling, OTel init, graceful shutdown, structured logging |
| `tesseract-api/Cargo.toml` | Modify | Add jsonwebtoken, opentelemetry, opentelemetry-prometheus, tracing-opentelemetry |
| `tesseract-cluster/src/replication.rs` | Modify | Dead code cleanup |
| `.github/workflows/ci.yml` | Modify | Add audit + coverage jobs |

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Unit (A1, A2) | NormalizedVector zero/NaN → Err; lock poisoning propagation | Existing tests + new assertion tests |
| Unit (A3) | Serialization error variant correctness | Verify JsonError vs SerializationError on matching |
| Unit (A5) | Embedding timeout, retry exhaustion | Mock HTTP server |
| Unit (A9) | TestEmbeddingService determinism + normalization | SHA-256 property tests |
| Unit (A11) | HNSW concurrent reads + write | Thread stress test |
| Integration (A4) | SIGTERM → WAL flush + HotBuffer drain | Server process signal + log verify |
| Integration (A6, A7) | Auth rejection, rate limit 429 | HTTP client tests |
| Integration (A8) | Health readiness failure modes | Test with missing index → 503 |
| E2E (A9) | INSERT + FIND SIMILARITY with TestEmbeddingService | Full HTTP pipeline |

## Threat Matrix

N/A — no routing, shell, subprocess, VCS/PR automation, executable-file classification, or process-integration boundary.

## Migration / Rollout

No migration required. Each PR independently revertable. Auth defaults to `"none"` — existing deployments work unchanged. Feature flag `legacy-locking` reverts A11.

## Open Questions

- [ ] Windows SIGTERM equivalent: use `tokio::signal::windows::ctrl_break()` for graceful shutdown on Windows?
- [ ] `tesseract-cluster/src/replication.rs`: what specific dead code exists? Needs audit during PR4.