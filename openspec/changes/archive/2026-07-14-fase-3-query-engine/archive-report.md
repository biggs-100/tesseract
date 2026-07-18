# Archive Report: fase-3-query-engine

**Archived**: 2026-07-17
**Change**: fase-3-query-engine
**Project**: VQL (Tesseract)
**Mode**: openspec

## Status

| Field | Value |
|-------|-------|
| Tasks total | 27 (Phase 3 scope) |
| Tasks complete | 27 (all 27 in-scope tasks) |
| Verify verdict | PASS WITH WARNINGS |
| Archive type | intentional-with-warnings |

## Task Reconciliation Notes

### Task 1.8 — Stale Checkbox (reconciled at archive time)
- **Status**: Implemented post-verify as a correction
- **What**: `SimilarityExpr` now has `vector: Option<Vec<f64>>` field; parser grammar constructs it with `vector: None`; planner `plan_find` emits `FindClause::Vector` when `vector` is `Some`
- **Evidence**: All 235 tests pass (`cargo test --workspace`), clippy clean (`cargo clippy --all-targets -- -D warnings`), fmt clean (`cargo fmt --check`)
- **Rationale**: Not marked in tasks.md during apply. Verification identified it as a critical gap. Correction was applied post-verify and proven complete. Checkbox was stale.

### Tasks 5.1–5.5 — gRPC API (intentionally deferred)
- **Status**: Out of scope for Phase 3
- **Rationale**: Per user decision, gRPC tasks are deferred to a future phase. These 5 tasks remain in the archived tasks.md as unchecked to preserve the original task plan. They are NOT incomplete Phase 3 tasks — they were explicitly excluded from scope.

## Verify Report Summary

- **Verdict**: PASS WITH WARNINGS
- **Blockers**: 0
- **Critical findings**: 3 (all documented in verify-report.md — included Task 1.8 gap which was since corrected)
- **Requirements compliance**: 19/22
- **Scenarios coverage**: 24/28
- **Tests**: 235 passed, 0 failed
- **Build**: Clean
- **Clippy**: Zero warnings (`-D warnings`)
- **Warnings documented**:
  1. Episodic memory update formula deviates from spec (fixed α=0.7 vs confidence decay)
  2. Episodic convergence (REQ-M4) not implemented
  3. REQ-M5 (relevance scoring) not implemented
  4. Planner default ef differs from spec (50 vs 200)
  5. Cost estimation formula simplified per design

## Specs Synced to Main

| Domain | Action | Details |
|--------|--------|---------|
| query-planner | Created | 6 requirements, 8 scenarios |
| query-executor | Created | 6 requirements, 7 scenarios |
| episodic-memory | Created | 5 requirements, 7 scenarios |
| http-api | Created | 5 requirements, 7 scenarios |
| embedding-service | Created | 5 requirements, 6 scenarios |

## Archive Contents

- `proposal.md` ✅
- `exploration.md` ✅
- `specs/` (6 domains including grpc-api) ✅
- `design.md` ✅
- `tasks.md` ✅ (27/27 Phase 3 tasks)
- `verify-report.md` ✅
- `archive-report.md` ✅ (this file)
- `state.yaml` ✅

## Decisions Recorded

1. **gRPC deferred**: intentionally excluded from Phase 3 scope per orchestrator decision
2. **Task 1.8 corrected post-verify**: completed as a hotfix after verify reported it as a critical gap
3. **Simplified cost model**: implementation uses `ef × dim × 2 × ln(N) × cost_per_distance_ms` per design, diverging from original spec formula
4. **Simplified episodic memory**: fixed α=0.7 replacing confidence decay — acknowledged deviation from spec

## Source of Truth Updated

The following main specs now reflect Phase 3 behavior:
- `openspec/specs/query-planner/spec.md`
- `openspec/specs/query-executor/spec.md`
- `openspec/specs/episodic-memory/spec.md`
- `openspec/specs/http-api/spec.md`
- `openspec/specs/embedding-service/spec.md`
