# pg-postgres-extension Specification

## Purpose

PostgreSQL extension (pgrx) that bridges PostgreSQL to Tesseract via HTTP sidecar. SQL-callable functions proxy vector/semantic queries to Tesseract's existing API — no core changes to Tesseract.

## Requirements

### Requirement: Extension Installation

The extension MUST be installable via `CREATE EXTENSION tesseract_fdw`. It SHALL register SQL functions accessible from any connected PG session.

#### Scenario: Successful installation

- GIVEN a PostgreSQL instance with the extension binary available
- WHEN a superuser runs `CREATE EXTENSION tesseract_fdw`
- THEN the extension loads successfully
- AND the functions `tesseract_query` and `tesseract_insert` become available in `public` schema

#### Scenario: Duplicate installation

- GIVEN the extension is already installed
- WHEN a user runs `CREATE EXTENSION tesseract_fdw` again
- THEN PostgreSQL returns `extension "tesseract_fdw" already exists`

### Requirement: Connection Configuration

The extension MUST accept a Tesseract HTTP endpoint via `tesseract_connect(host text, port integer)`. The connection SHALL persist for the session.

#### Scenario: Configure connection

- GIVEN the extension is loaded
- WHEN a user runs `SELECT tesseract_connect('http://localhost', 8080)`
- THEN the endpoint `http://localhost:8080` is stored for the session
- AND subsequent queries reach Tesseract at that endpoint

#### Scenario: Query before connection

- GIVEN the extension is loaded but no connection is configured
- WHEN a user calls `tesseract_query(...)`
- THEN the extension raises a PG ERROR: `tesseract_fdw: no connection configured`

### Requirement: VQL Query Execution

The system MUST expose `tesseract_query(vql text)` returning a table with columns `id BIGINT`, `score REAL`, `metadata JSONB`. It SHALL proxy the VQL string to Tesseract's `POST /query`.

#### Scenario: Successful query

- GIVEN a connection is configured and Tesseract is reachable
- WHEN a user runs `SELECT * FROM tesseract_query('{"collection":"docs","query":[0.1,0.2],"k":5}')`
- THEN the function returns up to 5 rows with columns `id`, `score`, `metadata`

#### Scenario: Tesseract unreachable

- GIVEN a connection is configured but Tesseract is not running at the endpoint
- WHEN a user calls `tesseract_query(...)`
- THEN the extension raises a PG ERROR: `tesseract_fdw: connection refused`

### Requirement: Data Insertion

The system MUST expose `tesseract_insert(id BIGINT, vector REAL[], metadata JSONB)` returning `BIGINT`. It SHALL proxy the data to Tesseract's `POST /insert`.

#### Scenario: Successful insert

- GIVEN a connection is configured and Tesseract is reachable
- WHEN a user runs `SELECT tesseract_insert(42, ARRAY[0.1,0.2,0.3], '{"title":"hello"}'::jsonb)`
- THEN the function returns `42`

#### Scenario: Dimension mismatch

- GIVEN a connection is configured and the collection expects 3-dimensional vectors
- WHEN a user calls `tesseract_insert(1, ARRAY[1.0], '{}'::jsonb)`
- THEN the extension returns a PG ERROR from Tesseract's rejection: `tesseract_fdw: dimension mismatch`

### Requirement: Type Mapping

The extension MUST map PG types to Tesseract types as follows:

| PG Type | Tesseract Type | Description |
|---------|---------------|-------------|
| `BIGINT` | `u64` | Row IDs |
| `REAL` | `f32` | Score/relevance |
| `REAL[]` | `Vec<f32>` | Vector embedding |
| `JSONB` | `serde_json::Value` | Arbitrary metadata |

#### Scenario: Type round-trip

- GIVEN a row inserted via `tesseract_insert` with all three column types
- WHEN retrieved via `tesseract_query`
- THEN the returned row matches the inserted types: `id` is `BIGINT`, `score` is `REAL`, `metadata` is `JSONB`
