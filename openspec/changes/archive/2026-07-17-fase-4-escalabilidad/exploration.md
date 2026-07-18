# Phase 4 — Scalability: Exploration

## Current State

Tesseract is a **single-node, single-process** vector database engine. All phases so far assumed one machine:

| Crate | Role | Single-node assumption |
|-------|------|----------------------|
| `tesseract-storage` | WAL + hot/cold tiers + page cache + skeleton + ANN index | All files local, process-local mutexes |
| `tesseract-index` | HNSW with weighted distance (Cosine/Euclidean) | In-memory graph behind `tokio::sync::Mutex` |
| `tesseract-vql` | VQL parser + planner + executor | Local `StorageEngine::search()` call |
| `tesseract-api` | HTTP server (Axum) | Single `Arc<StorageEngine>`, no clustering |
| `tesseract-core` | Types (`VectorId(u64)`), embedding, episodic memory | No distribution awareness |
| `tesseract-common` | Error types | No network errors, no consensus errors |

**Key constraint**: `StorageEngine` is the sole entry point for all reads and writes — it owns the WAL, the hot store, the cold store, and the index. There is zero distribution awareness in any crate.

---

## Subsystem Analysis

### 1. Sharding — Data Distribution

#### Option A: Hash-based (consistent hashing on `VectorId`)

| Aspect | Detail |
|--------|--------|
| **How** | Hash `VectorId(u64)` using consistent hash (jump hash or ring hash) to assign vectors to shards |
| **Implementation** | `VectorId` is already a `u64` — trivial to hash. Add a `ConsistentHasher` utility in `tesseract-common` |
| **Pros** | Even distribution; no hot spots; simple rebalance (only what moved between nodes) |
| **Cons** | Range queries (`WHERE timestamp > X`) hit ALL shards; cannot colocate related vectors |
| **Cons for ANN** | Similar vectors land on different nodes — every query must scatter-gather |
| **Complexity** | **Low** — jump hash is ~50 lines, ring hash ~150 lines |

#### Option B: Metadata-based (partition key from metadata field)

| Aspect | Detail |
|--------|--------|
| **How** | Client specifies a partition key (e.g., `tenant_id`, `category`, `date_bucket`) |
| **Implementation** | Extract key from `serde_json::Value` metadata at insert time; route by key |
| **Pros** | Natural data isolation; range queries within a partition hit one shard; tenant-level QoS |
| **Cons** | Hot spots if one partition grows or gets more writes; rebalance requires moving entire partitions; schema coupling |
| **Cons for ANN** | Excellent if queries are also scoped by partition key (tenant-specific search) |
| **Complexity** | **Medium** — need metadata extraction, partition-to-shard mapping, and rebalance logic |

#### Option C: Vector-space clustering

| Aspect | Detail |
|--------|--------|
| **How** | Run K-means or similar on the vector space; assign each centroid to a shard; vectors route to nearest centroid |
| **Implementation** | Run periodic clustering; store centroid→shard mapping in coordination service |
| **Pros** | Similar vectors co-located on same node — ANN queries hit fewer shards; ideal recall/performance |
| **Cons** | Very complex rebalancing; requires full vector scan to recompute centroids; cold-start problem for new vectors |
| **Cons** | Write path latency (must compute nearest centroid before routing) |
| **Complexity** | **Very High** — clustering at scale is a research-level problem |

#### Initial Recommendation

**Option A (hash-based) for MVP**. Jump hash on `VectorId(u64)`:

- Zero schema coupling
- Deterministic routing with no state needed client-side
- Simple rebalance (virtual nodes or jump-hash ring expansion)
- All ANN queries work correctly (scatter-gather) — no algorithmic change needed
- Can add metadata-based routing later as an optimization overlay

### 2. Replication

#### Option A: Leader-follower (async)

| Aspect | Detail |
|--------|--------|
| **How** | One primary per shard writes; followers tail the WAL asynchronously |
| **Implementation** | Replicate WAL segments to followers; followers apply to their local index |
| **Pros** | Simple; high write throughput; well-understood |
| **Cons** | Replica lag → stale reads if follower is queried; failover may lose last writes |
| **Complexity** | **Medium** |

#### Option B: Leader-follower (sync, quorum)

| Aspect | Detail |
|--------|--------|
| **How** | Writer waits for N/2+1 followers to ack before confirming |
| **Implementation** | Add quorum ack to WAL replication; `WriteMode::Durable` waits for quorum |
| **Pros** | Stronger durability; no lost writes on failover |
| **Cons** | Latency penalty (network RTT to majority); harder to implement correctly |
| **Complexity** | **Medium-High** |

#### Option C: Raft consensus (embedded)

| Aspect | Detail |
|--------|--------|
| **How** | Each shard is a Raft group; log replication is the Raft log |
| **Implementation** | Use `openraft` or `raft-rs` (tikv); embed in `tesseract-storage` |
| **Pros** | Leader election built-in; strong consistency; well-proven in production (etcd, TiKV) |
| **Cons** | Heavier per-shard overhead; Raft requires at least 3 nodes for quorum; careful about snapshot/log compaction |
| **Complexity** | **High** — but well-solved by existing Rust crates |

#### Initial Recommendation

**Option A (async leader-follower) for MVP**, with a path to Raft in later iterations. Why:

- The WAL already exists with segment-based log structure — replicating WAL segments to followers is a natural extension
- Vector search is inherently approximate — temporary stale reads from replica lag are acceptable for an MVP
- Can introduce Raft later per-shard for the metadata/catalog (strong consistency needed for shard assignments) without forcing it on the data path

### 3. Coordination Service

#### Option A: Embedded Raft (single-node coordinator, replicated)

| Aspect | Detail |
|--------|--------|
| **How** | Same binary runs a small Raft cluster for metadata (shard map, node membership, leader info) |
| **Implementation** | `openraft` crate; store metadata in a small embedded RocksDB or sled |
| **Pros** | No external dependency; single binary deployment |
| **Cons** | Adds complexity to the binary; Raft for metadata is overkill for small clusters |
| **Complexity** | **Medium-High** |

#### Option B: etcd (external)

| Aspect | Detail |
|--------|--------|
| **How** | etcd cluster (3 nodes) stores shard assignments, node heartbeats, config |
| **Implementation** | etcd client (etcd-client crate) with lease-based health checks |
| **Pros** | Battle-tested; watch API for config changes; simple API |
| **Cons** | External dependency; operational overhead; Java/Python bindings not needed here |
| **Complexity** | **Low** for integration; **Medium** for ops |

#### Option C: Custom gossip-based

| Aspect | Detail |
|--------|--------|
| **How** | Nodes gossip membership + shard ownership using SWIM or similar |
| **Implementation** | Membership protocol in `tesseract-common`; no external store |
| **Pros** | No external dependency; fully autonomous |
| **Cons** | **Very high implementation effort**; subtle bugs in failure detection |
| **Complexity** | **Very High** |

#### Initial Recommendation

**Option B (etcd)** for MVP and the foreseeable future:

- Mature, Go-based, small footprint
- `etcd-client` crate works with tokio async runtime (already the project's runtime)
- Watch API simplifies node discovery and rebalance triggers
- Can later swap to embedded Raft if the external dependency becomes a problem

### 4. Distributed Query Execution

#### Option A: Scatter-gather (exact cross-shard search)

| Aspect | Detail |
|--------|--------|
| **How** | Coordinator sends query to ALL shards; each shard returns its top-k; coordinator merges and returns global top-k |
| **Implementation** | New `Coordinator` component in `tesseract-vql` or a new `tesseract-coordinator` crate |
| **Pros** | Correct (exact across shards); simple to implement and debug |
| **Cons** | Every query hits every shard — O(nodes) search cost; higher tail latency |
| **Complexity** | **Low-Medium** |

#### Option B: Two-phase search with approximate routing

| Aspect | Detail |
|--------|--------|
| **How** | Phase 1: probe a subset of shards (e.g., closest centroid shards); Phase 2: refine |
| **Implementation** | Requires vector-space clustering (Option C in sharding) or metadata routing |
| **Pros** | Fewer shards queried per request; lower latency |
| **Cons** | Approximate — may miss relevant results that live on unprobed shards |
| **Complexity** | **High** (depends on sharding strategy) |

#### Option C: Cached coordinator results

| Aspect | Detail |
|--------|--------|
| **How** | Coordinator caches recent query results by query hash; TTL-based invalidation |
| **Implementation** | LRU cache at coordinator level; invalidate on insert to relevant shard |
| **Pros** | Fast repeat queries; reduces load on shards |
| **Cons** | Cache invalidation is complex with distributed writes; stale results for time-sensitive queries |
| **Complexity** | **Medium** |

#### Initial Recommendation

**Option A (scatter-gather)** for MVP, with Option C (caching) as a bolt-on:

- Works with hash-based sharding (the simplest sharding option)
- No algorithmic changes to the HNSW search — each shard runs the existing `TopologicalIndex::search()`
- Merge step is a simple top-k across sorted lists — O(n*k*log(k))
- Caching (Option C) can be added later with query-param hashing

### 5. Consistency Model

| Model | Read guarantees | Write guarantees | Complexity |
|-------|----------------|-----------------|------------|
| **Eventual** | Stale reads allowed | Writes always succeed async | Lowest |
| **Read-your-writes** | Reader sees its own writes (sticky session) | Async replication, but reads route to origin | Low |
| **Linearizable** | Every read sees the latest write | Quorum-based, slow but correct | Highest |

#### Initial Recommendation

**Eventual consistency for the data path** + **Linearizability for the metadata/catalog**:

- Vector search is inherently approximate — the difference between eventual and linearizable is lost in the ANN recall noise
- Metadata (shard assignments, node membership) MUST be linearizable — use etcd for this
- Read-your-writes can be achieved per-session by client-side stickiness if needed later

---

## Recommended MVP Architecture

```
┌──────────────────────────────────────────────────────────┐
│                      etcd cluster                         │
│  (shard assignment, node membership, leader election)     │
└──────────────┬───────────────────────────────┬──────────-┘
               │ HTTP (client)                 │ etcd watch
               ▼                               │
┌──────────────────────────────┐               │
│      Coordinator (proxy)     │              │
│  - route INSERT to shard[hash(id)]         │
│  - scatter-gather SEARCH to ALL shards     │
│  - merge top-k results                     │
│  - health-check via /health                │
└──┬───────────────┬───────────────┬──────────┘
   │ HTTP gRPC?    │               │
   ▼               ▼               ▼
┌──────────┐ ┌──────────┐ ┌──────────┐
│ Shard 0  │ │ Shard 1  │ │ Shard N  │
│ (node A) │ │ (node B) │ │ (node C) │
│ Hashed   │ │ Hashed   │ │ Hashed   │
│ 1/16th   │ │ 1/16th   │ │ 1/16th   │
│ of data  │ │ of data  │ │ of data  │
└──────────┘ └──────────┘ └──────────┘
     │            │            │
     └────────────┼────────────┘
                  ▼
         Async WAL replication
         (to follower nodes)
```

### New Crate: `tesseract-coordinator`

- **Location**: `tesseract-coordinator/src/lib.rs`
- **Responsibilities**:
  - etcd client for shard map + membership watching
  - HTTP/gRPC client to forward queries to shards
  - Scatter-gather merge for SEARCH
  - Health check polling
- **Dependencies**: `etcd-client`, `reqwest` (or `tonic` for gRPC), `tokio`

### Modified Crates

| Crate | Changes needed |
|-------|---------------|
| `tesseract-storage` | Add `shard_id` config; expose replication endpoint (tail WAL or accept replication stream) |
| `tesseract-common` | Add `CoordinatorError`, `ShardId` type; consistent hash utility |
| `tesseract-api` | Split into shard mode (serves data) vs client-facing mode (proxies to coordinator); OR keep as single binary with `--mode` flag |
| `tesseract-vql` | Coordinator-aware executor (scatter-gather instead of local search) |

### First slice (true MVP)

1. **JumpHash on `VectorId`** → deterministic shard routing
2. **etcd-based membership** → nodes register on startup, heartbeat via lease
3. **Coordinator as a proxy** → single binary, `--coordinator` vs `--data` mode
4. **Scatter-gather SEARCH** → query all shards, merge top-k
5. **Direct INSERT** → coordinator routes to correct shard (no replication initially)
6. **No replication** → single copy of each shard (replication = Phase 4.1)

---

## Risks

### Network Partitions
- **Split-brain in scatter-gather**: If the coordinator can't reach some shards, results are incomplete. Mitigation: mark partial results with a `partial: true` flag, or return error if quorum of shards is unreachable.
- **Split-brain in metadata**: etcd handles this natively with Raft — the minority partition can't write. Good.
- **Mitigation**: Coordinator should only proceed if `N/2 + 1` shards respond within a configurable timeout.

### Consistency Under Partition
- **Stale shard map**: If the coordinator has a stale shard assignment (partition happened while it was isolated), vectors may route to the wrong node. Mitigation: coordinator validates shard assignment against etcd before every write (cheap with etcd revision checks).

### Operational Complexity
- **Multi-node debugging**: Distributed systems are harder to debug. Need structured logging with node IDs and request tracing.
- **etcd management**: Need at least 3 etcd nodes for HA — adds deployment burden.
- **Rolling upgrades**: Shards must be drained before node replacement — requires graceful shutdown protocol.
- **Mitigation**: Use `tracing` with span propagation for distributed trace IDs; document operational procedures in `ops/`.

### Performance Risks
- **Scatter-gather latency**: Every query waits for the slowest shard. Use per-shard timeout with best-effort semantics.
- **Rebalancing cost**: Moving vectors between shards while serving reads/writes is non-trivial. MVP should support only manual rebalance with drain-and-rejoin.

---

## Delivery Forecast

### Estimated Lines of Code

| Component | Estimated lines | Description |
|-----------|----------------|-------------|
| `tesseract-common` — consistent hash, shard types, network errors | ~200 | New types + utilities |
| `tesseract-storage` — shard awareness, replication endpoint | ~300 | Config + streaming replication interface |
| `tesseract-coordinator` — etcd client, query router, scatter-gather | ~800 | New crate |
| `tesseract-api` — `--mode` flag, coordinator client | ~400 | Refactor main binary |
| `tesseract-vql` — coordinator-aware executor | ~200 | Pass coordinator mode through executor |
| **Total** | **~1,900 lines** | |

### PR Split

Given the 400-line review budget, this needs **chained PRs**:

| PR | Focus | Est. lines | Dependencies |
|----|-------|-----------|-------------|
| **PR #1** | `tesseract-common` new types + jump hash + `tesseract-storage` shard-aware config | ~250 | None |
| **PR #2** | `tesseract-coordinator` crate skeleton + etcd integration + membership | ~450 | PR #1 |
| **PR #3** | Coordinator: scatter-gather query execution + merge | ~500 | PR #2 |
| **PR #4** | `tesseract-api` split modes + `tesseract-vql` coordinator executor | ~400 | PR #3 |
| **PR #5** | Async replication (WAL tailing) + failover | ~500 | PR #4 |
| **PR #6** | Rebalancing (manual drain/join) | ~300 | PR #5 |

Total: **6 chained PRs** recommended. The review budget risk is **High** — this is the largest phase of the project by far.

---

## Key Decisions Summary

| Decision | MVP Choice | Future Direction |
|----------|-----------|-----------------|
| **Shard key** | Hash-based (jump hash on `VectorId`) | Metadata-based routing for tenant isolation |
| **Shard count** | Fixed (e.g., 16 or 256 virtual nodes) | Dynamic with rebalancing |
| **Replication factor** | 1 (no replication MVP) → 3 (async leader-follower) | Raft per shard for strong consistency |
| **Consensus** | etcd (external) for metadata | Embedded Raft if external etcd becomes ops burden |
| **Query routing** | Proxy-based coordinator | Client-side routing with smart client library |
| **Rebalancing** | Manual (drain + rejoin) | Automatic with consistent-hash virtual node migration |
| **Cross-shard search** | Exact scatter-gather | Approximate two-phase with centroid routing |
| **Consistency (data)** | Eventual | Read-your-writes with session stickiness |
| **Consistency (metadata)** | Linearizable (via etcd) | Linearizable (always) |

---

## Ready for Proposal

**Yes.** This exploration covers all subsystems with clear MVP-first choices. The proposal phase should formalize the scope, define the coordinator contract, and produce a rollback plan for the two most critical subsystems (shard routing and scatter-gather).
