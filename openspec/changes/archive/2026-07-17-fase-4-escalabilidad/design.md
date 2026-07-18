# Design: Phase 4 — Scalability

## Technical Approach

Add horizontal scalability by introducing a `tesseract-cluster` binary that wraps the existing `StorageEngine` with JumpHash sharding, etcd-based coordination, async WAL replication, and HTTP scatter-gather query execution. The proposal's coordinator+data mode split becomes a single unified binary (`tesseract-cluster`) where every node is both a data node and a query coordinator.

## Architecture Decisions

| Option | Tradeoff | Decision |
|--------|----------|----------|
| **JumpHash** vs Ring Hash | JumpHash: O(1), minimal redistribution. Ring hash: O(log n), higher rebalance cost. | **JumpHash** — 50 lines, deterministic, perfect for fixed 64 shards |
| **etcd** vs embedded Raft | etcd: external dep, battle-tested, watch API. Embedded Raft: no dep, more binary complexity. | **etcd** — best Rust client ecosystem, watch API for shard map changes |
| **Async WAL replication** vs sync quorum | Async: lower latency, eventual consistent. Sync: stronger durability, higher latency. | **Async** — vector search is approximate, replica lag is acceptable for MVP |
| **Scatter-gather** vs approximate routing | Scatter-gather: every query hits every shard, exact merge. Approximate: fewer shards, may miss results. | **Scatter-gather** — works with hash sharding, no algorithmic change to HNSW |
| **Unified binary** vs coordinator+data split | Unified: any node can coordinate, no single point of failure. Split: cleaner resource isolation. | **Unified binary** — `tesseract-cluster` with `--mode data\|coordinator\|both` |
| **64 fixed shards** vs dynamic | Fixed: simple routing, no rebalance complexity. Dynamic: adapts to cluster size. | **64 fixed** — balances granularity with overhead, operator can rebalance manually |

## Data Flow

```
INSERT:
  Client ──POST /insert──→ Any node (coordinator role)
    │
    ├── jump_hash(VectorId, 64) → shard_id
    ├── etcd lookup → shard leader address
    │
    ├── Local node is leader? → StorageEngine::insert()
    │                            └── WAL append → async replicate to followers
    └── Remote leader? → HTTP forward → leader inserts
                                         └── WAL append → async replicate

SEARCH:
  Client ──POST /query──→ Any node (coordinator role)
    │
    ├── Get all shard leaders from etcd
    ├── Fan out query to ALL leaders (concurrent HTTP, per-shard timeout)
    │
    ├── Each leader: StorageEngine::search() → top-k local results
    │
    └── Coordinator merges, deduplicates by VectorId, returns global top-K
        └── Partial results if any shard timed out (partial: true flag)
```

## File Changes

| File | Action | Description |
|------|--------|-------------|
| `Cargo.toml` | Modify | Add `tesseract-cluster` to workspace members |
| `tesseract-cluster/Cargo.toml` | Create | New crate deps: etcd-client, reqwest, tokio, clap |
| `tesseract-cluster/src/main.rs` | Create | CLI with `--mode`, `--etcd-endpoints`, data dir config |
| `tesseract-cluster/src/cluster.rs` | Create | `ClusterNode` — wires etcd, shard manager, API, replication |
| `tesseract-cluster/src/shard_manager.rs` | Create | `jump_hash()`, shard assignment read/write via etcd |
| `tesseract-cluster/src/replication.rs` | Create | HTTP-based WAL entry streaming, promotion logic |
| `tesseract-cluster/src/discovery.rs` | Create | etcd lease-based registration, heartbeat, node list watch |
| `tesseract-cluster/src/api.rs` | Create | Axum router: `/cluster/nodes`, `/shard-assignment`, `/rebalance`, `/health` |
| `tesseract-common/src/error.rs` | Modify | Add `ShardNotAssigned`, `AllShardsFailed`, `NodeConflict` variants |

## Interfaces / Contracts

```rust
// cluster.rs — core orchestration
pub struct ClusterNode {
    node_id: String,
    addr: SocketAddr,
    storage: Arc<StorageEngine>,
    shard_manager: ShardManager,
    replication: ReplicationEngine,
    etcd_client: etcd_client::Client,
}

// shard_manager.rs
pub fn jump_hash(key: u64, num_buckets: u64) -> u64;
// jump_hash uses the Google JumpHash algorithm:
//   key → (key * 2862933555777941757 + 1) mod 2^64
//          scaled to [0, num_buckets) via exponential dropping

// Cross-node HTTP API (internal)
// POST /shard/{shard_id}/insert — forward WAL entries for replication
// POST /shard/{shard_id}/search — execute search on local HNSW
// GET  /shard/{shard_id}/health — liveness check

// WAL entry reuse: WalEntry::to_bytes() / from_bytes() for on-wire format
```

## Cross-node HTTP Contract

| Endpoint | Direction | Payload | Response |
|----------|-----------|---------|----------|
| `POST /internal/insert` | Coordinator → leader | `{ id, vector, metadata, mode }` | `{ txn_id }` or error |
| `POST /internal/search` | Coordinator → shard | `{ query, ef, mask? }` | `{ results: [{id,score}], partial }` |
| `POST /internal/replicate` | Leader → follower | WAL entry bytes | `{ acked_lsn }` |
| `GET /internal/health` | Any → node | — | `{ is_leader: true, shards: [...] }` |

Internal routes mounted at `/internal/*` on every node, separate from the external `/cluster/*` management API.

## Threat Matrix

N/A — this change does not touch shell commands, subprocesses, VCS/PR automation, executable-file classification, or user-facing URL routing. Internal node-to-node HTTP forwarding uses fixed IPs/ports from etcd membership, not user-supplied URL input.

## Testing Strategy

| Layer | What to Test | Approach |
|-------|-------------|----------|
| Unit | JumpHash uniformity | 10K random VectorIds → verify distribution uniform (chi-squared) |
| Unit | Shard assignment read/write | Mock etcd client, verify roundtrip |
| Integration | Two-node cluster | Start 2 `tesseract-cluster` + embedded etcd, insert on A, query on B |
| Integration | Scatter-gather merge | 2 shards with data, verify merged result correctness + dedup |
| Integration | Leader failover | Kill leader process, wait for re-election, verify new leader serves |
| Integration | Replication lag | Insert 100 entries, verify follower LSN catches up |

## Migration / Rollout

Single-node migration: existing `tesseract-api` deployment stays unchanged. New `tesseract-cluster` binary runs alongside for testing. Operator:
1. Deploy etcd cluster (3 nodes)
2. Start first `tesseract-cluster` node with all 64 shards assigned
3. Verify existing query patterns work through cluster API
4. Add second node, rebalance 32 shards to it manually
5. Decommission old `tesseract-api`

No data migration needed — existing WAL + storage directories mount directly.

## Open Questions

- [ ] Cross-node request tracing: `tracing` span propagation via HTTP headers (requires `tracing-opentelemetry` or manual trace-id header)?
- [ ] Replication backpressure: what happens when follower queue hits max (10K entries)? Drop oldest or block?
- [ ] Snapshot transfer API: for large-gap catchup, need `GET /internal/snapshot/{shard_id}` endpoint details
