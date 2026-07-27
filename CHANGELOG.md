# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.2.0] — 2026-07-27

### Added

- **Authentication** — API key and JWT (HS256) support via `TESSERACT_AUTH_MODE`.
  Three modes: `none`, `api-key`, `jwt`, `both`. Configurable per environment.
- **Rate Limiting** — Per-IP sliding window rate limiter (100 RPM default,
  configurable via `TESSERACT_RATE_LIMIT_RPM`). Returns HTTP 429 with `Retry-After`.
- **Health Endpoints** — `GET /health/liveness` (alive check) and
  `GET /health/readiness` (component-level: WAL, index, HotBuffer).
- **Prometheus Metrics** — OpenTelemetry integration with `--features otel`.
  Exposes `queries_total`, `query_duration_seconds`, `inserts_total`,
  `index_size`, `hotbuffer_size` on `GET /metrics`.
- **Graceful Shutdown** — SIGTERM/SIGINT handler drains HotBuffer, flushes WAL,
  and persists index before exit. Configurable timeout via
  `TESSERACT_SHUTDOWN_TIMEOUT_SECS`.
- **Embedding Resilience** — Configurable timeout (`TESSERACT_EMBEDDING_TIMEOUT_SECS`)
  and retry with exponential backoff for OpenAI-compatible embedding services.
- **TestEmbeddingService** — Deterministic SHA-256-based embedding service for tests
  (`--features test-embedding`). Enables end-to-end testing without external APIs.
- **CI/CD** — `cargo deny check advisories` for vulnerability scanning.
  `cargo llvm-cov` for code coverage reporting (70% threshold).
- **Structured Logging** — JSON log format via `TESSERACT_LOG_FORMAT=json`.
  `#[instrument]` spans on query pipeline (parse, plan, execute).

### Changed

- **Panics → Results** — `NormalizedVector::new()`, `PageCache::new()`, and
  `register_field()` now return `Result` instead of panicking on invalid input.
  Zero vectors, NaN, and empty boundaries are handled gracefully.
- **Lock Safety** — All 46+ `lock().unwrap()` / `lock().expect()` sites replaced
  with `map_err` propagation. Lock poisoning no longer causes silent data loss.
- **Concurrent Index Access** — HNSW index uses `parking_lot::RwLock` for 2-5x
  faster concurrent reads. StorageEngine uses `tokio::sync::RwLock` allowing
  parallel searches. Feature flag `legacy-locking` available for revert.
- **Error Enum** — `BincodeError` renamed to `SerializationError`. Added
  `JsonError`, `LockPoisoned`, `InvalidVector`, `InvalidConfig` variants.
- **WAL Error Reporting** — JSON deserialization errors now correctly report as
  `JsonError` instead of `BincodeError`.

### Fixed

- Zero-vector and NaN inputs no longer crash the process.
- Episodic memory lock poisoning no longer silently returns `None`.
- WAL payload errors now use the correct error variant.

### Security

- Embedding HTTP client includes timeout (default 30s) to prevent hanging requests.
- Retry with exponential backoff for embedding API rate limits and server errors.
- All lock acquisitions in production code handle poisoning without data loss.

### Performance

- HNSW index: concurrent reads no longer block each other under `parking_lot::RwLock`.
- StorageEngine: search operations use `read()` instead of `lock()`, enabling
  parallel searches.
- 2 new concurrency stress tests (10 readers + 1 writer, no deadlock).

### Tests

- 511+ tests (up from 345+).
- 13 new auth tests (ApiKey, JWT, MultiAuth).
- 4 new rate limiter unit tests.
- 7 TestEmbeddingService unit tests + 6 E2E tests.
- 3 graceful shutdown integration tests.
- 2 concurrency stress tests for HNSW.

---

## [0.1.0] — 2026-07-18

### Added

- **VQL Engine** — Full query planner and executor supporting:
  - `FIND SIMILARITY` — nearest-neighbour vector search with optional metadata filters.
  - `INSERT` — insert vectors with associated metadata.
  - `LIMIT` / `OFFSET` — pagination over result sets.
  - Hybrid search combining vector similarity with structured predicates.
- **HTTP API** — Axum-based REST server:
  - `POST /query` — execute VQL queries, return scored results.
  - `POST /insert` — insert vectors with metadata.
  - `GET /health` — liveness probe.
- **Storage Engine** — Tiered vector storage with:
  - Hot store (in-memory, high throughput).
  - Cold store (on-disk, mmap-backed).
  - WAL (write-ahead log for durability).
  - Page cache for optimised I/O.
  - Skeleton-based vector lifecycle (promote/demote between tiers).
- **HNSW Index** — Approximate nearest-neighbour search with configurable
  parameters (M, efConstruction, efSearch).
- **Embedding Service** — Pluggable interface with a `NoopEmbeddingService`
  default (identity pass-through). Supports external embedding providers.
- **Episodic Memory** — Query history tracking and pattern detection.
- **Cluster Support** — Shard management, replication engine, cluster
  coordination protocol, distributed query execution.
- **PostgreSQL Extension** (`tesseract_fdw`) — pgrx-based sidecar extension:
  - `tesseract_connect(host, port)` — configure session endpoint.
  - `tesseract_query(vql)` — SRF returning `TABLE(id, score, metadata)`.
  - `tesseract_insert(id, vector, metadata)` — insert via SQL.
- **Docker Support** — Multi-stage build + Docker Compose for local
  development and evaluation.
- **CI/CD** — GitHub Actions for linting, testing (Linux/macOS/Windows),
  dependency audit, and release automation.
- **511+ Tests** — Unit, integration, and PG extension tests.

### Changed

- N/A — initial release.

### Deprecated

- N/A — initial release.

### Removed

- N/A — initial release.

### Fixed

- N/A — initial release.

### Security

- No CVEs at release time.
- All dependencies audited via `cargo deny`.
- SPDX license headers on all source files (AGPL-3.0-only).

---

## [Unreleased]

### Planned

- Foreign Data Wrapper (FDW) for native PostgreSQL table integration.
- Native PG index access method (pgvector-style).
- Tesseract Cloud / managed service.
- Benchmark suite and published results.
- Streaming replication for high availability.

[0.1.0]: https://github.com/tesseract-db/tesseract/releases/tag/v0.1.0
