# Proposal: Phase 0 — Foundation and Key Concepts

## Intent

Establish the Tesseract project scaffold and formalize core mathematical concepts before any engine code. All six crate stubs must compile, the VQL parser must parse sample queries into an AST, and the math foundation types must exist as traits — but nothing executes at runtime yet. This is the structural and conceptual bedrock.

## Scope

### In Scope
- **Rust workspace**: Cargo.toml with 6 crate members (core, storage, index, vql, api, common)
- **CI/CD**: GitHub Actions for build + lint + test on push/PR
- **Code quality**: rustfmt config, clippy config, deny.toml (license audit)
- **License**: AGPL v3 headers in all `.rs` source files
- **VQL grammar**: nom-based parser crate with AST types and grammar combinators — syntax only
- **Math foundation**: Core types (VectorId, MetadataValue, distance traits) and topological projection traits

### Out of Scope
- HNSW index implementation → Phase 2
- WAL / Parquet cold tier storage → Phase 2
- Query planner and executor → Phase 2
- gRPC/HTTP API layer → Phase 2
- Any runtime engine code or database operations

## Capabilities

### New Capabilities
- `project-scaffold`: Rust workspace with 6 crates, CI pipeline, lint/format/tooling configuration
- `vql-grammar`: nom-based VQL parser with complete AST — syntax-only, no execution
- `math-foundation`: Core types (VectorId, MetadataValue, Distance trait) and topological projection trait definitions

### Modified Capabilities
None — greenfield project with no existing specs.

## Approach

1. **Scaffold** workspace root `Cargo.toml` with 6 crate members and workspace-level dependency declarations
2. **Create** stub crates with module trees per exploration layout, each exposing a public `lib.rs`
3. **Configure** CI (GitHub Actions), lint (clippy), format (rustfmt), license audit (deny.toml)
4. **Add** AGPL v3 license headers to all `.rs` files
5. **Parse** VQL grammar in `tesseract-vql` — combinators for `SIMILARITY()`, `WITH METADATA WHERE`, `WITHIN`, `ORDER BY`, `LIMIT` clauses
6. **Define** core types and `Distance`/`Projection` traits in `tesseract-core` with serde + bincode roundtrip
7. **Verify** `cargo build`, `cargo clippy --all-targets`, `cargo fmt --check` all pass

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `Cargo.toml` | New | Workspace root with 6 crate member paths |
| `tesseract-*/Cargo.toml` | New | Per-crate manifests with dependencies |
| `tesseract-core/src/` | New | Core types, distance & projection traits |
| `tesseract-vql/src/` | New | Parser, AST, grammar combinators |
| `tesseract-{storage,index,api,common}/src/` | New | Stub crate modules with `lib.rs` |
| `.github/workflows/ci.yml` | New | Build + lint + test pipeline |
| `.rustfmt.toml`, `deny.toml` | New | Formatting and license audit rules |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| nom learning curve delays parser | Low | VQL grammar is small (~15 combinators); nom docs cover this well |
| Rust edition / dependency version conflicts | Low | Pin to stable toolchain; commit `Cargo.lock` |
| CI misconfiguration discovered late | Low | Validate CI on a throwaway branch before merging |

## Rollback Plan

Revert the PR that introduces Phase 0 files. No runtime data or migrations exist — this is a clean `git revert` with zero state to recover.

## Dependencies

- Rust stable toolchain (1.80+) available in CI and locally
- GitHub Actions runner available (public repo or self-hosted)

## Success Criteria

- [ ] `cargo build` succeeds for workspace and all 6 crates
- [ ] `cargo clippy --all-targets` passes with zero warnings
- [ ] `cargo fmt --check` passes
- [ ] CI passes on push (build + lint)
- [ ] VQL parser parses sample queries with `SIMILARITY()`, `WITH METADATA WHERE`, `WITHIN` clauses
- [ ] Core types compile with serde + bincode encoding roundtrip
- [ ] AGPL v3 headers present in all `.rs` files
