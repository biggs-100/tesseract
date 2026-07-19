# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- **345+ Tests** — Unit, integration, and PG extension tests.

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
