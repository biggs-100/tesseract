# Tier Lifecycle Specification

## Purpose

The tier lifecycle manager monitors access patterns on cold data and orchestrates promotion (cold → hot) and demotion (hot → cold) of vectors as a background async task. Its goal is to keep frequently accessed data in the fast in-memory tier while offloading cold data to compressed Parquet storage — all without blocking query execution.

## Requirements

### Requirement: Access Frequency Monitoring

The lifecycle manager MUST monitor access frequency on cold-tier data. For each partition, it MUST track the number of reads within a configurable time window.

#### Scenario: Access count recorded on cold read

- GIVEN a cold partition P1 that has not been accessed
- WHEN a query reads data from P1
- THEN the access counter for P1 is incremented

#### Scenario: Access count decays over time window

- GIVEN a cold partition P1 with 100 recorded accesses in the current window
- WHEN the time window expires and no new accesses occur
- THEN the access counter resets or decays according to the configured policy

### Requirement: Promotion of Frequently Accessed Cold Data

The lifecycle manager SHOULD promote frequently accessed cold partitions to the hot tier. A partition MUST be promoted when its access frequency exceeds a configurable promotion threshold.

#### Scenario: Cold partition promoted when threshold exceeded

- GIVEN a cold partition P1 and a promotion threshold of 50 accesses per minute
- WHEN P1 accumulates 60 accesses within one minute
- THEN the lifecycle manager schedules P1 for promotion
- AND P1's vectors are loaded into the hot tier
- AND the partition is marked as promoted in the access tracker

#### Scenario: Partition not promoted below threshold

- GIVEN a cold partition P2 with 10 accesses per minute against a threshold of 50
- WHEN the lifecycle evaluation cycle runs
- THEN P2 is NOT promoted
- AND it remains in the cold tier

### Requirement: Demotion of Stale Hot Data

The lifecycle manager SHOULD demote hot-tier vectors that are no longer actively accessed to the cold tier. A vector MUST be demoted when its access frequency falls below a configurable demotion threshold.

#### Scenario: Hot data demoted after access drops

- GIVEN a hot-tier vector V1 that was previously accessed 100 times/hour and is now accessed 2 times/hour
- WHEN the demotion threshold is 5 accesses/hour
- THEN V1 is scheduled for demotion
- AND V1's data is flushed to the cold tier
- AND V1 is removed from the hot tier

#### Scenario: Frequently accessed hot data stays promoted

- GIVEN a hot-tier vector V2 with 200 accesses/hour against a demotion threshold of 5
- WHEN the lifecycle evaluation cycle runs
- THEN V2 is NOT demoted
- AND it remains in the hot tier

### Requirement: Non-Blocking Background Execution

The lifecycle manager MUST run as a background async task. Promotion and demotion operations MUST NOT block concurrent query execution on either tier.

#### Scenario: Query completes during lifecycle operation

- GIVEN a lifecycle manager mid-promotion of a large partition
- WHEN a concurrent query targets an unrelated vector in the hot tier
- THEN the query completes within normal latency bounds (no blocking)
- AND the promotion completes successfully

#### Scenario: Multiple lifecycle cycles run concurrently

- GIVEN a lifecycle manager that runs every 60 seconds
- WHEN two cycles run back-to-back
- THEN promotion and demotion decisions in the second cycle reflect the state AFTER the first cycle completed
