# HNSW Graph Specification

## Purpose

The HNSW graph implements the Navigable Small World with Hierarchical layers (Malkov & Yashunin 2016) for approximate nearest neighbor search. It provides configurable graph topology, weighted distance injection, and concurrent read access.

## Requirements

### Requirement: Configurable Graph Topology

The system MUST expose M (max connections per node, default 16) and ef_construction (default 200) as configuration parameters at construction time. ef_search MUST be configurable per-query.

#### Scenario: Default parameters applied

- GIVEN a new HnswGraph constructed without explicit M or ef_construction
- WHEN the graph is used for insert and search
- THEN M defaults to 16 AND ef_construction defaults to 200

#### Scenario: Per-query ef_search override

- GIVEN a built HNSW graph
- WHEN search is called with ef=100 and then ef=300 on the same query
- THEN the query with ef=300 MUST return more candidates (higher recall) than ef=100

### Requirement: Multi-Layer Navigation

The graph MUST use L = max(1, ceil(log2(n))) layers where n is the number of indexed vectors. Layer 0 MUST contain all vectors. Higher layers MUST contain progressively fewer vectors (exponential decay via random level assignment).

#### Scenario: Layer count grows with index size

- GIVEN an empty HNSW graph
- WHEN N vectors are inserted
- THEN the number of layers MUST be at least ceil(log2(N)) and at most max(1, ceil(log2(N)))

#### Scenario: Single vector in graph

- GIVEN an HNSW graph with exactly 1 vector
- WHEN the graph is queried
- THEN L MUST equal 1 (single layer)

### Requirement: Generic Distance Computer

The graph MUST accept a DistanceComputer via generic parameter for distance computation. DistanceComputer MUST be a trait defining a `distance(a: &[f32], b: &[f32]) -> f32` method. All graph edges MUST store f32 vectors internally.

#### Scenario: Euclidean distance graph

- GIVEN HnswGraph<Euclidean> where Euclidean implements DistanceComputer
- WHEN vectors are inserted and searched
- THEN all distance comparisons use the Euclidean metric

### Requirement: Weighted Distance via WeightMask

Search MUST accept an optional `&WeightMask` parameter. When provided, weights MUST be applied inline during graph traversal (single fused O(d) pass, not post-filtered).

#### Scenario: Weighted query returns different results

- GIVEN a built graph with vectors
- WHEN search is called without WeightMask vs with a non-uniform WeightMask
- THEN the top-K results MAY differ between the two queries

#### Scenario: WeightMask fused into distance loop

- GIVEN a query with WeightMask of length dim
- WHEN distance is computed during traversal
- THEN each dimension contribution MUST be multiplied by the corresponding weight in a single pass (no separate weighting step)

### Requirement: Idempotent Insert

Re-inserting an existing VectorId MUST update the stored vector without creating duplicate nodes. The graph MUST detect duplicate IDs and replace the vector data.

#### Scenario: Update existing vector

- GIVEN a graph containing vector v1 with id=42
- WHEN insert(42, v2) is called where v2 is a different vector
- THEN the graph MUST replace v1 with v2 AND the node count MUST NOT increase

### Requirement: Concurrent Read Access

The graph MUST use RwLock to allow concurrent reads. Write operations (insert, remove) MUST acquire a write lock. Read operations (search) MUST acquire a read lock.

#### Scenario: Concurrent searches proceed in parallel

- GIVEN a built graph with 10K vectors
- WHEN 4 threads execute search simultaneously
- THEN all 4 searches MUST complete successfully AND return valid results

#### Scenario: Insert blocks during write lock

- GIVEN an in-progress insert operation
- WHEN a concurrent search request arrives
- THEN the search MUST wait until the insert completes (read lock acquired after write lock released)
