# Cold Tier Specification

## Purpose

The cold tier provides persistent, compressed storage for vectors and metadata using Parquet files. It is optimized for bulk reads and analytical-style queries with minimal memory footprint. Writes are batch-only (no single-record inserts), and reads leverage row group statistics to prune irrelevant data.

## Requirements

### Requirement: Parquet File Persistence

The cold tier MUST persist vectors and metadata to Parquet files. Each Parquet file MUST contain the vector embedding column and all metadata columns for the stored records.

#### Scenario: Vectors and metadata written to Parquet and read back

- GIVEN a batch of 100 vectors with metadata
- WHEN the batch is flushed to the cold tier
- THEN a Parquet file is created on disk
- AND reading the file back returns all 100 vectors with their metadata intact

### Requirement: ZSTD Compression for Embedding Columns

The cold tier MUST use ZSTD compression for embedding (vector) columns. The compression setting MUST be configurable at the store level.

#### Scenario: Compressed column is smaller than uncompressed

- GIVEN a batch of 1024-dimensional vectors
- WHEN the batch is written to Parquet with ZSTD compression on the embedding column
- THEN the on-disk size of the embedding column is smaller than the raw byte size of the vectors

#### Scenario: Decompressed data matches original

- GIVEN a batch of vectors written with ZSTD compression
- WHEN the Parquet file is read back
- THEN every decompressed embedding is bitwise identical to the original

### Requirement: Row Group Statistics for Query Pruning

The cold tier MUST maintain min/max statistics per metadata column at the row group level. These statistics MUST be used to skip row groups during queries when the predicate cannot be satisfied.

#### Scenario: Row group pruned by min/max statistics

- GIVEN a cold tier with two row groups: RG1 (`score` 1–50) and RG2 (`score` 51–100)
- WHEN a query filters for `score > 75`
- THEN RG1 is pruned (no rows match)
- AND only RG2 is scanned

#### Scenario: Row group included when statistics overlap

- GIVEN the same two row groups from the previous scenario
- WHEN a query filters for `score > 25`
- THEN BOTH row groups are scanned (RG1 is not pruned because 1–50 overlaps with the predicate)

### Requirement: Batch-Only Writes

The cold tier MUST accept writes only in batches. Single-record writes MUST NOT be supported. The minimum batch size MUST be configurable (default: 1 entry, effective minimum enforced by the tier lifecycle).

#### Scenario: Single-record write is rejected

- GIVEN a cold tier
- WHEN an attempt is made to write a single vector record
- THEN the write is rejected with a `BatchRequired` error

#### Scenario: Batch write succeeds

- GIVEN a cold tier
- WHEN a batch of 1000 vectors is written
- THEN the write succeeds
- AND all 1000 vectors are persisted to Parquet

### Requirement: Partitioned Reads

The cold tier MUST support reading specific partitions without loading the entire Parquet store. A partition is defined as a single Parquet file or a subset of row groups within a file.

#### Scenario: Read single partition returns only that partition's data

- GIVEN a cold tier partitioned by date (file-per-day)
- WHEN a query reads the "2026-07-14" partition
- THEN only vectors stored on that date are returned
- AND no data from other partitions is loaded into memory
