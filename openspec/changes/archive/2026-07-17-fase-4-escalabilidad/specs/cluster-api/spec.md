# Cluster API Specification

## Purpose

Define the HTTP endpoints for cluster management and observability.

## Requirements

### Requirement: Node Listing

The system MUST provide `GET /cluster/nodes` returning all registered nodes with their current status.

#### Scenario: List healthy cluster

- GIVEN a cluster with 3 active nodes
- WHEN a client calls `GET /cluster/nodes`
- THEN the response contains all 3 nodes with `status: healthy`
- AND each entry includes `node_id`, `address`, `mode` (data|coordinator), and `shard_count`

#### Scenario: List with failed node

- GIVEN a cluster where 1 of 3 nodes has missed its heartbeat
- WHEN a client calls `GET /cluster/nodes`
- THEN the failed node appears with `status: unhealthy`
- AND `last_seen` timestamp is included

### Requirement: Shard Assignment View

The system MUST provide `GET /cluster/shard-assignment` returning the current shard-to-node mapping.

#### Scenario: Full shard map

- GIVEN 64 shards distributed across 4 data nodes
- WHEN a client calls `GET /cluster/shard-assignment`
- THEN the response is a map of shard_id → { node_id, state: primary|replica|moving }
- AND every shard (0–63) has an entry

#### Scenario: Partial assignment

- GIVEN only 32 of 64 shards are assigned
- WHEN a client calls `GET /cluster/shard-assignment`
- THEN unassigned shards appear with `state: unassigned`
- AND no `node_id` field

### Requirement: Rebalance Trigger

The system MUST provide `POST /cluster/rebalance` accepting `{ shard_id, target_node }` to trigger a manual shard move.

#### Scenario: Valid rebalance

- GIVEN shard `S5` on `node-a`
- WHEN a client calls `POST /cluster/rebalance` with `{ shard_id: "S5", target_node: "node-b" }`
- THEN the response is `202 Accepted` with a `job_id`
- AND the rebalance proceeds asynchronously

#### Scenario: Invalid rebalance

- GIVEN a non-existent shard `S99`
- WHEN a client calls `POST /cluster/rebalance` with `{ shard_id: "S99", target_node: "node-b" }`
- THEN the response is `404 Not Found` with `{ error: "ShardNotFound" }`

### Requirement: Cluster Health

The system MUST provide `GET /cluster/health` returning per-node health status including connectivity and lag.

#### Scenario: All healthy

- GIVEN a fully operational cluster
- WHEN a client calls `GET /cluster/health`
- THEN the response includes `status: healthy` for the cluster
- AND per-node entries with `connected: true`, `replication_lag: 0`

#### Scenario: Degraded health

- GIVEN a follower with replication lag of 500 entries
- WHEN a client calls `GET /cluster/health`
- THEN the cluster-level status is `degraded`
- AND the lagging node includes `replication_lag: 500`, `status: lagging`

### Requirement: Metrics Export

The system SHOULD expose per-endpoint and per-node metrics including replication lag and query latency.

#### Scenario: Metrics query

- GIVEN a cluster with active query traffic
- WHEN a client calls `GET /cluster/health` with `?metrics=true`
- THEN the response includes `replication_lag_avg`, `query_latency_p50`, `query_latency_p99` per node
- AND a `cluster_total_queries` counter
