# Topological Index Specification

## Purpose

The TopologicalIndex trait defines a common interface for ANN algorithms, enabling algorithm swapping (HNSW, IVF, DiskANN) behind a single abstraction. The primary implementation is HnswIndex<D> wrapping the HNSW graph.

## Requirements

### Requirement: TopologicalIndex Trait Definition

The system MUST define a `TopologicalIndex` trait with the following methods:

| Method | Signature | Description |
|--------|-----------|-------------|
| `insert` | `(&mut self, id: VectorId, vector: &[f32])` | Add or update a vector |
| `search` | `(&self, query: &[f32], ef: usize, mask: Option<&WeightMask>) -> Vec<(VectorId, f32)>` | Find nearest neighbors |
| `remove` | `(&mut self, id: VectorId)` | Mark a vector as deleted |
| `len` | `(&self) -> usize` | Number of active vectors |
| `save` | `(&self, writer: &mut impl Write)` | Serialize full graph state |
| `load` | `(&mut self, reader: &mut impl Read)` | Deserialize graph state |

#### Scenario: Trait implemented for HNSW

- GIVEN a concrete type HnswIndex<D> where D: DistanceComputer
- WHEN the type is checked against the TopologicalIndex trait
- THEN it MUST satisfy all trait methods with the correct signatures

#### Scenario: Default empty state

- GIVEN a newly constructed TopologicalIndex implementation
- WHEN len() is called
- THEN it MUST return 0

### Requirement: Search Returns Sorted Results

TopologicalIndex::search MUST return Vec<(VectorId, f32)> sorted by distance ascending. The f32 value MUST be the computed distance (weighted if a mask was provided).

#### Scenario: Results in ascending distance

- GIVEN a query vector and a built index with 1000 vectors
- WHEN search returns 10 results
- THEN result[0].1 <= result[1].1 <= ... <= result[9].1

#### Scenario: Single result for single vector index

- GIVEN an index with exactly one vector matching the query
- WHEN search returns the result
- THEN the result Vec MUST have length 1 with distance 0.0

### Requirement: Weighted Search Delegation

When TopologicalIndex::search receives a Some(mask), it MUST pass the mask through to the underlying graph search for fused weighted distance computation.

#### Scenario: Mask forwarded to graph

- GIVEN an HnswIndex with an internal HNSW graph
- WHEN search is called with mask=Some(weights)
- THEN the graph MUST receive the weight mask AND results MUST reflect weighted distances

### Requirement: Removal via Tombstone

TopologicalIndex::remove SHOULD mark nodes as deleted using a tombstone bitset rather than removing edges from the graph. Tombstoned nodes MUST be excluded from search results. Tombstoned nodes SHOULD NOT count toward len().

#### Scenario: Tombstoned node excluded from results

- GIVEN an index with 100 vectors including vector with id=42
- WHEN remove(42) is called followed by search
- THEN id=42 MUST NOT appear in any search results

#### Scenario: Re-insert after tombstone

- GIVEN an index where id=42 was removed and len() returned 99
- WHEN insert(42, new_vector) is called
- THEN the tombstone MUST be cleared AND len() MUST return 100

### Requirement: Full State Persistence

TopologicalIndex::save MUST serialize the complete graph state (nodes, edges, vectors, entry point, config). TopologicalIndex::load MUST restore all state from the serialized form.

#### Scenario: Save then load roundtrip

- GIVEN an index with 1000 inserted vectors
- WHEN save(writer) followed by load(reader) on a fresh index
- THEN the loaded index MUST return identical search results for the same query (same ordering and distances within f32 epsilon)
