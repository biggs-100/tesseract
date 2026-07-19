# Tasks: Phase 6 — gRPC API

## Review Workload Forecast

Decision needed before apply: No
Chained PRs recommended: No
Chain strategy: single-pr
400-line budget risk: Low

| Field | Value |
|-------|-------|
| Estimated changed lines | ~200 (proto + build.rs + grpc.rs + Cargo.toml) |
| 400-line budget risk | Low |
| Chained PRs recommended | No |
| Suggested split | Single PR — all changes fit within 400-line budget |
| Delivery strategy | single-pr |
| Chain strategy | single-pr |

## Tasks

- [x] 1.1 Create `tesseract-api/proto/tesseract.proto` — `TesseractQuery` service with Query, Insert, Health RPCs + `ScoredRecord`, `QueryRequest`, `QueryResponse`, `InsertRequest`, `InsertResponse`, `HealthRequest`, `HealthResponse` messages
- [x] 1.2 Create `tesseract-api/build.rs` — compile proto with prost using `tonic_build::compile_protos`, gated behind `cfg(feature = "grpc")`
- [x] 1.3 Create `tesseract-api/src/grpc.rs` — tonic `TesseractQuery` service impl behind `#[cfg(feature = "grpc")]`, mirroring HTTP API handlers (Query, Insert, Health)
- [x] 1.4 Modify `tesseract-api/Cargo.toml` — add `tonic` + `prost` as optional deps behind `grpc` feature; add `prost-build` behind `grpc` in `[build-dependencies]`
- [x] 1.5 Verify: both `cargo build --features grpc` and `cargo build` (default) compile clean
