# Proposal: Phase 6 — gRPC API

## Intent

Implement the deferred gRPC API for Tesseract, behind a `grpc` feature flag. The gRPC API was scoped in Phase 3 (Query Engine) as a feature-gated secondary transport but never implemented.

## Scope

### In Scope
- Proto definition at `tesseract-api/proto/tesseract.proto` — Query, Insert, Health RPCs
- `build.rs` for proto compilation with prost
- Tonic service implementation in `tesseract-api/src/grpc.rs`
- Feature gate behind `grpc` flag (default off, zero deps in default build)
- Verification: both `cargo build --features grpc` and default build compile clean

### Out of Scope
- Streaming improvements (all-at-once matches HNSW semantics)
- Authentication / authorization / interceptors
- gRPC-web or TLS configuration
- Performance tuning or benchmarking
- Client SDK generation

## Approach

Add tonic + prost as optional dependencies behind `grpc` feature in `tesseract-api/Cargo.toml`. Define a `TesseractQuery` service in proto with Query (unary), Insert, and Health RPCs. Implement the tonic service trait in `grpc.rs` behind `#[cfg(feature = "grpc")]`, mirroring the existing HTTP API handlers from Phase 3. The build script compiles the proto at compile time. No runtime server wiring — the gRPC server is started from the same binary entry point when the feature is enabled.

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `tesseract-api/proto/tesseract.proto` | **New** | Protobuf service definition |
| `tesseract-api/build.rs` | **New** | Proto compilation with prost |
| `tesseract-api/src/grpc.rs` | **New** | Tonic service implementation |
| `tesseract-api/Cargo.toml` | Modified | Add tonic + prost behind `grpc` feature |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Proto changes incompatible with future needs | Low | Keep proto minimal, extend later |
| Feature gate leaks dependencies | Low | Verify `cargo build` (no flags) pulls zero tonic deps |

## Rollback Plan

Revert `grpc.rs`, `proto/`, `build.rs`, and the `grpc` feature section in `Cargo.toml`. All changes are additive — no existing functionality is removed.

## Dependencies

- `tonic` (optional, behind `grpc` feature)
- `prost` (optional, behind `grpc` feature)
- `prost-build` (build dependency, behind `grpc` feature)

## Success Criteria

- [ ] `cargo build --features grpc` compiles cleanly
- [ ] `cargo build` (default) compiles cleanly with zero gRPC deps
- [ ] gRPC service mirrors HTTP API: Query, Insert, Health RPCs
