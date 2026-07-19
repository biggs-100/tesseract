# Design: Fase 5 — Go-to-Market (PG Extension)

## Technical Approach

New `tesseract-pg/` workspace crate using `pgrx` (v0.13+) that registers SQL-callable functions proxying to Tesseract's HTTP API (`POST /query`, `POST /insert`, `GET /health`). Zero changes to existing crates. The extension runs as a thin sidecar — every PG query translates to an HTTP round-trip to a colocated Tesseract process.

## Architecture Decisions

| # | Decision | Choice | Alternatives | Rationale |
|---|----------|--------|-------------|-----------|
| 1 | PG interface pattern | Set-returning functions (SRF) | FDW (Phase 6), custom scan | SRFs are ~100 lines vs 300+ for FDW; deliver functional integration now, full FDW later |
| 2 | Connection management | Per-session GUC variables | Connection pool, per-query URL | pgrx runs per-backend; Tesseract is colocated (localhost); pooling adds complexity for zero gain |
| 3 | Vector type mapping | PG `REAL[]` → wire `Vec<f64>` | Custom PG type, JSONB blob | Existing API accepts `Vec<f64>`; serde transparently casts `f32[]` → `f64` on send. Spec-compliant round-trip |
| 4 | Metadata mapping | PG `JSONB` → `serde_json::Value` | Custom type per MetadataValue enum | JSONB is already serde_json::Value-compatible; no custom PG type needed |
| 5 | Error propagation | `pgrx::error!()` with `ERRCODE_*` | Return NULL, log only | Users need immediate feedback when Tesseract is down; PG ERROR is the clearest signal |
| 6 | Project structure | Workspace member `tesseract-pg/` | Standalone repo, submodule | Unified CI, single `cargo test`, consistent versioning; pgrx build constraints are manageable |

## Data Flow

```
PG Backend ──→ tesseract_connect(host, port) ──→ session GUCs (host:port)
     │
     ├── tesseract_query(vql)
     │      └── reqwest POST /query ──→ Tesseract ──→ Vec<ScoredResult> ──→ PG TABLE(id, score, metadata)
     │
     └── tesseract_insert(id, vector, metadata)
            └── reqwest POST /insert ──→ Tesseract ──→ InsertResponse ──→ PG BIGINT
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `Cargo.toml` | Modify | Add `"tesseract-pg"` to workspace members |
| `deny.toml` | Modify | Add pgrx and reqwest license allowlist entries |
| `tesseract-pg/Cargo.toml` | Create | pgrx extension manifest (pgrx, reqwest, serde, serde_json, tokio) |
| `tesseract-pg/src/lib.rs` | Create | Extension entry: `#[pg_extern]` functions + `pg_module_magic!()` |
| `tesseract-pg/src/client.rs` | Create | HTTP client: `QueryArgs`, `InsertArgs`, response deserialization |
| `tesseract-pg/src/config.rs` | Create | Session-level GUCs: `tesseract.host`, `tesseract.port` |

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Unit | Client deserialization, config parsing | `#[cfg(test)]` — no PG needed, mock HTTP with `wiremock` |
| Integration | Full extension in PG | `cargo pgrx test` — spins PG 16, loads extension, runs SQL |
| E2E | Tesseract + PG extension | Docker Compose: `tesseract-server` + `postgres:16` with extension pre-installed |

## Threat Matrix

N/A — no routing, shell commands, subprocesses, VCS/PR automation, executable-file classification, or process-integration boundary. All communication is standard HTTP client-server; pgrx manages PG lifecycle internally.

## Migration / Rollout

No migration required. Extension is additive — `CREATE EXTENSION tesseract_fdw` activates it. Remove by `DROP EXTENSION tesseract_fdw` and deleting the `tesseract-pg` workspace member.

## Open Questions

- Should `tesseract_query` expose an optional `user_id` parameter for the existing API field?
- What is the minimum PG version to target for `cargo pgrx` (16 vs 17)?
- Should Docker Compose live at the repo root or under `examples/`?
