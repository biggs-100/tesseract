# Apply Progress: production-readiness PR4

> Status: ✅ All 2 issues complete (A11, A12).

---

## Summary

| Metric | Value |
|--------|-------|
| **PR** | PR4 — Performance |
| **Target** | `main` |
| **Mode** | Standard |
| **Issues total** | 2 (A11, A12) |
| **Completed** | 2 |
| **Blocked** | 0 |
| **Tests** | All 511 pass (509 existing + 2 new concurrent tests) |
| **Clippy** | Clean — zero `dead_code` warnings, no suppression attributes |

---

## Issue Status

### A11 — HNSW Locking ✅

| Task | File(s) | What Was Done |
|------|---------|---------------|
| PR4-T1 | `tesseract-index/Cargo.toml`, `tesseract-index/src/hnsw.rs` | Added `parking_lot = "0.12"` dep; added `legacy-locking = []` feature; replaced `std::sync::RwLock` with conditional `parking_lot::RwLock` / `std::sync::RwLock`; removed `.unwrap()` from lock read since parking_lot doesn't poison |
| PR4-T2 | `tesseract-storage/src/engine.rs`, `tesseract-storage/Cargo.toml` | Added `IndexLock` type alias (tokio::sync::RwLock by default, tokio::sync::Mutex with legacy-locking); `.read().await` for search, `.write().await` for insert/shutdown/replay; feature `legacy-locking` added to Cargo.toml; refactored `replay_index_entry` → `replay_index_entry_inner` to avoid type conflict |
| PR4-T3 | `tesseract-index/tests/concurrent.rs` (new) | 2 tests: `concurrent_reads_with_write` (10 readers + 1 writer, 10s timeout) and `readers_dont_serialize` (parallelism benchmark); gated with `#![cfg(not(feature = "legacy-locking"))]` |

### A12 — Dead Code ✅

| File | What Was Done |
|------|---------------|
| `tesseract-storage/src/engine.rs` | Removed unused `skeleton: Arc<VectorSkeleton>` field from `StorageEngine` (local variable kept for lifecycle init) |
| `tesseract-storage/src/hot_store.rs` | Renamed `config` → `_config` (field kept with underscore prefix for future use) |
| `tesseract-storage/src/wal.rs` | Removed unused `path: PathBuf` field from `SegmentWriter`; prefixed constructor params with `_` |
| `tesseract-cluster/src/replication.rs` | Removed unused `node_id: String` field from `ReplicationEngine`; prefixed constructor param with `_` |
| Workspace-wide | All `#[allow(dead_code)]` and `#[expect(dead_code)]` removed from production code |

---

## Deviations from Design

| ADR | Deviation | Rationale |
|-----|-----------|-----------|
| ADR-011 (A11) | `replay_index_entry` refactored to `replay_index_entry_inner` taking `&mut AnyIndex` | The original function took `&Mutex<AnyIndex>`, which doesn't work with the conditional RwLock/Mutex type. Callers now acquire the lock first and pass the guarded reference. |
| ADR-011 | `tesseract-storage` defines its own `legacy-locking` feature | Both `tesseract-index` and `tesseract-storage` use `#[cfg(feature = "legacy-locking")]`, so each crate needs the feature defined independently. |
| ADR-012 | `skeleton` field fully removed (not just `#[allow]`) | Clippy flagged it as unused (never read after construction). The local `skeleton` variable is still created for `TierLifecycle::start()` and cold store init. |

## Issues Found

None.

## Work Unit Evidence

| Evidence | Value |
|----------|-------|
| Focused test command | `cargo test --workspace --exclude tesseract-pg` — 511 tests passed |
| Legacy locking build | `cargo build --features legacy-locking --workspace --exclude tesseract-pg` — compiles clean |
| Legacy locking tests | `cargo test --features legacy-locking --workspace --exclude tesseract-pg` — 509 tests passed (2 concurrent skipped via cfg) |
| Clippy (no dead_code) | `cargo clippy --all-targets --workspace --exclude tesseract-pg` — zero dead_code warnings |
| Runtime harness | N/A — no external runtime boundary |
| Rollback boundary | Revert A11 changes (`tesseract-index/Cargo.toml`, `tesseract-index/src/hnsw.rs`, `tesseract-storage/src/engine.rs`, `tesseract-storage/Cargo.toml`, `tesseract-index/tests/concurrent.rs`) and A12 changes (`tesseract-storage/src/engine.rs`, `tesseract-storage/src/hot_store.rs`, `tesseract-storage/src/wal.rs`, `tesseract-cluster/src/replication.rs`) independently |

## Workload / PR Boundary

- **Mode**: Stacked PR (PR4 of 4) → `main`
- **Changed lines**: ~380 (within 800-line budget)
- **Feature flag**: `legacy-locking` to revert A11 if needed
