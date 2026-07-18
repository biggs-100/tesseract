# Vector Skeleton Specification

## Purpose

The vector skeleton stores a lightweight compressed centroid per cold partition in RAM. It enables fast distance-based pruning: a query vector is compared against skeleton centroids before deciding whether to wake a cold partition. The skeleton must be cheap enough (< 1 KB per partition) to keep thousands of partition centroids in memory simultaneously.

## Requirements

### Requirement: Compressed Centroid Per Partition

The skeleton MUST store one compressed centroid per cold partition in RAM. The centroid MUST be a `Vec<f32>` computed as the element-wise mean of all vectors in that partition.

#### Scenario: Centroid computed from partition vectors

- GIVEN a cold partition containing vectors `v1 = [1.0, 2.0]` and `v2 = [3.0, 4.0]`
- WHEN the skeleton centroid is computed for this partition
- THEN the centroid is `[2.0, 3.0]` — the element-wise mean

#### Scenario: Centroid updated after partition flush

- GIVEN a hot tier that accumulates vectors and flushes them to a new cold partition
- WHEN the flush completes
- THEN a new skeleton entry is created for that partition with the correct centroid
- AND the entry is added to the in-memory skeleton index

### Requirement: Distance Comparison

The skeleton MUST support comparing a query vector against any stored centroid, returning a similarity or distance score that callers can use for partition ranking.

#### Scenario: Query compared against all centroids

- GIVEN a skeleton with centroids for partitions P1, P2, P3
- WHEN a query vector is compared against all three centroids
- THEN three distance scores are returned
- AND the scores correctly reflect proximity (closer vectors get higher scores)

### Requirement: Configurable Partition Wake Threshold

The skeleton SHOULD trigger partition wake (load the partition from cold storage into memory) when a query vector's distance to the centroid is below a configurable threshold.

#### Scenario: Partition woken when query is close enough

- GIVEN a skeleton with threshold 0.15 and a partition P1 whose centroid is at distance 0.10 from query Q
- WHEN Q is compared against the skeleton
- THEN P1 is scheduled for wake
- AND the partition begins loading from cold tier

#### Scenario: Partition not woken when query is far

- GIVEN a skeleton with threshold 0.15 and a partition P2 whose centroid is at distance 0.80 from query Q
- WHEN Q is compared against the skeleton
- THEN P2 is NOT scheduled for wake

### Requirement: Skeleton Memory Budget

Each skeleton entry MUST be less than 1 KB in memory. The skeleton data structure MUST support at least 10,000 partition entries without exceeding 10 MB total.

#### Scenario: Skeleton entry size within budget

- GIVEN a partition centroid with 1536 dimensions (typical embedding size)
- WHEN the skeleton entry is measured
- THEN the entry size (centroid Vec<f32> + partition ID + overhead) is under 1024 bytes

#### Scenario: Large number of partitions fits in memory

- GIVEN a skeleton with 10,000 partition entries
- WHEN memory usage is measured
- THEN total memory consumption is below 10 MB
