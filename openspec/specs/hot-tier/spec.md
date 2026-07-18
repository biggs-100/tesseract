# Hot Tier Specification

## Purpose

The hot tier holds recently written or frequently accessed vectors and their metadata in memory. It serves as the primary write target (entries arrive via WAL) and the fastest read path for point lookups. When memory pressure reaches a configurable threshold, the hot tier flushes a batch to the cold tier.

## Requirements

### Requirement: In-Memory Vector and Metadata Store

The hot tier MUST store vectors (`Vec<f32>`) together with their associated metadata (key-value pairs) in memory. The data structures MUST support concurrent read and write access.

#### Scenario: Vector stored and retrieved from memory

- GIVEN an empty hot tier
- WHEN a vector with VectorId `v1` and metadata `{"label": "cat"}` is inserted
- THEN a point lookup for `v1` returns the vector and its metadata

#### Scenario: Concurrent readers do not block writers

- GIVEN a hot tier with 1000 stored vectors
- WHEN one task reads vectors while another task inserts a new vector
- THEN both operations complete without deadlock or data corruption
- AND the reader sees a consistent snapshot (either before or after the insert)

### Requirement: Point Lookup by VectorId

The hot tier MUST support point lookups by `VectorId` with O(1) expected time. Lookups for missing IDs MUST return a `NotFound` error.

#### Scenario: Lookup returns vector for existing ID

- GIVEN a hot tier containing vector `v42`
- WHEN a point lookup for `v42` is issued
- THEN the vector and its metadata are returned

#### Scenario: Lookup returns error for missing ID

- GIVEN a hot tier that does not contain `v99`
- WHEN a point lookup for `v99` is issued
- THEN a `NotFound` error is returned

### Requirement: Range Scan by Metadata Fields

The hot tier SHOULD support range scans over metadata fields. The scan MUST return all vectors whose metadata satisfies the predicate.

#### Scenario: Range scan returns matching vectors

- GIVEN a hot tier with vectors having metadata `{"score": 10}`, `{"score": 20}`, `{"score": 30}`
- WHEN a range scan is issued for `score > 15`
- THEN vectors with scores 20 and 30 are returned

### Requirement: Flush to Cold Tier on Memory Threshold

The hot tier MUST flush entries to the cold tier when memory usage reaches a configurable threshold (default: 80% of allocated hot tier capacity). The flush MUST select vectors for eviction and transfer them in a batch.

#### Scenario: Flush triggers at threshold

- GIVEN a hot tier with 80% capacity watermark configured
- WHEN memory usage reaches 80%
- THEN a background flush is initiated
- AND the flushed vectors are removed from the hot tier after the cold tier acknowledges the batch write

#### Scenario: Normal operation below threshold

- GIVEN a hot tier with 80% capacity watermark configured
- WHEN memory usage is at 40%
- THEN no flush is triggered
- AND all data remains in the hot tier

### Requirement: Recovery from WAL Replay

After a crash, the hot tier MUST be recoverable by replaying the WAL. All mutations replayed from the WAL MUST be applied to the hot tier in order, producing the same in-memory state as before the crash.

#### Scenario: Hot tier state restored after crash

- GIVEN a hot tier that previously held vectors `v1`, `v2`, `v3` before a crash
- WHEN WAL replay is triggered during startup
- THEN after replay completes, the hot tier contains `v1`, `v2`, `v3`

#### Scenario: Partial replay stops at corruption boundary

- GIVEN a partially corrupt WAL where `v1` and `v2` are valid but `v3` is corrupt
- WHEN WAL replay stops at the first corrupt entry
- THEN the hot tier contains `v1` and `v2` but NOT `v3`
