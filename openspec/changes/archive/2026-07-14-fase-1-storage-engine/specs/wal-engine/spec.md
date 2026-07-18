# WAL Engine Specification

## Purpose

Durable, crash-safe mutation recording before entries reach the hot tier. Supports configurable consistency via durable and fast modes.

## Requirements

### Requirement: Append-Only Write with CRC32 Integrity

WAL entries MUST be appended sequentially. Each entry MUST include a CRC32 over its header and payload. On readback, CRC32 MUST be validated; the reader MUST stop at the first corrupt entry.

#### Scenario: Entry CRC validated on readback; corruption stops replay

- GIVEN a segment with a valid entry followed by a corrupted entry
- WHEN the segment is read back
- THEN the first entry's CRC32 validates correctly
- AND the reader stops at the corrupt entry boundary

### Requirement: Entry Binary Format

Each WAL entry MUST follow the format: `(txn_id: u64, op_code: u8, payload_len: u32, payload: [u8; payload_len], crc32: u32)`.

#### Scenario: Entry round-trips through serialization

- GIVEN a mutation with known txn_id, op_code, and payload
- WHEN serialized and deserialized
- THEN all fields match the original values

#### Scenario: Payload length mismatch detected

- GIVEN an entry with a payload_len exceeding the remaining bytes
- WHEN parsed
- THEN a `PayloadTruncated` error is returned

### Requirement: Segment Rotation

The WAL MUST split writes across 64 MB segments. When the current segment reaches the limit, it MUST be sealed and a new segment created automatically.

#### Scenario: Segment rolls over at size boundary

- GIVEN a WAL writing to a segment 1 byte below 64 MB
- WHEN an entry pushes past the limit
- THEN the current segment is sealed (no further writes)
- AND a new segment is created for subsequent entries

### Requirement: Configurable Async Fsync

The WAL MUST provide async fsync at configurable intervals. Default: every 100 ms or every 1000 ops, whichever occurs first.

#### Scenario: Fsync triggered by operation count

- GIVEN a WAL with threshold of 1000 ops
- WHEN 1000 entries are written within one interval
- THEN fsync fires before the timer

### Requirement: Crash Recovery with Replay

On startup, the WAL MUST replay from the last checkpoint, validate CRC32 per entry, and stop at the first corruption.

#### Scenario: Clean recovery from checkpoint

- GIVEN a WAL with 3 sealed segments and a known checkpoint
- WHEN recovery runs
- THEN all post-checkpoint entries are replayed in order
- AND the hot tier reflects the replayed mutations

#### Scenario: Recovery stops at corruption

- GIVEN a WAL whose second post-checkpoint entry has corrupt CRC32
- WHEN recovery replays
- THEN the first entry replays
- AND recovery stops at the corrupt entry without applying it

### Requirement: Concurrent Writer Lock

The WAL MUST serialize writers through a single lock. Multiple callers MAY request writes, but only one writes at a time.

#### Scenario: Serialized concurrent writes

- GIVEN two tasks writing entries A and B concurrently
- WHEN both acquire the lock
- THEN entries appear sequentially (A then B or B then A)
- AND both are valid in the WAL

### Requirement: Durable and Fast Consistency Modes

**Durable mode**: acknowledge after fsync. **Fast mode**: acknowledge after buffer write, before fsync.

#### Scenario: Durable mode persists on acknowledgement

- GIVEN a WAL in durable mode
- WHEN an entry is written and acknowledged
- THEN it is guaranteed stable on disk

#### Scenario: Fast mode acknowledges before fsync

- GIVEN a WAL in fast mode
- WHEN an entry is written and acknowledged
- THEN it is in buffer but MAY not yet be stable on disk

### Requirement: WAL Compaction

The WAL MUST merge sealed segments, discarding stale or overwritten entries to recover space.

#### Scenario: Compacted output excludes stale entries

- GIVEN sealed segments with entries A, B, C where B overwrites A (same VectorId)
- WHEN compaction runs
- THEN compacted output contains only B and C
- AND original segments are deleted after the compacted output is fsynced
