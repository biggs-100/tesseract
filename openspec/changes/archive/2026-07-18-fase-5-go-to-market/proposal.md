# Proposal: Fase 5 — Go-to-Market

## Intent

Tesseract is a production-ready semantic-relational DB (7 crates, 345 tests, AGPL v3) with zero user-facing documentation, no PostgreSQL integration, no container image, no release pipeline. This change prepares it for public adoption with minimum surface area for evaluation and integration.

## Scope

### In Scope
- Sidecar PostgreSQL extension (`tesseract-pg`) via pgrx — ~100 lines proxying to existing HTTP API
- Open source release: README, CONTRIBUTING, Dockerfile, examples (3), release CI, CHANGELOG
- Dependency audit for new deps

### Out of Scope
- FDW — Phase 6
- Native PG index access method (pgvector-style)
- Tesseract Cloud
- Benchmark results in README

## Capabilities

### New Capabilities
- `pg-postgres-extension`: PostgreSQL extension (pgrx) providing SQL-callable functions that proxy vector/semantic queries to Tesseract via existing HTTP API.

### Modified Capabilities
None. Sidecar consumes `http-api` spec as-is — zero spec-level changes.

## Approach

Two parallel workstreams:

1. **Sidecar PG Plugin** — New `tesseract-pg/` crate via pgrx. Expose `tesseract_query(vql text)` → table of (id, score, metadata) and `tesseract_insert(id bigint, vector float8[], metadata jsonb)` → bigint. Both proxy to Tesseract's `POST /query` and `POST /insert` via reqwest. Zero changes to existing crates.

2. **Open Source Release** — README.md (architecture + quickstart), CONTRIBUTING.md, multi-stage Dockerfile, 3 examples, release.yml CI, CHANGELOG.md.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `Cargo.toml` (workspace) | Modified | Add `tesseract-pg` member |
| `deny.toml` | Modified | Add pgrx deps audit |
| `.github/workflows/ci.yml` | Modified | Build `tesseract-pg` |
| `.github/workflows/release.yml` | New | Cargo publish + binary releases |
| `tesseract-pg/` | New | Sidecar PG extension crate |
| `README.md` | New | Project overview + quickstart |
| `CONTRIBUTING.md` | New | Contribution guide |
| `CHANGELOG.md` | New | Release notes |
| `Dockerfile` | New | Multi-stage server image |
| `examples/` | New | 3 usage examples |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| pgrx version-coupled to PG versions | Low (thin layer) | Test against PG 16/17 |
| CI publish order (7 crates) | Med | Use cargo-release with ordered members |
| AGPL-3.0 deters adoption | Low | Conscious choice, consistent with Qdrant |
| Sidecar is not "real" integration | Med | Document tradeoff; FDW planned |

## Rollback Plan

- **PG plugin**: Remove `tesseract-pg/` + workspace entry. Zero core impact.
- **Release files**: Revert all additive files — no functional impact.
- **CI**: Revert release.yml and ci.yml changes.

## Dependencies

- `pgrx` crate for PG extension development
- `reqwest` for HTTP proxying in PG plugin

## Success Criteria

- [ ] `cargo build` succeeds with `tesseract-pg` in workspace
- [ ] PG extension functions return correct results against live Tesseract
- [ ] `cargo test` passes for all crates (zero regressions)
- [ ] Dockerfile produces runnable `tesseract-server` image
- [ ] All 3 examples in `examples/` execute successfully
- [ ] CI release workflow publishes crates in dependency order
