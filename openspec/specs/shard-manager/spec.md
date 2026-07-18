# Shard Manager Specification

## Purpose

Define how the system assigns vectors to shards, maintains the shard-to-node routing table in etcd, and supports manual rebalancing.

## Requirements

### Requirement: Vector-to-Shard Mapping

The system MUST assign every vector to exactly one of 64 shards using `JumpHash(VectorId) % 64`.

#### Scenario: Deterministic shard assignment

- GIVEN vectors V1 and V2 with distinct IDs
- WHEN `JumpHash(V1) % 64` and `JumpHash(V2) % 64` are computed
- THEN each vector maps to a consistent shard across all nodes
- AND adding or removing nodes does not change each vector's shard assignment

#### Scenario: All 64 shards reachable

- GIVEN a set of `JumpHash` outputs across the full key space
- WHEN mapping modulo 64
- THEN all 64 shard buckets (0–63) are populated

### Requirement: Shard-to-Node Mapping

The system MUST maintain the current shard-to-node assignment in etcd at `/tesseract/shard-map/{shard_id}`.

#### Scenario: Read shard assignment

- GIVEN shard `S1` is assigned to node `node-a`
- WHEN the coordinator reads `/tesseract/shard-map/S1`
- THEN it receives the node address for `node-a` as the owner

#### Scenario: Assignment change

- GIVEN shard `S1` is being rebalanced from `node-a` to `node-b`
- WHEN the shard manager updates `/tesseract/shard-map/S1`
- THEN all coordinators observe the change on next poll or via etcd watch

### Requirement: Manual Rebalance

The system MUST support moving a shard from one node to another via an operator command.

#### Scenario: Rebalance shard

- GIVEN shard `S1` on `node-a` with shard state `primary`
- WHEN the operator calls rebalance for `S1` with target `node-b`
- THEN shard state transitions to `moving`
- AND data is transferred from `node-a` to `node-b`
- AND on completion, the shard-to-node mapping updates to `node-b`
- AND shard state transitions to `primary`

#### Scenario: Rebalance rejects invalid target

- GIVEN shard `S1` is on `node-a`
- WHEN the operator calls rebalance targeting a non-existent node
- THEN the operation fails with `InvalidTarget`
- AND the shard state remains unchanged

### Requirement: Shard State Tracking

The system SHOULD track each shard's state: `primary`, `replica`, or `moving`.

#### Scenario: State lifecycle

- GIVEN shard `S1` is in state `primary` on `node-a`
- WHEN a rebalance begins
- THEN state becomes `moving`
- AND after data transfer completes and mapping updates
- THEN state becomes `primary` on `node-b`
- AND the old copy on `node-a` becomes `replica`

### Requirement: Unassigned Shard Rejection

The system MUST reject any read or write operation targeting a shard that has no node assignment in etcd.

#### Scenario: Write to unassigned shard

- GIVEN shard `S7` has no entry in `/tesseract/shard-map/S7`
- WHEN a coordinator receives an INSERT for a vector mapped to `S7`
- THEN the operation fails with `ShardNotAssigned` error

#### Scenario: Read from unassigned shard

- GIVEN shard `S7` is unassigned
- WHEN a SEARCH query would route to `S7`
- THEN the query returns an error for that shard
- AND partial results from other shards SHALL include a `partial: true` flag
