# Distributed Query Specification

## Purpose

Define how the coordinator executes scatter-gather queries across all shard leaders, merges top-k results, and handles partial failures.

## Requirements

### Requirement: Scatter-Gather Execution

On a SEARCH query, the coordinator MUST fan out the query to all shard leaders and gather their individual top-k results.

#### Scenario: Full scatter-gather

- GIVEN a cluster with 3 active shard leaders
- WHEN a SEARCH query arrives at the coordinator
- THEN the coordinator sends the query to all 3 leaders concurrently
- AND each leader returns its top-k local results
- AND the coordinator merges all results into a single sorted list

#### Scenario: Single-shard cluster

- GIVEN a cluster with only one shard leader
- WHEN a SEARCH query arrives
- THEN the coordinator sends the query to the sole leader
- AND returns results directly without merging

### Requirement: Merged Result Set

The coordinator MUST merge results from all shards, deduplicate by VectorId, and return a global top-K sorted by descending score.

#### Scenario: Merge with dedup

- GIVEN shard A returns `[(v1, 0.9), (v2, 0.8)]` and shard B returns `[(v3, 0.95), (v1, 0.9)]`
- WHEN the coordinator merges
- THEN the result is `[(v3, 0.95), (v1, 0.9), (v2, 0.8)]` — v1 appears once
- AND the result length equals `min(global_top_k, distinct_results)`

### Requirement: Partial Failure Handling

The coordinator MUST handle per-shard timeouts and return partial results with a `partial: true` flag when some shards fail.

#### Scenario: One shard times out

- GIVEN a query with per-shard timeout of 500ms
- WHEN shard A responds in 300ms but shard B does not respond within 500ms
- THEN the coordinator collects results from shard A only
- AND returns `{ results: [...], partial: true, failed_shards: ["shard-B"] }`

#### Scenario: All shards fail

- GIVEN all 3 shard leaders are unreachable
- WHEN a SEARCH query is executed
- THEN the coordinator returns an error `AllShardsFailed` after the timeout
- AND does NOT return partial results

### Requirement: Latency Budget

The coordinator MUST accept a `WITHIN <duration>` clause in SEARCH queries and prorate the budget across shards.

#### Scenario: Prorated timeout

- GIVEN a SEARCH query with `WITHIN 2s` and 4 shards
- WHEN the coordinator fans out the query
- THEN each shard gets a timeout of 500ms (2000ms / 4)
- AND if a shard exceeds its prorated budget, it is treated as failed

#### Scenario: Budget floor

- GIVEN a query with `WITHIN 100ms` and 10 shards
- WHEN the prorated timeout per shard would be 10ms
- THEN the coordinator uses a minimum per-shard timeout of 50ms
- AND adjusts the overall timeout accordingly

### Requirement: Routing Cache

The coordinator SHOULD cache shard-to-node routing for hot queries to avoid etcd reads on every request.

#### Scenario: Cache hit

- GIVEN the coordinator has cached shard routes with TTL of 5s
- WHEN a SEARCH query arrives
- THEN the coordinator reads shard routes from cache instead of etcd
- AND query latency is reduced by the etcd round-trip

#### Scenario: Stale cache

- GIVEN a rebalance occurred and the cache is stale
- WHEN the coordinator sends a query to an old node address
- THEN the node responds with `ShardNotHere` redirect
- AND the coordinator fetches fresh routes from etcd
- AND retries the query against the correct node
