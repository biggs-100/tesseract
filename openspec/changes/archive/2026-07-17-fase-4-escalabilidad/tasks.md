# Tasks: Phase 4 — Scalability

## Review Workload Forecast

| Field | Value |
|-------|-------|
| Estimated changed lines | ~1900 across 6 stacked PRs |
| 800-line budget risk | Low |
| Chained PRs recommended | Yes |
| Suggested split | PR 1 (Shard) → PR 2 (Discovery) → PR 3 (Replication) → PR 4 (Query) → PR 5 (Failover) → PR 6 (API+BIN) |
| Delivery strategy | auto-chain |
| Chain strategy | stacked-to-main |

Decision needed before apply: No
Chained PRs recommended: Yes
Chain strategy: stacked-to-main
800-line budget risk: Low

### Suggested Work Units

| Unit | Goal | Likely PR | Focused test command | Runtime harness | Rollback boundary |
|------|------|-----------|----------------------|-----------------|-------------------|
| 1 | JumpHash routing + shard assignment in etcd | PR 1 | `cargo test -p tesseract-cluster shard_manager` | 2-node cluster + embedded etcd | Remove tesseract-cluster/ crate, restore Cargo.toml |
| 2 | etcd node registration + heartbeat | PR 2 | `cargo test -p tesseract-cluster discovery` | same harness | Revert discovery.rs, remove etcd registration calls |
| 3 | Async WAL replication + catch-up | PR 3 | `cargo test -p tesseract-cluster replication` | 3-node cluster (leader + 2 followers) | Revert replication.rs, remove /internal/replicate routes |
| 4 | Scatter-gather query + merge | PR 4 | `cargo test -p tesseract-cluster distributed_query` | same harness | Revert /internal/search and /internal/insert routes, scatter-gather logic |
| 5 | etcd leader election + failover | PR 5 | `cargo test -p tesseract-cluster failover` | 3-node cluster, kill leader | Revert election campaign/observe, remove /tesseract/leader/ keys |
| 6 | Cluster HTTP API + tesseract-cluster binary | PR 6 | `cargo test -p tesseract-cluster --integration` | full cluster binary | Revert main.rs, api.rs, tesseract-cluster/Cargo.toml |

## Phase 1: Shard Manager (PR 1)

- [x] 1.1 Modify `Cargo.toml` — add `tesseract-cluster` workspace member
- [x] 1.2 Create `tesseract-cluster/src/jump_hash.rs` + `shard_manager.rs` — `jump_hash()` using Google JumpHash algorithm
- [x] 1.3 Add `ShardManager` — in-memory shard assignment with JSON serialization for etcd (ShardState enum deferred to rebalance PR)
- [x] 1.4 Add unassigned shard rejection — `ShardNotAssigned` returned by `add_replica` on unassigned shards; API-level rejection deferred to query PRs
- [x] 1.5 Modify `tesseract-common/src/error.rs` — add `ShardNotAssigned`, `AllShardsFailed`, `NodeConflict`
- [x] 1.6 Write unit tests: JumpHash distribution (100K keys, chi-squared), shard assignment roundtrip, 20 tests total

## Phase 2: Discovery & Heartbeat (PR 2)

- [x] 2.1 Create `tesseract-cluster/src/discovery.rs` — in-memory `NodeRegistry` with register, heartbeat, status transitions; create `cluster.rs` with `ClusterState` (combines NodeRegistry + ShardManager); create `etcd_discovery.rs` behind `#[cfg(feature = "etcd")]` with `EtcdDiscovery` (lease connect, register, heartbeat, watch)
- [x] 2.2 Add heartbeat refresh via `NodeRegistry::heartbeat()` (resets timestamp + status to Active) + timeout detection via `check_heartbeats()` (marks overdue nodes as Suspect); etcd feature: lease keepalive + prefix watch on `/tesseract/nodes/`
- [x] 2.3 Add duplicate node ID detection — `NodeRegistry::register()` returns `NodeConflict` if same node_id with different addr is already registered
- [x] 2.4 Add graceful shutdown — `ClusterState::leave()` removes local node from registry; etcd variant would deregister + resign leaderships (deferred to PR 5)
- [x] 2.5 Write tests: 15 unit tests — register/active, duplicate conflict, re-register, heartbeat refresh, timeout detection, mark_dead, active_nodes filter, remove_node cleanup, 3-node multi, heartbeat unknown error, mark_dead unknown error, check_heartbeats skips dead, multiple-node integration, ClusterState join/leave, join with shards, rejoin after leave

## Phase 3: Async Replication (PR 3)

- [x] 3.1 Create `tesseract-cluster/src/replication.rs` — `ReplicationEngine` with follower list per shard, replica state machine, pending queue, config
- [x] 3.2 Create `tesseract-cluster/src/replication_handler.rs` — `handle_replicate()` function + `StorageEngine::apply_replicated_entry()` in `tesseract-storage/src/engine.rs`
- [x] 3.3 Create `tesseract-cluster/src/replication_client.rs` — `ReplicationClient` (reqwest-based HTTP client) for async streaming to followers; non-blocking `record_entry()` hot path
- [x] 3.4 Add follower ack tracking, `replication_lag()`, `trim_acked()` (watermark cleanup), and replica state transitions (Synced → Lagging → Disconnected)
- [ ] 3.5 Add catch-up on reconnect — stream entries from missed LSN (data model ready via `pending_for_replica`; active streaming loop deferred)
- [ ] 3.6 Add large-gap snapshot transfer (`GET /internal/snapshot/{shard_id}`) for 500K+ entry gaps (deferred, design allows via config max_lag_entries ceiling)
- [x] 3.7 Add lag monitoring per shard per follower — `replication_lag()`, `replica_states()` methods exported on `ReplicationEngine`
- [x] 3.8 Add `max_lag_entries` → auto-`Disconnected` transition, blocking laggy followers from being considered synced (promotion gate data ready)
- [x] 3.9 Write 19 tests: entry roundtrip, ack, lag calc, trim_acked, pending filter, max_lag detection, reconnect, multi-replica independent ack, serde roundtrip for both `ReplicationEntry` and `ReplicaState`, `ReplicationResponse` serde

## Phase 4: Distributed Query (PR 4)

- [x] 4.1 Add `POST /internal/search` — `handle_remote_search()` handler + `RemoteSearchRequest`/`RemoteSearchResponse` types
- [ ] 4.2 Add `POST /internal/insert` — forward INSERT to shard leader (deferred to PR 6 Cluster API)
- [x] 4.3 Add scatter-gather coordinator — `QueryCoordinator` with concurrent fan-out to all shard leaders via `ClusterState::shards().assigned_leaders()`
- [x] 4.4 Add merge logic — `merge_results()` sorts by score descending, deduplicates by id, returns global top-K capped at limit
- [x] 4.5 Add `WITHIN <duration>` latency budget proration per shard with 50ms floor
- [x] 4.6 Add per-shard timeout → `partial: true` flag on `DistributedQueryResult`; returns `AllShardsFailed` if all shards fail
- [ ] 4.7 Add routing cache (5s TTL) with stale-cache retry via `ShardNotHere` redirect (deferred — spec says SHOULD, not MUST)
- [x] 4.8 Write tests: 17 unit tests — merge+dedup correctness, partial failure flag, timeout proration, serde roundtrips, AllShardsFailed error

## Phase 5: Leader Election & Failover (PR 5)

- [x] 5.1 Add `tesseract-cluster/src/leader_election.rs` — in-memory `LeaderElection` with `ElectionState` (Leader/Candidate/Follower/NoLeader), heartbeat timeout detection, per-shard leader tracking
- [x] 5.2 Add `tesseract-cluster/src/failover.rs` — `FailoverManager` with `FailoverConfig`, periodic failure detection via `start()` background task, `check_and_failover()`, `promote_to_leader()`, eligibility via replication lag
- [x] 5.3 Wire `LeaderElection` and `FailoverManager` into `ClusterState` as public fields
- [x] 5.4 Add follower promotion — verify `max_promotion_lag`, become candidate, win election, update shard manager
- [ ] 5.5 Add etcd campaign/observe (`/tesseract/leader/{shard_id}`) — first node claims leadership via etcd lease (deferred: in-memory fallback works without etcd)
- [ ] 5.6 Add graceful handoff — resign leadership on shutdown, notify followers (deferred: `ClusterState::leave` can call `my_leaderships()` on election when etcd-backed)
- [ ] 5.7 Add forced failover — lease expiry triggers re-election (deferred: etcd lease-based; in-memory `check_and_failover` provides failover via heartbeat timeout)
- [x] 5.8 Add `FailoverConfig` with `election_timeout_ms` (default 3s), `check_interval_ms` (500ms), `max_promotion_lag` (100 entries)
- [x] 5.9 Write 22 tests: become_candidate, win_election, set_leader → follower, heartbeat prevents timeout, timeout detection, my_leaderships/followerships, elected_count, win_election idempotent, win_election on non-candidate errors, no_heartbeat_is_timed_out, eligible with low/high lag, check_and_failover promotes/skips healthy/skips ineligible, promotion_candidates, promote_to_leader eligible/ineligible

## Phase 6: Cluster API & Binary (PR 6)

- [x] 6.1 Add axum, tower-http, clap deps to `Cargo.toml`
- [x] 6.2 Create `tesseract-cluster/src/main.rs` — CLI with `--node-id`, `--listen`, `--data-dir`, `--query-timeout-ms`
- [x] 6.3 Create `tesseract-cluster/src/cluster_node.rs` — `ClusterNode` struct wiring `ClusterState`, `QueryCoordinator`, HTTP server
- [x] 6.4 Create `tesseract-cluster/src/api.rs` — Axum router with `/cluster/*` management and `/internal/*` node-to-node endpoints
- [x] 6.5 Add `GET /cluster/nodes` — list all registered nodes via `NodeRegistry::all_nodes()`
- [x] 6.6 Add `GET /cluster/shard-assignment` — returns assigned shard-to-leader mapping
- [ ] 6.7 Add `POST /cluster/rebalance` — deferred: needs async job tracking and shard migration logic (not covered by delivery prompt scope)
- [x] 6.8 Add `GET /cluster/health` — per-node health summary with active_nodes, leaderships, followerships
- [x] 6.9 Add unit tests: 9 new tests (8 API handler tests + 1 ClusterNode identity test), verify merge correctness

## Phase 7: Verification

- [ ] 7.1 Run full test suite: `cargo test -p tesseract-cluster`
- [ ] 7.2 Verify threat-matrix scenarios: scatter-gather merge, leader failover, replication lag, partial results
- [ ] 7.3 Generate `openspec/changes/fase-4-escalabilidad/verify-report.md`
