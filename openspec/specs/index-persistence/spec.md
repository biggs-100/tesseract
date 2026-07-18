# Index Persistence Specification

## Purpose

The persistence layer serializes and deserializes HNSW graph state to and from byte streams using bincode. A version prefix ensures forward/backward compatibility detection.

## Requirements

### Requirement: Bincode Serialization

Graph persistence MUST use bincode for all serialization. All persisted data structures MUST derive or implement serde::Serialize and serde::Deserialize.

#### Scenario: Roundtrip preserves graph identity

- GIVEN a fully constructed HNSW graph with 5000 vectors across 12 layers
- WHEN the graph is serialized to bytes and deserialized into a new graph
- THEN the deserialized graph MUST have identical node count, edge structure, and entry point

### Requirement: Version Prefix

The serialized format MUST start with a u32 little-endian version prefix. Readers MUST validate this prefix against the current format version on load.

#### Scenario: Valid version loads successfully

- GIVEN a byte stream starting with version prefix 0x00000001
- WHEN load() is called on a reader implementing version 1
- THEN deserialization MUST proceed without version error

#### Scenario: Invalid version is rejected

- GIVEN a byte stream starting with version prefix 0xFFFFFFFF
- WHEN load() is called
- THEN an error MUST be returned AND no graph state must be mutated

### Requirement: Complete State Serialization

Save MUST persist the following components:

| Component | Description |
|-----------|-------------|
| Nodes | All node metadata (IDs, level assignments) |
| Edges per layer | Adjacency lists for each graph layer |
| Vectors | f32 vector data for all nodes |
| Entry point | Current entry node ID (top of the graph) |
| Config parameters | M, ef_construction, max_layers |

#### Scenario: All components restored after load

- GIVEN a saved graph with custom M=32 and ef_construction=400
- WHEN the graph is loaded from the saved state
- THEN M MUST be 32, ef_construction MUST be 400, AND all edges at every layer MUST match the pre-save state

### Requirement: Synchronous I/O Only

Save and load MUST operate on std::io::Read and std::io::Write. The persistence layer MUST NOT use async I/O.

#### Scenario: Save to a Vec<u8>

- GIVEN a graph and a Vec<u8> implementing std::io::Write
- WHEN save(writer) is called
- THEN the Vec<u8> MUST contain valid bincode data starting with the version prefix

#### Scenario: Load from a byte slice

- GIVEN a valid serialized byte stream
- WHEN load(&mut Cursor<&[u8]>) is called
- THEN the graph MUST restore successfully from the byte slice

### Requirement: Incompatible Version Detection

Load SHOULD detect and reject format versions that are incompatible (not just different — e.g., a future breaking format change). The version check SHOULD distinguish between backwards-compatible and breaking changes.

#### Scenario: Future breaking version rejected

- GIVEN a byte stream with a version prefix indicating a breaking format change (e.g., version 2 when only version 1 is understood)
- WHEN load() is called
- THEN an IncompatibleVersion error MUST be returned with the expected and actual versions
