# Proposal: Phase 4 — Scalability

## Intent

Tesseract is single-node. As data grows, it must scale horizontally. This phase adds sharding, replication, and distributed query execution while preserving the existing storage and indexing architecture.

## Scope

### In Scope
- JumpHash shard routing on VectorId (64 shards)
- etcd-based cluster membership + leader election + config
- New `tesseract-coordinator` crate for scatter-gather query
- Async WAL replication (1 leader, 2 followers per shard)
- Cluster management HTTP API
- Manual rebalance (operator drain/join command)

### Out of Scope
- Cross-shard transactions / distributed SQL
- Online schema changes
- Multi-datacenter replication
- Auto-scaling
- Embedded Raft (future phase)

## Capabilities

### New Capabilities
- `cluster-coordination`: etcd-based membership, leader election, health checking
- `shard-manager`: Shard allocation via JumpHash, routing table, rebalance orchestration
- `distributed-query`: Scatter-gather execution across shards with top-k merge
- `replication-engine`: Async leader-follower WAL tailing per shard
- `cluster-api`: HTTP endpoints for cluster status, rebalance, node management

### Modified Capabilities
- `http-api`: Extended with cluster management routes for admin operations.

## Approach

New `tesseract-coordinator` crate acts as stateless proxy: routes INSERT by hash to shard leader, broadcasts SEARCH to all leaders, merges top-k. Shard leaders own WAL + index locally. etcd stores shard map + membership (linearizable). Replication via async WAL stream to followers. Single binary with `--mode coordinator|data`. Consistency: eventual for vectors, linearizable for metadata (etcd).

## Affected Areas

| Area | Impact | Description |
|------|--------|-------------|
| `tesseract-coordinator/` | New | etcd client, query router, merge, health |
| `tesseract-common/` | Modified | ShardId, network errors, jump hash |
| `tesseract-storage/` | Modified | Shard config, WAL replication export |
| `tesseract-api/` | Modified | --mode flag, coordinator client routes |
| `tesseract-vql/` | Modified | Coordinator-aware executor path |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Network partition | Med | Quorum check (N/2+1), partial results flag |
| Stale shard map | Low | etcd revision check before every write |
| Ops complexity | High | tracing spans, ops docs, graceful drain |
| Scatter-gather latency | Med | Per-shard timeout, best-effort fallback |

## Rollback Plan

Deploy single-node in `--mode data` without coordinator — bypasses all distributed logic. Coordinator is stateless; if it fails, route directly to any shard node. etcd shutdown forces single-node fallback.

## Dependencies

- **External**: etcd v3.5+ cluster (3 nodes minimum)
- **Crates**: `etcd-client`, `reqwest`, `tokio` (existing)

## Success Criteria

- [ ] Two-node cluster discovers each other via etcd
- [ ] INSERT on one node is replicated and queryable on follower
- [ ] Scatter-gather across 2+ shards returns correct merged results
- [ ] Leader failure → follower promoted → queries continue
- [ ] New node joins → manual rebalance moves data correctly
