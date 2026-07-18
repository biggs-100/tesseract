# Replication Engine Specification

## Purpose

Define how shard data is replicated asynchronously from leader to followers using the existing WAL format, ensuring durability and promotability.

## Requirements

### Requirement: Async Leader-Follower Replication

Each shard leader MUST write every mutation to its local WAL and then stream the WAL entry to all configured followers asynchronously.

#### Scenario: Successful replication

- GIVEN a shard with 1 leader and 2 followers
- WHEN the leader processes an INSERT
- THEN the entry is written to the leader's local WAL
- AND the leader sends the WAL entry to both followers
- AND followers apply the entry to their local store

#### Scenario: Follower lag

- GIVEN a follower is slow to acknowledge
- WHEN the leader continues accepting writes
- THEN the leader queues unacknowledged entries (up to `max_replication_queue: 10000`)
- AND does NOT block writes on the hot path

### Requirement: WAL Format for Replication

The system MUST use the existing WAL entry format (see `wal-engine` spec) for replication transfers. No separate replication protocol format is needed.

#### Scenario: Entry transfer

- GIVEN a WAL entry with LSN, operation type, and payload
- WHEN the leader sends it to followers
- THEN the serialized bytes match the on-disk WAL format exactly
- AND the follower deserializes using the same WAL reader

### Requirement: Follower Acknowledgment

A leader MUST NOT mark an entry as `replicated` until all followers acknowledge receipt.

#### Scenario: Full acknowledgment

- GIVEN a leader with 2 followers
- WHEN the leader sends entry LSN=42 to both
- THEN both followers acknowledge receipt after applying
- AND the leader updates the replication watermark to LSN=42

#### Scenario: Partial acknowledgment

- GIVEN follower A acknowledges but follower B does not
- WHEN the leader waits for `replication_timeout` (default 2s)
- THEN the leader marks the entry as `partially_replicated`
- AND continues processing new writes

### Requirement: Follower Promotability

Any follower that has applied all committed entries up to the leader's last known LSN MUST be promotable to leader.

#### Scenario: Promote follower

- GIVEN the leader of shard `S1` has failed
- WHEN a follower has replication lag < configured `max_promotion_lag`
- THEN the follower can be promoted to leader via etcd election
- AND it starts accepting writes for the shard

#### Scenario: Promote with lag

- GIVEN a follower with lag exceeding `max_promotion_lag`
- WHEN the leader fails
- THEN the follower SHALL NOT campaign for leadership
- AND it must catch up via replication from another source first

### Requirement: Replication Lag Monitoring

Replication lag MUST be monitored per shard per follower and exposed via metrics.

#### Scenario: Lag reporting

- GIVEN a shard with 1 leader and 2 followers
- WHEN the leader records the current LSN and the followers' last acknowledged LSNs
- THEN the lag is reported as `leader_lsn - follower_lsn`
- AND the metric is exported every `metrics_interval` (default 10s)

### Requirement: Follower Catch-Up

After a disconnection, a follower MUST be able to catch up by replaying WAL entries it missed.

#### Scenario: Catch-up after reconnect

- GIVEN a follower disconnected at LSN=50 and the leader is now at LSN=200
- WHEN the follower reconnects
- THEN the leader streams entries from LSN=51 onward
- AND the follower replays them in order
- AND once the follower reaches LSN=200, it is fully caught up

#### Scenario: Large gap catch-up

- GIVEN a follower was disconnected for 1 hour and missed 500K entries
- WHEN the follower reconnects
- THEN the leader initiates a full snapshot transfer (compacted WAL segment)
- AND the follower truncates its local store and applies the snapshot
- THEN subsequent WAL streaming resumes from the snapshot LSN
