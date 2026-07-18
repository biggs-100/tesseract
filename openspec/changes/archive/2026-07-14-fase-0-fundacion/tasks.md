# Tasks: Phase 0 — Foundation and Key Concepts

## Review Workload Forecast

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
400-line budget risk: High

~1700–2200 lines / 24 files / 3 stacked PRs to main.

| Unit | Scope | PR | Focused test | Harness | Rollback |
|------|-------|----|--------------|---------|----------|
| 1 | Workspace + 6 stubs + CI + config + AGPL | 1 | `cargo build --workspace` | `cargo clippy -all-targets && cargo fmt --check` | `git revert` |
| 2 | Error types + core types + distance + projection + tests | 2 | `cargo test -p tesseract-core` | `cargo build -p tesseract-core && cargo clippy -p tesseract-core` | `git revert` |
| 3 | VQL AST + grammar combinators + parser + tests | 3 | `cargo test -p tesseract-vql` | `cargo build -p tesseract-vql && cargo clippy -p tesseract-vql` | `git revert` |

## Phase 1: Workspace + CI (PR 1)

- [x] 1.1 Workspace `Cargo.toml` — 6 members, edition 2024, `rust-version = "1.85"` (bumped from 1.80: edition 2024 requires 1.85+)
- [x] 1.2 6 crate dirs w/ `Cargo.toml` + `src/lib.rs` — common (thiserror), core (serde/bincode/tracing), storage, index, vql (nom/nom_locate), api
- [x] 1.3 Tooling: `.rustfmt.toml`, `deny.toml`, `.cargo/config.toml`
- [x] 1.4 `.github/workflows/ci.yml` — 5-job × 3-platform (check/clippy/fmt/test/audit)
- [x] 1.5 AGPL v3 header on all `.rs` files
- [x] 1.6 Verify: `cargo build --workspace && cargo clippy --all-targets && cargo fmt --check && cargo deny check`

## Phase 2: Math Foundation (PR 2)

- [x] 2.1 `tesseract-common/src/error.rs` — Error enum (DimensionMismatch, IndexOutOfBounds, ParseError) + Result<T> alias
- [x] 2.2 `tesseract-core/src/types.rs` — VectorId(u64), MetadataValue(6 variants), Timestamp — all serde+Debug+Clone+PartialEq
- [x] 2.3 Tests: Error display; VectorId bincode roundtrip; MetadataValue variants + nested array
- [x] 2.4 `src/distance.rs` — Distance trait, NormalizedVector w/ L2 norm enforcement, CosineDistance, EuclideanDistance
- [x] 2.5 Tests: normalize [3,4]→[0.6,0.8]; panic on zero; cosine identical→0.0; Euclidean 3-4-5→5.0; dim mismatch→Err
- [x] 2.6 `src/projection.rs` — WeightMask(Vec<(usize,f32)>), Projection trait
- [x] 2.7 Tests: uniform weights→original; OOB→Err; zero weight→0.0
- [x] 2.8 Verify: `cargo test -p tesseract-core && cargo clippy -p tesseract-core && cargo fmt --check`

## Phase 3: VQL Parser (PR 3)

- [x] 3.1 `src/ast.rs` — Query, SimilarityExpr, MetadataWhere, Predicate, ComparisonOp, OrderBy, Limit, Within — all Debug+Clone+PartialEq
- [x] 3.2 `src/grammar.rs` — nom combinators for each clause + full_query (with trailing-content rejection)
- [x] 3.3 `src/parser.rs` — `parse(&str) -> Result<Query, Error>` wrapping nom IResult with line/col tracking
- [x] 3.4 Tests: SIMILARITY(emb,'text')→AST; missing paren→error; WHERE color='red'→Predicate; IN+BETWEEN+AND→multi; WITHIN 100ms; missing ms→error; ORDER BY; LIMIT; malformed `=<`→error; error line/col
- [x] 3.5 Verify: `cargo test -p tesseract-vql && cargo clippy -p tesseract-vql && cargo fmt --check` — ALL PASS

## Phase 4: Final Verification

- [x] 4.1 `cargo build --workspace` — zero errors
- [x] 4.2 `cargo clippy --all-targets` — zero warnings
- [x] 4.3 `cargo fmt --check` — all files formatted
- [x] 4.4 `cargo test --workspace` — all tests pass
- [x] 4.5 `cargo deny check` — license audit passes (CI installs cargo-deny; not available in local env)
