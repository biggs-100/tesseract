# Apply Progress: production-readiness PR3

> Status: ✅ All 2 issues complete (A9, A10).

---

## Summary

| Metric | Value |
|--------|-------|
| **PR** | PR3 — Quality |
| **Target** | `main` |
| **Mode** | Standard |
| **Issues total** | 2 (A9, A10) |
| **Completed** | 2 |
| **Blocked** | 0 |
| **Tests** | All 495+ pass (80 unit + 6 E2E feature-gated + existing 400+) |
| **Clippy** | Clean (pre-existing warning only: `skeleton` field in `tesseract-storage`) |

---

## Commit Log

| Commit | Message | Files |
|--------|---------|-------|
| `7b3cf29` | feat(core): add TestEmbeddingService with deterministic SHA-256 embeddings and E2E tests | A9 |
| `bc6ebd1` | ci: add cargo-deny advisories and cargo-llvm-cov coverage jobs | A10 |

---

## Issue Status

### A9 — TestEmbeddingService ✅

| File | Action | What Was Done |
|------|--------|---------------|
| `tesseract-core/src/test_embedding.rs` | **Created** | `TestEmbeddingService` implementing `EmbeddingService` trait; SHA-256 → f64 vector → L2 normalize; dim configurable (default 128, clamped to 32); 7 unit tests |
| `tesseract-core/tests/e2e_test_embedding.rs` | **Created** | 6 E2E integration tests (determinism, normalization, cosine similarity, dimension config, empty input); gated behind `#![cfg(feature = "test-embedding")]` |
| `tesseract-core/Cargo.toml` | Modified | Added `sha2` as optional dep, `test-embedding = ["dep:sha2"]` feature |
| `tesseract-core/src/lib.rs` | Modified | Added `#[cfg(feature = "test-embedding")] pub mod test_embedding;` |
| `Cargo.lock` | Modified | sha2 + transitive deps locked |

**Feature**: `test-embedding` — run with `cargo test -p tesseract-core --features test-embedding`

### A10 — CI Hardening ✅

| File | Action | What Was Done |
|------|--------|---------------|
| `.github/workflows/ci.yml` | Modified | Updated `audit` job: added advisory-db cache, `cargo deny check advisories` + `check licenses`; added `e2e` job for feature-gated tests; added `coverage` job with `cargo llvm-cov --html` + 70% threshold warning + artifact upload |
| `deny.toml` | Modified | Added `[advisories]` section with `vulnerability = "deny"`, `unmaintained = "warn"`, `yanked = "warn"` |

**Config**: Advisory DB auto-cached via `hashFiles('**/Cargo.lock')` key

---

## Deviations from Design

| ADR | Deviation | Rationale |
|-----|-----------|-----------|
| ADR-009 (TestEmbeddingService) | E2E tests in `tesseract-core/tests/` cannot perform INSERT + FIND SIMILARITY via `StorageEngine` | `tesseract-core` does not depend on `tesseract-storage`. Full pipeline E2E belongs in `tesseract-storage/tests/` or `tesseract-api/tests/`. The tests verify embedding determinism, normalization, and cosine similarity at the embedding service level. |
| ADR-010 (CI) | Use `actions-rust-lang/setup-rust-toolchain@v1` instead of `dtolnay/rust-toolchain` | Consistent with existing CI pattern in the project. |

## Issues Found

None.

## Work Unit Evidence

| Evidence | Value |
|----------|-------|
| Focused test command | `cargo test -p tesseract-core --features test-embedding` — 86 tests passed (80 unit + 6 E2E) |
| Workspace test command | `cargo test --workspace --exclude tesseract-pg` — all 490+ tests pass |
| Runtime harness | N/A — no external runtime boundary in this PR |
| Rollback boundary | Revert 2 commits (A9: `tesseract-core/src/test_embedding.rs`, `tesseract-core/tests/`, `tesseract-core/Cargo.toml`, `tesseract-core/src/lib.rs`, `Cargo.lock`; A10: `.github/workflows/ci.yml`, `deny.toml`) |

## Workload / PR Boundary

- **Mode**: Stacked PR (PR3 of 4) → `main`
- **Changed lines**: ~357 (within 800-line budget)
- **Commits**: 2 clean commits (A9, A10)
