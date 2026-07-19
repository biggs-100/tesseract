# Tasks: Fase 5 — Go-to-Market

## Review Workload Forecast

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: Medium

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | `tesseract-pg` crate skeleton + HTTP client + GUC config | PR 1 | `cargo test -p tesseract-pg --lib` | `cargo pgrx run` → `CREATE EXTENSION tesseract_fdw; SELECT tesseract_connect('http://localhost', 8080);` | Remove `tesseract-pg/` + workspace entry |
| 2 | Query/insert SRF functions + type mapping + error handling | PR 2 | `cargo pgrx test` | Docker Compose: start PG + Tesseract → `SELECT * FROM tesseract_query(...)` | Revert `lib.rs` SRF additions, keep client/config |
| 3 | Open source release (README, Docker, CI, examples) | PR 3 | `docker build .` | `docker compose up` then run `examples/` scripts | Revert all additive files — no functional impact |

## Phase 1: PG Extension Foundation (PR 1)

- [x] 1.1 Create `tesseract-pg/Cargo.toml` — pgrx, reqwest, serde, serde_json, tokio deps
- [x] 1.2 Create `tesseract-pg/src/config.rs` — GUC vars (`tesseract_host`, `tesseract_port`, `tesseract_timeout`) + `tesseract_connect()`
- [x] 1.3 Create `tesseract-pg/src/client.rs` — `QueryArgs`/`InsertArgs` types, `POST /query`/`/insert` response deserialization, wiremock unit tests
- [x] 1.4 Create `tesseract-pg/src/lib.rs` — module entry, `pg_module_magic!()`, `#[pg_extern]` exports
- [x] 1.5 Modify workspace `Cargo.toml` — add `"tesseract-pg"` member
- [x] 1.6 Modify `deny.toml` — add pgrx, reqwest license allowlist entries

## Phase 2: Query/Insert Functions (PR 2)

- [x] 2.1 Add `tesseract_query(vql text)` SRF returning `TABLE(id BIGINT, score REAL, metadata JSONB)` — proxies `POST /query`
- [x] 2.2 Add `tesseract_insert(id BIGINT, vector REAL[], metadata JSONB)` → `BIGINT` — proxies `POST /insert`
- [x] 2.3 Add error propagation — `pgrx::error!()` with `ERRCODE_CONNECTION_FAILURE`, `ERRCODE_DATA_EXCEPTION`
- [x] 2.4 Add integration tests — `cargo pgrx test` covering spec scenarios (connect, query, insert, errors, unconfigured)

## Phase 3: Open Source Release (PR 3)

- [x] 3.1 Create `README.md` — architecture overview + quickstart (PG + Tesseract)
- [x] 3.2 Create `CONTRIBUTING.md` — setup, build, test, PR workflow
- [x] 3.3 Create `Dockerfile` — multi-stage build for `tesseract-server` binary
- [x] 3.4 Create `docker-compose.yml` — `tesseract-server` + `postgres:16` with extension
- [x] 3.5 Create `.github/workflows/release.yml` — Cargo publish + binary release
- [x] 3.6 Modify `.github/workflows/ci.yml` — add `tesseract-pg` build + test step
- [x] 3.7 Create 3 examples under `examples/` — semantic search, insert, hybrid query
- [x] 3.8 Create `CHANGELOG.md` — initial release notes
