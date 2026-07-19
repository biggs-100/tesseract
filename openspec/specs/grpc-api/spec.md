# gRPC API Specification

## Purpose

The gRPC API provides typed, streaming query execution over gRPC using the tonic framework, behind the `grpc` feature flag as a secondary transport.

## Requirements

### Requirement: Feature-gated compilation

The gRPC API MUST compile only when the `grpc` feature flag is enabled. When the flag is disabled, no gRPC dependencies or code are compiled.

#### Scenario: Compiles with feature flag

- GIVEN the workspace is built with `--features grpc`
- WHEN `cargo build` is run
- THEN the gRPC server code compiles
- AND the binary includes the gRPC routes

#### Scenario: Skipped without feature flag

- GIVEN the workspace is built without `--features grpc`
- WHEN `cargo build` is run
- THEN no gRPC-related code is compiled
- AND no tonic or prost dependencies are pulled in

### Requirement: Query RPC definition

The gRPC API MUST define a `Query` RPC that accepts a VQL string as input and returns a stream of `ScoredRecord` messages.

#### Scenario: Query RPC returns results

- GIVEN a running gRPC server and a valid VQL query string
- WHEN a client calls the `Query` RPC with `{ "vql": "FIND SIMILARITY(emb, [0.1, 0.2]) LIMIT 5" }`
- THEN the server responds with a stream of `ScoredRecord` messages
- AND each message contains `id: uint64`, `score: float`, and optional `metadata`

#### Scenario: Query RPC returns error on bad input

- GIVEN a running gRPC server
- WHEN a client calls the `Query` RPC with an invalid VQL string
- THEN the server returns a gRPC error with status `INVALID_ARGUMENT`
- AND the error message describes the parse failure

### Requirement: Tonic framework

The gRPC API MUST use `tonic` as the gRPC framework. The protobuf service definition MUST be compiled with `prost`.

#### Scenario: Service defined in proto

- GIVEN a protobuf service definition for `TesseractQuery` with the `Query` RPC
- WHEN the build script compiles the proto file
- THEN the generated Rust types are used in the tonic server implementation
- AND the server serves the service on a configurable gRPC address

### Requirement: Mirror HTTP API capabilities

The gRPC API SHOULD mirror the HTTP API capabilities, providing equivalent insert and health check RPCs in addition to Query.

#### Scenario: Insert RPC available

- GIVEN a running gRPC server
- WHEN a client calls the `Insert` RPC with `{ "id": 42, "vector": [0.1], "metadata": {...} }`
- THEN the server inserts the vector and returns `{ "id": 42 }`
- AND this mirrors the `POST /insert` HTTP endpoint behavior
