# Page Cache Specification

## Purpose

The page cache sits between the cold tier and query execution, caching recently accessed cold-tier pages in memory under LRU eviction. It reduces latency for repeated cold reads by avoiding redundant Parquet I/O.

## Requirements

### Requirement: Cold Tier Page Caching

The page cache MUST cache pages read from the cold tier in memory. A "page" is defined as a row group of a Parquet file or a contiguous byte range of decompressed vector data.

#### Scenario: Cold page served from cache on second access

- GIVEN an empty page cache
- WHEN a cold-tier page is read for the first time
- THEN the page is fetched from disk AND inserted into the cache
- AND when the same page is read again, it is served from the cache (no disk I/O)

### Requirement: LRU Eviction

When the cache reaches its capacity limit, the least recently used page MUST be evicted to make room for new pages.

#### Scenario: Eviction removes least recently used page

- GIVEN a page cache with capacity for 3 pages, already holding pages A, B, C
- WHEN page A is accessed (making B and C the LRU candidates), then a new page D is inserted
- THEN page B is evicted (oldest non-recently-used entry)
- AND the cache now holds A, C, D

#### Scenario: Accessed page is promoted in LRU order

- GIVEN a page cache with capacity 3 holding A, B, C
- WHEN page A is accessed and then a new page D is inserted
- THEN A stays (recently accessed), B is evicted
- AND A, C, D remain in the cache

### Requirement: Configurable Cache Size

The page cache size MUST be configurable at initialization. The unit MUST be either number of pages or total bytes.

#### Scenario: Cache configured by byte limit

- GIVEN a page cache initialized with a 256 MB limit
- WHEN pages totalling 256 MB are cached
- THEN inserting a new page triggers eviction of existing pages to stay under the limit

#### Scenario: Cache configured by page count

- GIVEN a page cache initialized with a limit of 100 pages
- WHEN 100 pages are cached
- THEN inserting a new page evicts the LRU page

### Requirement: Concurrent Read Support

The page cache MUST support concurrent read access from multiple tasks. Cache hits and misses MUST be safe under concurrent reads without data races.

#### Scenario: Concurrent reads do not corrupt the cache

- GIVEN a page cache with pages A, B, C
- WHEN 10 concurrent tasks read pages A and B simultaneously
- THEN all 10 tasks receive correct data
- AND the cache structure is not corrupted
