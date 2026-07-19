# Phase 5 — Go-to-Market: Exploration

> **Change**: `fase-5-go-to-market`
> **Project**: VQL (Tesseract)
> **Date**: 2026-07-18
> **State**: Exploration complete

---

## Current State

Tesseract is a **complete semantic-relational database engine** with:

- **7 crates** in a Cargo workspace: `tesseract-common`, `tesseract-core`, `tesseract-storage`, `tesseract-index`, `tesseract-vql`, `tesseract-api`, `tesseract-cluster`
- **~344 test functions** across all crates (unit + integration)
- **AGPL-3.0 license** (SPDX headers on every file)
- **Edition 2024**, Rust 1.85, stable toolchain
- **CI pipeline**: `cargo check`, `cargo clippy`, `cargo fmt`, `cargo test` (ubuntu/windows/macos), `cargo deny` for licensing — all in `.github/workflows/ci.yml`
- **cargo-deny** configured (`deny.toml`) allowing: AGPL-3.0, MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-DFS-2016, CC0-1.0
- **22 specs** in `openspec/specs/` covering all major subsystems

**What exists for users today:**
- An Axum HTTP server (`tesseract-api/src/main.rs`) with `POST /query`, `POST /insert`, `GET /health`
- VQL parser, planner, and executor with full pipeline (parse → plan → embed → search → score)
- HNSW ANN index with weighted distance, persistent serialization
- Tiered storage (hot RAM + cold disk) with WAL, page cache, tier lifecycle
- Distributed mode with JumpHash sharding, etcd discovery, leader election, failover, replication
- OpenAI embedding service integration (`openai-embedding` feature)

**What is MISSING for public release:**
- **No README.md** — zero documentation for new users
- **No CONTRIBUTING.md** — no contribution guide
- **No quickstart or examples** — no `examples/` directory
- **No benchmark results** published
- **No Dockerfile** for easy deployment
- **No changelog** or release notes
- **CI runs but has no badge, no release workflow, no publishing pipeline**
- **No PostgreSQL integration** of any kind

---

## Affected Areas

| Path | Role in go-to-market |
|------|---------------------|
| `Cargo.toml` | Workspace root — may add new members (PG plugin crate) |
| `deny.toml` | May need updates for new dependencies |
| `.github/workflows/ci.yml` | CI — needs release workflow, publishing |
| `tesseract-api/src/http.rs` | Existing HTTP API that PG plugin would call |
| `tesseract-api/Cargo.toml` | Current deps (axum, tower, etc.) |
| `tesseract-api/src/main.rs` | Server binary — Docker entry point |
| `tesseract-storage/src/engine.rs` | StorageEngine API — PG plugin entry point |
| `tesseract-storage/src/types.rs` | StorageConfig, WriteMode, etc. |
| `tesseract-core/src/types.rs` | VectorId, MetadataValue — shared types |
| `tesseract-core/src/embedding.rs` | EmbeddingService trait |
| `tesseract-vql/src/executor.rs` | QueryExecutor — end-to-end pipeline |
| `tesseract-cluster/Cargo.toml` | etcd feature gate — reference for PG plugin features |
| (new) `tesseract-pg/` | Possible new crate for PostgreSQL plugin |
| (new) `README.md` | Root-level project documentation |
| (new) `CONTRIBUTING.md` | Contribution guide |
| (new) `Dockerfile` | Container image for easy deployment |
| (new) `.github/workflows/release.yml` | Release/publishing pipeline |
| (new) `examples/` | Quickstart examples |

---

## Approaches

### A. PostgreSQL Integration

#### A1. pgvector-style Extension (Custom Type + Index Access Method)

**Description**: Build a native PostgreSQL extension using `pgrx` that registers a custom `vector` type and an index access method backed by Tesseract's HNSW index. Vectors are stored as PostgreSQL tuples; the index access method calls into Tesseract's Rust code via FFI or embedded process.

**Pros:**
- Deep integration — PG handles storage, transactions, replication; Tesseract handles ANN search
- Transparent to existing PG tooling (pgAdmin, pg_dump, etc.)
- Users write standard SQL with custom operators (`<->` for distance)
- `pgrx` is mature and well-documented for Rust extensions
- Best PostgreSQL user experience — no separate server to manage

**Cons:**
- Requires C FFI glue or `pgrx` Rust integration (pgrx compiles to a shared library loaded by PG)
- PG extension API for index access methods is complex (need `amhandler`, `ambuild`, `ambeginscan`, etc.)
- Tesseract's storage engine is duplicated or must be embedded — weight computation, metadata projection, WAL, tier management live inside PG memory
- `pgrx` tightly couples to specific PG versions
- Cannot use Tesseract's existing HTTP API, cluster mode, or VQL parser
- Massive engineering effort — estimated **1,500-2,500 lines** for a minimal extension

**Effort**: Very High (months of work for production-quality extension)

#### A2. Foreign Data Wrapper (FDW) using pgrx

**Description**: Create a foreign data wrapper that maps Tesseract collections to PG foreign tables. Queries push down vector search to Tesseract via its HTTP API, returning results as PG tuples.

**Pros:**
- Tesseract runs as its own process — no embedding PG memory
- Uses existing HTTP API — no new API surface needed
- FDW API is simpler than index access methods
- PG handles SQL parsing; Tesseract handles vector search
- Works with Tesseract's cluster mode out of the box

**Cons:**
- Two-process architecture: PG + Tesseract (more operational complexity)
- Query push-down is limited — must forward VQL semantics over FDW
- FDW performance is worse than native index (row-by-row tuple mapping)
- No PG-native transaction integration
- `pgrx` FDW support exists but is less mature than index extension path

**Effort**: Medium (400-600 lines for a basic FDW)

#### A3. Sidecar Approach (Separate Process, HTTP Client)

**Description**: Keep Tesseract as a standalone server. Users connect via HTTP from any language. Provide a thin PG-side function (`tesseract_query(query_text)`) defined via `pgrx` that wraps the HTTP call. No FDW, no custom index — just PG functions that proxy to Tesseract.

**Pros:**
- Zero changes to Tesseract internals
- Works today — `POST /query` and `POST /insert` are already implemented
- Client libraries for any language (not just PG)
- Simplest possible integration path
- PG function is ~100 lines of Rust

**Cons:**
- Not transparent — users explicitly call `SELECT * FROM tesseract_query(...)`
- No push-down optimization — full query result must be fetched
- Two servers to manage
- Not a "real" PG extension — doesn't integrate with PG's query planner

**Effort**: Low (100-200 lines for PG functions, 0 lines on Tesseract side)

#### A4. gRPC API (Add gRPC to Tesseract alongside HTTP)

**Description**: Add a gRPC service definition (protobuf) and server to `tesseract-api` using `tonic`. The PG side connects via a C/Rust gRPC client. This replaces the HTTP layer with a more structured, typed API contract.

**Pros:**
- Strongly typed API contract via protobuf
- Streaming support for large result sets
- Better performance than HTTP/JSON (binary encoding)
- Industry standard for service-to-service communication
- More ecosystem tooling (protoc, grpcurl)

**Cons:**
- gRPC adds a new dependency (`tonic`, `prost`, protobuf compiler)
- Existing HTTP API must be maintained alongside gRPC
- Requires protobuf definitions and code generation
- Overhead of managing two API protocols
- PG gRPC client in C is more complex than HTTP

**Effort**: Medium (300-500 lines for gRPC service + protobuf definitions)

#### A5. Managed Sidecar (pg_tesseract via pgrx background worker)

**Description**: Use `pgrx` to create a background worker that embeds a `reqwest`-based HTTP client. The worker exposes SQL-callable functions (`tesseract_search(...)`, `tesseract_insert(...)`) that communicate with a Tesseract server.

**Pros:**
- Similar to A3 but with proper PG background worker lifecycle
- Connection pooling to Tesseract built-in
- Can register as a custom scan node for better push-down (future)
- `pgrx` provides background worker API

**Cons:**
- Still two-process architecture
- Background worker adds PG complexity
- Still requires explicit function calls, not transparent index

**Effort**: Low-Medium (200-400 lines)

---

### B. Open Source Release Readiness

#### B1. Minimal Release (MVP)

**Description**: Add only what is strictly necessary for a public release:
- `README.md` with project description, architecture overview, build instructions, and quickstart
- `examples/` directory with 2-3 VQL query examples
- `Dockerfile` for `tesseract-server`
- CI badge and release workflow (cargo publish for each crate)
- Dependency audit pass (update `deny.toml` if needed)

**Pros:**
- Fast to ship (1-2 days of work)
- Low effort
- Catches the most critical gaps for first-time users

**Cons:**
- No contribution guidance
- No benchmark results
- No changelog
- Projects without CONTRIBUTING.md get fewer community contributions

**Effort**: Low (~500 lines across README, examples, Dockerfile, CI)

#### B2. Complete Release (Recommended)

**Description**: Everything in B1 plus:
- `CONTRIBUTING.md` with coding standards, PR process, testing guidelines
- `CHANGELOG.md` (auto-generated or manual)
- `SECURITY.md` for vulnerability reporting
- Comprehensive documentation (docs/ folder or readthedocs)
- Benchmarks published (README badges for performance)
- `cargo publish` automation in CI
- Release workflow (GitHub Releases with binaries)
- Code of Conduct (CONTRIBUTING.md references)

**Pros:**
- Ready for community adoption
- Lowers barrier for contributors
- Shows project maturity
- Security policy builds trust

**Cons:**
- More effort (3-5 days)
- Documentation maintenance burden

**Effort**: Medium (~2,000 lines across docs, CI, workflows)

---

### C. Tesseract Cloud (Managed Service)

#### C1. Skip for Now

**Description**: Defer Tesseract Cloud to a later phase. Focus on making the open-source project self-serve and the PG integration functional.

**Pros:**
- Keeps Phase 5 focused
- Avoids operational overhead of running a cloud service
- Community feedback on OSS will inform cloud requirements

**Cons:**
- Misses early adopter revenue opportunities
- No managed offering for users who don't want to self-host

**Effort**: None

#### C2. Minimal Cloud Scaffolding

**Description**: Add a Helm chart for Kubernetes deployment (tesseract-server as a StatefulSet) and a simple Terraform module for single-node deployment. Document the deployment architecture.

**Pros:**
- Makes self-hosting much easier
- Natural stepping stone to a managed service
- Low effort

**Cons:**
- Not a managed service — users still operate it
- Still requires operational knowledge

**Effort**: Low (~300 lines of Helm/Terraform + docs)

---

## Recommendation

### PostgreSQL Integration: A3 (Sidecar with HTTP Client) + A2 (FDW as stretch goal)

**Why**: A3 requires **zero changes** to Tesseract's existing codebase. The HTTP API at `POST /query` and `POST /insert` is already production-ready. Users can integrate today from any language. For PG specifically, a thin `pgrx` function `tesseract_search()` wrapping HTTP calls is ~100 lines and can ship in the same release as the open source launch.

A2 (FDW) is the right long-term direction for deep PG integration — it provides transparent table-backed vector search — but it should be development Phase 6, not Phase 5.

### Open Source Release: B2 (Complete)

The project has 22 specs, 7 crates, and 344 tests. This is not a weekend hack — it's a production-quality database engine. The release materials should reflect that quality. Specifically:

1. **README.md** — architecture diagram, build from source, quickstart with Docker
2. **CONTRIBUTING.md** — how to build, test, submit PRs
3. **Dockerfile** — multi-stage build for `tesseract-server`
4. **Release CI** — `cargo publish` for all 7 crates (in dependency order), GitHub Releases with cross-compiled binaries
5. **examples/** — 3 examples: basic insert+query, text embedding with OpenAI, cluster mode
6. **CHANGELOG.md** — autogenerated from conventional commits

### Tesseract Cloud: C1 (Skip)

Focus the release on the open source project. Cloud can be evaluated after community traction.

---

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| **`pgrx` version coupling** — pgrx only supports specific PG versions (15, 16, 17). When PG releases a new major version, pgrx takes months to catch up | High | For the sidecar approach (A3), this is irrelevant — the PG function is thin. Only affects FDW (A2) path |
| **CI publishing** — `cargo publish` for 7 interdependent crates must be done in dependency order; one failure blocks all | Medium | Use `cargo release` or publish script with ordered workspace members |
| **Cross-compilation for releases** — Rust cross-compilation (especially for Windows ARM and macOS aarch64) requires toolchain setup | Medium | Start with Linux x86_64 + macOS aarch64; add Windows x86_64 in follow-up |
| **No embedding service out of the box** — The default `NoopEmbeddingService` means users must bring their own embedding provider or use raw vectors | Medium | Document this clearly in README; show OpenAI integration example; consider bundling a lightweight local embedding model |
| **AGPL-3.0 license may deter adoption** — Some companies and projects avoid AGPL due to perceived network-restriction clauses | Low | Accept this as a conscious choice (consistency with other vector DBs like Qdrant, Weaviate) |
| **No benchmark numbers** — Users can't evaluate performance without running their own benchmarks | Medium | Add benchmark results to README; include `cargo bench` harness output |
| **Sidecar approach is not "real" integration** — Some users expect a native PG extension like pgvector | Low | Document the tradeoff; FDW (A2) is the path forward for deeper integration |

---

## Ready for Proposal

**Yes.** This exploration covers all major dimensions of go-to-market for Tesseract:

1. **PostgreSQL integration** — clear recommendation for sidecar (A3), with FDW (A2) identified as the next evolution
2. **Open source release** — concrete list of what's missing and what to build
3. **Cloud** — explicit decision to defer

The proposal phase should:
- Define the scope boundary (what goes into Phase 5 vs deferred)
- Choose the delivery slice order (README first, then Docker, then CI, then PG plugin)
- Produce a rollback plan for the PG plugin (A3 is trivially revertible)
- Define the release milestone (v0.1.0 or v0.2.0?)
