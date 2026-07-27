# Spec: production-readiness

> 12 issues across 4 PRs stacked to main. Each PR independently verifiable.

---

## PR1 — Core Correctness

Issues: A1 (panics) → A2 (lock poisoning) → A4 (graceful shutdown) → A3 (WAL serialization)

### Requirements

1. `NormalizedVector::new` MUST return `Result<Self>` instead of panicking on zero, NaN, or Inf vectors.
2. `NumericalBucketTracker::register_field` MUST return `Result<()>` when `boundaries` is empty.
3. `PageCache::new` MUST return `Result<Self>` when `capacity` is zero.
4. All `std::sync::Mutex` accesses in production code MUST propagate lock poisoning errors via `map_err` instead of `.unwrap()` or `.expect()`.
5. `StorageEngine::shutdown` MUST execute on SIGTERM/SIGINT before the process terminates.
6. The HotBuffer MUST drain pending entries before shutdown completes.
7. The WAL MUST flush before shutdown completes.
8. `StorageEngine::shutdown` MUST support a configurable timeout, defaulting to 30 seconds.
9. The error enum MUST expose `SerializationError` and `JsonError` variants instead of `BincodeError` for JSON WAL payloads.

### Scenarios

#### Scenario: NormalizedVector rejects zero and NaN vectors (A1)

- GIVEN a zero vector `[0.0, 0.0, 0.0]`
- WHEN `NormalizedVector::new` is called
- THEN the call returns `Err` with a descriptive error, not a panic
- AND the same applies for a NaN vector `[f64::NAN, 1.0]`

#### Scenario: NormalizedVector accepts valid vector (A1)

- GIVEN a non-zero finite vector `[1.0, 2.0, 3.0]`
- WHEN `NormalizedVector::new` is called
- THEN the call returns `Ok(normalized_vector)` with unit-length result

#### Scenario: PageCache rejects zero capacity (A1)

- GIVEN `PageCache::new(0)` is called
- WHEN the constructor executes
- THEN it returns `Err`, not a panic

#### Scenario: Lock poisoning propagates error (A2)

- GIVEN a poisoned `std::sync::Mutex` in `EpisodicMemory` or `StorageEngine`
- WHEN any public method tries to acquire the lock
- THEN the error propagates via `Result` instead of panicking or returning `None`

#### Scenario: SIGTERM triggers shutdown (A4)

- GIVEN a running Tesseract server
- WHEN the process receives SIGTERM
- THEN `StorageEngine::shutdown` is called before the process exits
- AND the HotBuffer and WAL finish flushing within the configured timeout

#### Scenario: Shutdown timeout enforced (A4)

- GIVEN a `StorageEngine` with `shutdown_timeout = 10s`
- WHEN shutdown operations exceed 10 seconds
- THEN a warning is logged and the process terminates without blocking indefinitely

#### Scenario: WAL serialization error consistency (A3)

- GIVEN a JSON-format WAL payload serialization failure
- WHEN `serde_json::to_vec` fails inside `StorageEngine`
- THEN the error variant is `Error::JsonError`, not `Error::BincodeError`

### Acceptance Criteria

- [ ] `cargo test --workspace` passes with no regressions
- [ ] `cargo clippy --all-targets` produces no new warnings
- [ ] No `assert!`/`expect!`/`unwrap()` in production code for lock access
- [ ] SIGTERM test verifies WAL flush and HotBuffer drain via log assertions
- [ ] No `BincodeError` variant is used for JSON serialization paths

---

## PR2 — Security/Ops

Issues: A5 (embedding timeout) → A6 (auth) → A7 (rate limits) → A8 (observability)

### Requirements

1. The OpenAI HTTP client MUST use a 30-second per-request timeout.
2. The embedding client MUST retry on HTTP 429 and 5xx with exponential backoff, up to 3 retries.
3. Timeout and retry parameters MUST be configurable via `OpenAIEmbeddingConfig`.
4. The HTTP API MUST authenticate requests via `X-API-Key` header.
5. The HTTP API MUST authenticate requests via JWT (`Authorization: Bearer`).
6. The gRPC API MUST authenticate via tonic interceptors supporting the same mechanisms.
7. Authentication MUST be configurable: disabled in development, required in production.
8. A no-auth mode MUST exist for local development (no API key or JWT required).
9. The HTTP API MUST enforce rate limiting per IP on public routes.
10. The default rate limit MUST be 100 requests/minute, fully configurable.
11. The HotBuffer MUST enforce a configurable maximum capacity.
12. Queries without a `WITHIN` clause MUST have an implicit timeout, defaulting to 30 seconds.
13. The HTTP API MUST expose `GET /health/liveness` returning a lightweight pass/fail.
14. The HTTP API MUST expose `GET /health/readiness` that checks WAL and index status.
15. The API MUST expose `GET /metrics` in Prometheus format.
16. The query pipeline MUST use `#[instrument]` for distributed tracing.
17. The system MUST export QPS, P50/P95/P99 latency, error rate, and cache hit/miss metrics.
18. All logs MUST be structured (JSON), not plain text.

### Scenarios

#### Scenario: Embedding client timeout (A5)

- GIVEN an `OpenAIEmbedding` configured with default settings
- WHEN the OpenAI endpoint does not respond within 30 seconds
- THEN the call returns `Err` with a timeout error

#### Scenario: Embedding retries on 429 (A5)

- GIVEN an `OpenAIEmbedding` with retry enabled (max 3)
- WHEN the API responds with HTTP 429 three times
- THEN the call returns `Err` after all retries are exhausted
- AND each retry uses exponential backoff

#### Scenario: No embedding timeout configured (A5)

- GIVEN an `OpenAIEmbedding` configured with `timeout = 60s`
- WHEN the API responds slowly
- THEN the client waits up to 60 seconds before timing out

#### Scenario: Authenticated request succeeds (A6)

- GIVEN an HTTP API with auth enabled
- WHEN a request includes a valid `X-API-Key` header
- THEN the server processes the request normally (HTTP 200/201)

#### Scenario: Unauthenticated request rejected (A6)

- GIVEN an HTTP API with auth enabled
- WHEN a request has no `X-API-Key` or invalid JWT
- THEN the server responds with HTTP 401
- AND the body indicates missing or invalid credentials

#### Scenario: No-auth mode for development (A6)

- GIVEN an HTTP API with `auth.mode = "none"`
- WHEN any request is sent without credentials
- THEN the request is processed normally (no auth error)

#### Scenario: Rate limit exceeded (A7)

- GIVEN an HTTP API with rate limit set to 10 req/min
- WHEN a client sends 11 requests in one minute
- THEN the 11th request returns HTTP 429
- AND the response includes a `Retry-After` header

#### Scenario: Query without WITHIN times out (A7)

- GIVEN the system with default implicit query timeout of 30 seconds
- WHEN a `FIND SIMILARITY` query without `WITHIN` takes longer than 30 seconds
- THEN the query is aborted and returns a timeout error

#### Scenario: Liveness check (A8)

- GIVEN a running HTTP server
- WHEN `GET /health/liveness` is called
- THEN the server responds with HTTP 200 and `{ "status": "pass" }`

#### Scenario: Readiness check verifies dependencies (A8)

- GIVEN a running HTTP server with a functional WAL and index
- WHEN `GET /health/readiness` is called
- THEN the server responds with HTTP 200 and diagnostics confirming WAL is operational

#### Scenario: Readiness fails on missing index (A8)

- GIVEN a running HTTP server where the index is not loaded
- WHEN `GET /health/readiness` is called
- THEN the server responds with HTTP 503
- AND the body indicates the index is unavailable

#### Scenario: Prometheus metrics exported (A8)

- GIVEN a running HTTP server
- WHEN `GET /metrics` is called
- THEN the response is in Prometheus text format
- AND includes counters for QPS, histograms for P50/P95/P99 latency, and error rate

#### Scenario: Structured JSON logging (A8)

- GIVEN the system is configured with JSON log format
- WHEN a query is executed
- THEN each log line is valid JSON with `timestamp`, `level`, `message`, and span context

#### Scenario: gRPC auth interceptor rejects unauthenticated (A6)

- GIVEN a gRPC server with auth enabled
- WHEN a client sends a request without credentials
- THEN the server returns `UNAUTHENTICATED` gRPC status

### Acceptance Criteria

- [ ] `GET /health/liveness` and `GET /health/readiness` respond correctly
- [ ] `GET /metrics` exports Prometheus-format output
- [ ] Auth middleware rejects requests without valid `X-API-Key` or JWT
- [ ] No-auth mode works without credentials
- [ ] Requester behind rate limit receives HTTP 429
- [ ] Embedding client times out after the configured duration
- [ ] All logs are structured JSON when configured
- [ ] gRPC auth interceptor returns `UNAUTHENTICATED` for unauthenticated calls

---

## PR3 — Quality

Issues: A9 (test embedding) → A10 (CI audit)

### Requirements

1. A `TestEmbeddingService` MUST implement the `EmbeddingService` trait providing deterministic embeddings.
2. The embedding MUST be deterministic: SHA-256 of input text normalized to a unit vector.
3. The default embedding dimension MUST be 128, configurable via constructor.
4. At least one end-to-end test MUST execute `INSERT` + `FIND SIMILARITY` and verify scored results.
5. All end-to-end tests MUST pass in CI.
6. The CI pipeline MUST run `cargo deny check advisories` (or `cargo audit`) on every push and PR.
7. The CI pipeline MUST run `cargo llvm-cov` and generate a coverage report.
8. The initial coverage threshold MUST be 70%, reported as a warning (not a build blocker).

### Scenarios

#### Scenario: TestEmbeddingService returns deterministic vector (A9)

- GIVEN a `TestEmbeddingService` with dimension 128
- WHEN `embed("hello world")` is called twice
- THEN both calls return identical vectors
- AND the vector has L2 norm approximately 1.0

#### Scenario: TestEmbeddingService different texts differ (A9)

- GIVEN a `TestEmbeddingService`
- WHEN `embed("cat")` and `embed("dog")` are called
- THEN the two resulting vectors differ (different SHA-256 inputs)

#### Scenario: End-to-end INSERT + FIND SIMILARITY (A9)

- GIVEN a storage engine configured with `TestEmbeddingService`
- WHEN a client inserts `"quantum computing"` and executes `FIND SIMILARITY(emb, 'quantum computing') LIMIT 5`
- THEN the result set is non-empty
- AND the top result has a positive similarity score
- AND the inserted ID appears in the results

#### Scenario: CI blocks on known advisories (A10)

- GIVEN a dependency with a known CVE in `cargo deny` advisory database
- WHEN the CI pipeline runs
- THEN the `audit` job fails
- AND the error identifies the vulnerable dependency

#### Scenario: CI reports coverage (A10)

- GIVEN the CI pipeline
- WHEN `cargo llvm-cov` runs
- THEN a coverage report is generated as a CI artifact
- AND if coverage is below 70%, a warning is emitted (build does not fail)

### Acceptance Criteria

- [x] `TestEmbeddingService` passes unit tests for determinism and normalization
- [x] End-to-end test `INSERT` + `FIND SIMILARITY` with `TestEmbeddingService` passes
- [x] CI has an `audit` job running `cargo deny check advisories`
- [x] CI has a `coverage` job running `cargo llvm-cov`
- [x] Coverage warning fires below 70% without blocking the build

---

## PR4 — Performance

Issues: A11 (HNSW locking) → A12 (dead code)

### Requirements

1. HNSW index MUST use `parking_lot::RwLock` instead of `std::sync::RwLock<()>`.
2. `StorageEngine` MUST use `tokio::sync::RwLock<AnyIndex>` instead of `tokio::sync::Mutex<AnyIndex>`.
3. A feature flag `legacy-locking` MUST exist to revert to the previous locking strategy.
4. Concurrency tests MUST pass with the new locking under concurrent read/write load.
5. No `#[allow(dead_code)]` or `#[expect(dead_code)]` MUST remain in production code.
6. Unused fields in `StorageEngine`, `HotStore`, and `ReplicationEngine` MUST be removed.
7. Unused structs or modules MUST be removed or explicitly marked as deprecated.

### Scenarios

#### Scenario: HNSW concurrent reads with write (A11)

- GIVEN an HNSW index using `parking_lot::RwLock`
- WHEN 10 concurrent search tasks and 1 insert task run simultaneously
- THEN all search tasks complete without blocking each other
- AND the insert completes without deadlock

#### Scenario: StorageEngine allows concurrent searches (A11)

- GIVEN a `StorageEngine` using `tokio::sync::RwLock<AnyIndex>`
- WHEN 10 concurrent query tasks execute
- THEN all tasks run concurrently without serializing at the engine level

#### Scenario: Legacy locking mode compiles (A11)

- GIVEN the crate built with `--features legacy-locking`
- WHEN `cargo build --features legacy-locking` runs
- THEN it compiles successfully
- AND uses the previous `std::sync::RwLock<()>` and `tokio::sync::Mutex<AnyIndex>`

#### Scenario: Dead code lint passes (A12)

- GIVEN the workspace
- WHEN `cargo clippy --all-targets` runs
- THEN no `#[allow(dead_code)]` or `#[expect(dead_code)]` attributes exist in production code
- AND all unused items are either removed or properly deprecated

### Acceptance Criteria

- [ ] `cargo test --workspace` passes with the new locking
- [ ] Concurrency test with concurrent searches + inserts passes
- [ ] `cargo build --features legacy-locking` compiles
- [ ] `cargo clippy --all-targets` produces no `dead_code` warnings without suppression attributes
- [ ] All `#[allow(dead_code)]` and `#[expect(dead_code)]` removed from production code
