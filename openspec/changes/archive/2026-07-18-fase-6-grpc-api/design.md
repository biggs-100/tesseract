# Design: Phase 6 — gRPC API

## Technical Approach

Implement the deferred gRPC secondary transport using tonic + prost, feature-gated behind the `grpc` flag. The proto lives at `tesseract-api/proto/tesseract.proto`, compiled by a `build.rs` at `tesseract-api/build.rs`. The tonic service implementation mirrors the existing HTTP API handlers from Phase 3 (Query, Insert, Health) behind `#[cfg(feature = "grpc")]`.

## Architecture Decisions

### Decision: gRPC Framework
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Tonic + Prost | Ecosystem standard, HTTP/2, tonic-build, tonic-reflection | **Chosen** |
| gRPC-web | Adds envoy/envoy-grpc-web proxy complexity | Rejected |
| Manual HTTP/2 framing | Maintenance burden, no codegen | Rejected |

### Decision: Feature Gate
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Feature flag `grpc` (default off) | Zero deps in default build, explicit opt-in | **Chosen** |
| Always on | Adds tonic/prost compile time for all users | Rejected |

### Decision: Proto Location
| Option | Tradeoff | Decision |
|--------|----------|----------|
| `tesseract-api/proto/tesseract.proto` | Co-located with server code, standard tonic layout | **Chosen** |
| Workspace root `proto/` | Over-engineered for single service | Rejected |

### Decision: Build Script
| Option | Tradeoff | Decision |
|--------|----------|----------|
| `build.rs` in `tesseract-api` | Standard prost integration, conditional on feature | **Chosen** |
| Manual codegen + commit | Stale generated files, merge conflicts | Rejected |

### Decision: RPC Style
| Option | Tradeoff | Decision |
|--------|----------|----------|
| Unary Query + Insert + Health | Mirrors HTTP API, simplest client | **Chosen** |
| Server-streaming Query | Possible future optimization | Deferred |

## Data Flow

```
gRPC client
  → Tonic /TesseractQuery/Query { vql: "..." }
    → #[cfg(feature = "grpc")] TesseractQueryService::query()
      → tesseract_vql::execute(vql, &storage)
        → ScoredResult { id, score, metadata }
          → Unary gRPC response { results: [...] }
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `tesseract-api/proto/tesseract.proto` | Create | TesseractQuery service — Query, Insert, Health RPCs + ScoredRecord message |
| `tesseract-api/build.rs` | Create | Compile proto with prost, gated behind `cfg(feature = "grpc")` |
| `tesseract-api/src/grpc.rs` | Create | Tonic impl behind `#[cfg(feature = "grpc")]` — mirrors HTTP handlers |
| `tesseract-api/Cargo.toml` | Modify | Add tonic + prost behind `grpc` feature; add prost-build behind `grpc` build-dep |

## Interfaces

```protobuf
// tesseract-api/proto/tesseract.proto
service TesseractQuery {
  rpc Query(QueryRequest) returns (QueryResponse);
  rpc Insert(InsertRequest) returns (InsertResponse);
  rpc Health(HealthRequest) returns (HealthResponse);
}

message QueryRequest {
  string vql = 1;
}

message ScoredRecord {
  uint64 id = 1;
  double score = 2;
  string metadata = 3;  // JSON-encoded
}

message QueryResponse {
  repeated ScoredRecord results = 1;
  map<string, double> timings = 2;
}
```

```rust
// tesseract-api/src/grpc.rs
#[cfg(feature = "grpc")]
pub mod grpc_server {
    use tonic::{Request, Response, Status};
    use crate::proto::tesseract_query_server::{TesseractQuery, TesseractQueryServer};
    // ...
}
```

## Testing Strategy

| Layer | What | Approach |
|-------|------|----------|
| Compilation | Feature-gated build | `cargo build -p tesseract-api --features grpc` |
| Compilation | Default build | `cargo build -p tesseract-api` — verify zero tonic deps |
| Unit | Tonic service impl | `cargo test -p tesseract-api --features grpc` |

## Threat Matrix

N/A — no routing, shell, subprocess, or process-integration boundary. Proto schema is application-defined with no user-provided message types.

## Migration / Rollout

No migration — all changes are additive. The `grpc` feature flag defaults to off, so existing builds are unaffected. The gRPC server entry point is added alongside the existing HTTP server; no API routing changes.

## Open Questions

- Should the gRPC server listen on a separate port or share with HTTP/2? **Resolution**: Separate port (configurable via env `GRPC_ADDR`), keeping HTTP/1.1 axum on its own port.
