# Tesseract

[![CI](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml/badge.svg)](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Audit](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml/badge.svg?job=audit)](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml)
[![Audit](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml/badge.svg)](https://github.com/tesseract-db/tesseract/actions/workflows/ci.yml)

> **A semantic-relational database engine that unifies structured and unstructured data.**
>
> Tesseract redefines hybrid search by *projecting metadata into the vector space itself* — making filters a geometric constraint, not an afterthought.

---

## The Problem

Every vector database today has the same three limitations:

| Limitation | Consequence |
|---|---|
| **Filters are post-hoc** | `WHERE category = 'science'` runs *after* ANN search. If the top-10 nearest neighbors don't match the filter, you get **zero results** — even if result #11 was perfect. |
| **Data goes stale** | New vectors sit in a buffer, invisible to the ANN index, until an expensive full rebuild. Nightly reindexing is the norm. |
| **No personalization** | Every user gets the same ranking. Implicit preferences (recency, clicked categories) require a separate system. |

These aren't implementation bugs — they're **architectural limitations** of treating vectors and metadata as separate concerns.

---

## The Tesseract Approach

Tesseract solves all three with a unified architecture:

```
┌─────────────────────────────────────────────────────────────────┐
│                        Tesseract DB                             │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                    VQL Engine                             │   │
│  │  ┌────────────┐  ┌───────────┐  ┌─────────────────────┐  │   │
│  │  │  Parser    │→ │  Planner  │→ │  Executor           │  │   │
│  │  │  (10 cls)  │  │  (6 ops)  │  │  (algebraic tree)   │  │   │
│  │  └────────────┘  └───────────┘  └─────────────────────┘  │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────── TOPOLOGICAL INDEX ────────────────────────────┐   │
│  │  q' = q + Σ αᵢ · δᵢ        query shifted toward filters │   │
│  │  δ_cat = centroid(cat) - global_centroid                 │   │
│  │  δ_num = bucket_centroid(range) - global_centroid        │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────── MERKLE TREE ─────────────────────────────────┐   │
│  │  HotBuffer (mem, 10k)  ──async merge──► MerkleTree (disk)│   │
│  │  Queries search BOTH in parallel                         │   │
│  │  Result: 100% freshness, zero rebuilds                   │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────── STORAGE ──────────────────────────────────────┐   │
│  │  HNSW (ANN) │ HotStore │ ColdStore │ WAL │ Cache        │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────── POSTGRESQL ───────┐   ┌─────── GRPC ────────────┐    │
│  │  tesseract_query(vql)    │   │  TesseractQuery service │    │
│  │  tesseract_insert(...)   │   │  Query / Insert / Health│    │
│  └──────────────────────────┘   └──────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

---

## The Three Pillars

### 1. VQL — Vector Query Language

A purpose-built language for semantic search with first-class metadata, latency constraints, and personalization. 10 clause types, order-independent parsing, 113 tests.

```vql
-- Basic semantic search
FIND SIMILARITY(emb, 'quantum computing') LIMIT 10

-- With metadata filter + topological projection
FIND SIMILARITY(emb, 'quantum computing')
  PROJECT ON year, category
  WITH METADATA WHERE year BETWEEN 2020 AND 2025
    AND category IN ('science', 'physics')
  LIMIT 20

-- With latency budget and personalization
FIND SIMILARITY(emb, 'machine learning')
  BIAS recency()
  LIMIT 5 WITHIN 100ms

-- Pre-computed vector
FIND SIMILARITY(emb, VECTOR(0.1, 0.2, 0.3, 0.4))
  WITH METADATA WHERE tags LIKE 'deep%'
  LIMIT 10 OFFSET 20
```

### 2. Topological Dynamic Index

Instead of post-filtering, metadata filters become a **geometric constraint** on the query vector. At search time, the query is shifted toward the region that matches the filter — using centroid deltas computed incrementally, with zero training.

**Benchmark results** (1M vectors, 128d, adaptive α ∈ [0.2, 0.7]):

| Filter type | Without bias | With bias (adaptive) | Improvement |
|---|---|---|---|
| Category (`category = 'science'`) | 0.60 recall@10 | **0.71 recall@10** | **+18%** |
| Year range (`year >= 2020`) | 0.07 | **0.28** | **+307%** |
| Combined (category + year) | 0.02 | **0.31** | **+1437%** |
| No filter | 1.00 | 1.00 | **0% (no regression)** |

The bias is:
- **Deterministic** — no MLP, no training, no epochs
- **Incremental** — centroids update O(1) per insert
- **Query-time only** — the HNSW graph never changes

### 3. Progressive Merkle Tree

New vectors are immediately queryable via an in-memory **HotBuffer**, then asynchronously merged into a **Merkle Tree** of cluster centroids. No rebuilds, no downtime, no stale data.

**Benchmark results**:

| Metric | Value |
|---|---|
| Insert throughput | **2.7M vectors/sec** |
| Merge latency (10k batch) | **97 ms** |
| Freshness (hot buffer recall) | **100%** |
| Freshness (without merkle) | **0%** (data invisible until rebuild) |

---

## Quickstart

### Build and run the REPL

```bash
git clone https://github.com/tesseract-db/tesseract.git
cd tesseract

# Build everything
cargo build --release -p tesseract-vql

# Launch the interactive VQL REPL
cargo run --bin vql -- --data-dir ./demo-data
```

```vql
vql> INSERT id:1 VECTOR [0.1, 0.2, 0.3, 0.4] METADATA {"category": "science", "year": 2024}
vql> INSERT id:2 VECTOR [0.9, 0.8, 0.7, 0.6] METADATA {"category": "history", "year": 2019}
vql> FIND SIMILARITY(emb, VECTOR(0.15, 0.25, 0.35, 0.45))
       PROJECT ON category
       WITH METADATA WHERE category = 'science'
       LIMIT 5
```

### HTTP API

```bash
# Start the server
cargo run --release -p tesseract-api

# Insert
curl -X POST http://localhost:3000/insert \
  -H "Content-Type: application/json" \
  -d '{"id": 1, "vector": [0.1, 0.2, 0.3], "metadata": {"title": "hello world"}}'

# Query
curl -X POST http://localhost:3000/query \
  -H "Content-Type: application/json" \
  -d '{"vql": "FIND SIMILARITY(emb, VECTOR(0.1, 0.2, 0.3)) LIMIT 5"}'

# Health check
curl http://localhost:3000/health/liveness
```

### Docker Compose

```bash
docker compose up -d
# Tesseract on :8081, PostgreSQL 16 on :5432
```

---

### Authentication

Tesseract supports three authentication modes, configured via `TESSERACT_AUTH_MODE`:

- **`none`** (default) — All requests are accepted. Use for development and evaluation.
- **`api-key`** — Requests must include an `X-API-Key` header with a valid key.
  Keys are configured via `TESSERACT_API_KEYS` in `key:role,key:role` format.
- **`jwt`** — Requests must include an `Authorization: Bearer <token>` header.
  Tokens are verified using HMAC HS256 with the secret from `TESSERACT_JWT_SECRET`.
- **`both`** — Both authentication methods are accepted. The server tries API key first,
  then JWT.

Health endpoints (`/health/*`) and metrics (`/metrics`) are always public regardless
of the authentication mode.

```bash
# Start with API key auth
TESSERACT_AUTH_MODE=api-key TESSERACT_API_KEYS="sk-abc123:admin,sk-def456:reader" cargo run -p tesseract-api

# Query with the key
curl -H "X-API-Key: sk-abc123" http://localhost:3000/query -d '{"vql": "FIND SIMILARITY(emb, VECTOR(0.1, 0.2, 0.3)) LIMIT 5"}'
```

### Observability

Tesseract exposes health checks, Prometheus metrics, and structured logging.

**Health endpoints:**

| Endpoint | Description |
|---|---|
| `GET /health/liveness` | Returns `200 {"status": "pass"}` when the process is alive |
| `GET /health/readiness` | Returns `200` with component status (WAL, index, HotBuffer) |

**Metrics** (with `--features otel`):

| Endpoint | Description |
|---|---|
| `GET /metrics` | Prometheus-format metrics (queries, inserts, latency, index size) |

**Structured logging:**

Set `TESSERACT_LOG_FORMAT=json` for JSON-formatted log output, suitable
for ingestion by log aggregators.

---

## Project Structure

```
├── tesseract-common/      # Shared types, errors, traits
├── tesseract-core/        # Topological bias, embedding, episodic memory
│   └── topological.rs     # CentroidTracker, NumericalBucketTracker, apply_bias
├── tesseract-storage/     # Storage engine (WAL, hot/cold, page cache)
│   └── engine.rs          # Hybrid search: HNSW + HotBuffer + MerkleTree
├── tesseract-index/       # ANN indexing
│   ├── hnsw.rs            # HNSW graph (Malkov & Yashunin 2016)
│   └── merkle/            # Progressive Merkle Tree (HotBuffer, MerkleNode, MerkleTree)
├── tesseract-vql/         # VQL language
│   ├── grammar.rs         # Parser (nom, 10 clause types)
│   ├── planner.rs         # Algebra-based planner (6 composable operators)
│   ├── executor.rs        # Algebraic tree executor
│   └── repl.rs            # Interactive REPL binary
├── tesseract-api/         # HTTP API (axum) + gRPC (tonic, feature-gated)
├── tesseract-cluster/     # Distributed mode (sharding, replication, failover)
├── tesseract-pg/          # PostgreSQL extension (pgrx, sidecar HTTP)
├── examples/              # Quickstart scripts (Bash, Python, SQL)
├── Dockerfile             # Multi-stage build
├── docker-compose.yml     # Tesseract + PostgreSQL 16
└── CHANGELOG.md
```

---

## Benchmarks

| Benchmark | Metric | Result |
|---|---|---|---|
| Topological Index (category) | recall@10 | 0.71 (+18% vs baseline) |
| Topological Index (year range) | recall@10 | 0.28 (+307% vs baseline) |
| Topological Index (combined) | recall@10 | 0.31 (+1437% vs baseline) |
| Merkle Tree insert | throughput | 1.8M vectors/sec |
| Merkle Tree merge | latency | 135 ms / 10k batch |
| Merkle freshness | recall@10 | 100% |
| HNSW recall (baseline) | recall@10 | 0.95 @ ef=200 |
| Workspace tests | count | 511, 0 failures |

---

## Configuration

| Variable | Default | Description |
|---|---|---|
| `TESSERACT_DATA_DIR` | `./data` | Persistent storage path |
| `TESSERACT_LISTEN_ADDR` | `0.0.0.0:3000` | HTTP bind address |
| `TESSERACT_GRPC_ADDR` | `0.0.0.0:50051` | gRPC bind address (with `--features grpc`) |
| `TESSERACT_AUTH_MODE` | `none` | Authentication mode: `none`, `api-key`, `jwt`, or `both` |
| `TESSERACT_API_KEYS` | — | Comma-separated `key:role` pairs for API key auth |
| `TESSERACT_JWT_SECRET` | `dev-secret-do-not-use-in-prod` | HMAC secret for JWT verification (HS256) |
| `TESSERACT_RATE_LIMIT_RPM` | `100` | Max requests per minute per IP |
| `TESSERACT_QUERY_TIMEOUT_SECS` | `30` | Default timeout for queries without explicit `WITHIN` |
| `TESSERACT_SHUTDOWN_TIMEOUT_SECS` | `30` | Max time to drain buffers and flush WAL on shutdown |
| `TESSERACT_EMBEDDING_TIMEOUT_SECS` | `30` | Per-request timeout for embedding API calls |
| `TESSERACT_EMBEDDING_RETRY_MAX` | `3` | Max retries on embedding API rate limits (429) or server errors (5xx) |
| `TESSERACT_LOG_FORMAT` | `text` | Log output format: `text` or `json` |
| `RUST_LOG` | `info` | Logging level (e.g. `debug`, `trace`, `warn`) |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[AGPL-3.0-only](LICENSE) — © 2026 Tesseract Contributors.
