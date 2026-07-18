# Cluster Coordination Specification

## Purpose

Define how Tesseract nodes discover each other, maintain membership, elect shard leaders, and detect failures via etcd.

## Requirements

### Requirement: Node Registration

Every node MUST register itself in etcd on startup with a unique node ID, address, and set of owned shard IDs.

#### Scenario: Node joins cluster

- GIVEN a running etcd cluster with at least 3 members
- WHEN a Tesseract node starts in `--mode data` or `--mode coordinator`
- THEN it creates a lease-based key at `/tesseract/nodes/{node_id}` with its address and shard list
- AND the lease TTL is set to the configured heartbeat interval (default 5s)

#### Scenario: Duplicate node ID

- GIVEN a node with ID `node-a` is already registered
- WHEN another node attempts to register as `node-a`
- THEN the registration fails with a conflict error
- AND the node SHALL log the collision and exit

### Requirement: Leader Election per Shard

The system MUST elect exactly one leader per shard using etcd elections. Leaders own write authority for their shard.

#### Scenario: First node claims shard leadership

- GIVEN shard `S1` has no leader in etcd
- WHEN a node holding `S1` calls `etcd election campaign` for key `/tesseract/leader/S1`
- THEN the node becomes leader of `S1` and writes its node ID to the key

#### Scenario: Leader re-election after failure

- GIVEN the leader of shard `S1` has disconnected and its lease expired
- WHEN a follower observes the leader key `/tesseract/leader/S1` is vacant
- THEN it campaigns for leadership with a randomized delay (5-15s) to avoid thundering herd
- AND exactly one follower becomes the new leader

### Requirement: Heartbeat

Each node MUST maintain a TTL-based heartbeat in etcd that is refreshed periodically. Expiry implies node failure.

#### Scenario: Heartbeat refresh

- GIVEN a node is registered with a 5-second lease
- WHEN the node refreshes its lease before TTL expiry
- THEN the lease TTL resets and the node remains healthy

#### Scenario: Heartbeat expiry

- GIVEN a node has stopped refreshing its lease
- WHEN the lease TTL expires
- THEN etcd deletes the node's registration key
- AND other nodes detect the removal on the next membership poll

### Requirement: Configurable Failure Detection

The system MUST allow operators to configure heartbeat interval and failure timeout. The cluster SHALL NOT mark a node as failed before the timeout elapses.

#### Scenario: Configure timeout

- GIVEN a cluster with `heartbeat_interval: 5s` and `failure_timeout: 15s`
- WHEN a node stops heartbeating
- THEN the node is NOT considered failed until 15s have passed (3 missed heartbeats)

### Requirement: Graceful Shutdown

A node SHOULD deregister itself from etcd before shutting down, including resigning from any leadership positions.

#### Scenario: Graceful shutdown

- GIVEN a healthy leader node
- WHEN the operator sends SIGTERM
- THEN the node resigns all shard leaderships in etcd
- AND deletes its registration key
- AND waits up to `drain_timeout` (default 30s) for in-flight operations to complete
- THEN exits with code 0

#### Scenario: Forced shutdown

- GIVEN a leader node
- WHEN the node crashes without deregistering
- THEN the lease expires naturally
- AND followers detect the vacancy and trigger re-election
