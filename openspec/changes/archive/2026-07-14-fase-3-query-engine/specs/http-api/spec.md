# HTTP API Specification

## Purpose

The HTTP API exposes VQL query execution and vector insertion over REST using the axum framework, providing the primary external interface for the Tesseract system.

## Requirements

### Requirement: POST /query endpoint

The API MUST provide a `POST /query` endpoint accepting `{ "vql": "<string>" }` and returning `{ "results": [...] }` with scored results.

#### Scenario: Successful query

- GIVEN a running HTTP server with storage engine configured
- WHEN a client sends `POST /query` with body `{ "vql": "FIND SIMILARITY(emb, [0.1, 0.2]) LIMIT 5" }`
- THEN the server responds with HTTP 200
- AND the body contains `{ "results": [ { "id": "...", "score": 0.95, "metadata": {...} }, ... ] }`

#### Scenario: Bad VQL syntax

- GIVEN a running HTTP server
- WHEN a client sends `POST /query` with body `{ "vql": "INVALID SYNTAX!!!" }`
- THEN the server responds with HTTP 400
- AND the body contains a parse error description

### Requirement: POST /insert endpoint

The API MUST provide a `POST /insert` endpoint accepting `{ "id": u64, "vector": [f64], "metadata": {...} }` and returning the inserted ID.

#### Scenario: Successful insert

- GIVEN a running HTTP server
- WHEN a client sends `POST /insert` with body `{ "id": 42, "vector": [0.1, 0.2, 0.3], "metadata": { "title": "test" } }`
- THEN the server responds with HTTP 201
- AND the body contains `{ "id": 42 }`

#### Scenario: Insert with missing fields

- GIVEN a running HTTP server
- WHEN a client sends `POST /insert` with body `{ "id": 42 }` (missing vector)
- THEN the server responds with HTTP 400
- AND the body contains a validation error

### Requirement: GET /health endpoint

The API MUST provide a `GET /health` endpoint returning `{ "status": "ok" }`.

#### Scenario: Health check

- GIVEN a running HTTP server
- WHEN a client sends `GET /health`
- THEN the server responds with HTTP 200
- AND the body contains `{ "status": "ok" }`

### Requirement: HTTP status codes

The API MUST return proper HTTP status codes: 200 for success, 400 for client errors (malformed input, parse errors), 500 for server errors (storage failures, internal errors).

#### Scenario: Server error returns 500

- GIVEN a running HTTP server where the storage engine is unavailable
- WHEN a client sends `POST /query` with a valid query
- THEN the server responds with HTTP 500
- AND the body contains an error description

### Requirement: Axum framework

The API MUST use `axum` as the HTTP framework with tokio as the async runtime.

#### Scenario: Server starts with axum

- GIVEN the server is initialized
- WHEN the application starts
- THEN an axum `Router` is configured with the three routes
- AND the server binds to a configurable address (default: `0.0.0.0:3000`)
- AND graceful shutdown is supported via SIGINT/SIGTERM
