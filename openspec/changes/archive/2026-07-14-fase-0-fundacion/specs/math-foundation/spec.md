# Math Foundation Specification

## Purpose

Define the core mathematical types and traits that underpin vector operations in Tesseract: vector identification, metadata typing, distance computation, and weighted projection.

## Requirements

### Requirement: `VectorId` uniquely identifies a vector

The `VectorId` type MUST uniquely identify a vector within the system. It MUST implement `Serialize` and `Deserialize`.

#### Scenario: VectorId roundtrips through serde

- GIVEN a valid `VectorId` value
- WHEN it is serialized via serde and deserialized back
- THEN the deserialized value equals the original

### Requirement: `MetadataValue` typed enum

`MetadataValue` MUST be an enum with variants for `String`, `Integer`, `Float`, `Boolean`, `DateTime`, and `Array`. The `Array` variant MUST hold a `Vec<MetadataValue>`.

#### Scenario: All variants constructable

- GIVEN each metadata variant
- WHEN constructing a `MetadataValue::String("tag".into())` and `MetadataValue::Integer(42)`
- THEN both compile and store the expected inner value

#### Scenario: Nested array structure

- GIVEN a `MetadataValue::Array` containing mixed variants
- WHEN the array is accessed
- THEN each element preserves its variant type

### Requirement: `Distance` trait

The `Distance` trait MUST define a method `distance(&self, other: &Self) -> Result<f64, Error>`. Implementors SHALL compute the distance between two vector instances and return `Err(Error::DimensionMismatch)` when dimensions differ.

#### Scenario: Successful distance computation

- GIVEN a type implementing `Distance` with two same-dimension vectors
- WHEN `distance(&a, &b)` is called
- THEN it returns `Ok(f64)`

#### Scenario: Dimension mismatch returns error

- GIVEN a type implementing `Distance` with vectors of differing lengths
- WHEN `distance(&a, &b)` is called
- THEN it returns `Err(Error::DimensionMismatch(actual, expected))`

### Requirement: `CosineDistance` for normalized vectors

`CosineDistance` MUST wrap a `NormalizedVector` and implement `Distance`. Distance SHALL be computed as `1.0 - dot_product(self.0.0, other.0.0)`. It SHALL return `Err(Error::DimensionMismatch)` when the two wrapped vectors have unequal lengths.

#### Scenario: Cosine of identical vectors

- GIVEN two `CosineDistance` instances wrapping the same normalized vector
- WHEN `distance` is called
- THEN the result is `Ok(0.0)`

#### Scenario: Dimension mismatch between CosineDistance instances

- GIVEN `CosineDistance(NormalizedVector(vec![1.0, 0.0]))` and `CosineDistance(NormalizedVector(vec![1.0]))`
- WHEN `distance` is called
- THEN it returns `Err(Error::DimensionMismatch(2, 1))`

### Requirement: `EuclideanDistance` implementation

`EuclideanDistance` MUST implement `Distance` and compute the standard Euclidean distance.

#### Scenario: Euclidean distance between two points

- GIVEN vectors `[3.0, 4.0]` and `[0.0, 0.0]`
- WHEN `EuclideanDistance::distance` is called
- THEN the result is `Ok(5.0)`

### Requirement: `NormalizedVector` newtype

`NormalizedVector` MUST be a newtype wrapper over `Vec<f64>` that guarantees L2-normalized storage. Construction MUST divide each element by the L2 norm and panic on zero-length or non-finite input.

#### Scenario: NormalizedVector correctly normalizes

- GIVEN a vector `[3.0, 4.0]`
- WHEN `NormalizedVector::new` is called
- THEN the result is `NormalizedVector([0.6, 0.8])`

#### Scenario: NormalizedVector panics on zero vector

- GIVEN an empty `Vec<f64>` or a zero-magnitude vector
- WHEN `NormalizedVector::new` is called
- THEN the constructor panics with a descriptive message

### Requirement: `Projection` trait

The `Projection` trait MUST define a method `project(&self, mask: &WeightMask) -> Result<Self, Error>` that returns a projected copy of the vector weighted by the mask. It SHALL return `Err(Error::IndexOutOfBounds)` when a mask index exceeds the vector's length.

#### Scenario: Projection with uniform weights

- GIVEN a `WeightMask` with all weights set to 1.0 and indices within bounds
- WHEN `project` is called on a vector
- THEN the result is `Ok(projected)` where projected equals the original vector

#### Scenario: Projection with out-of-bounds index

- GIVEN a `WeightMask` containing `(10, 0.5)` and a 3-dimensional vector
- WHEN `project` is called
- THEN it returns `Err(Error::IndexOutOfBounds(10, 3))`

### Requirement: `WeightMask` sparse representation

`WeightMask` MUST be a sparse representation of dimension weights of type `f32`. It SHOULD store only non-zero entries.

#### Scenario: Zero-weight projection

- GIVEN a `WeightMask` with entry (dimension=0, weight=0.0)
- WHEN `project` is called
- THEN the projected dimension is `Ok(0.0)`

### Requirement: Unified `Error` type

`tesseract-common::error::Error` MUST be a thiserror-derived enum with variants for `DimensionMismatch(usize, usize)`, `IndexOutOfBounds(usize, usize)`, and `ParseError { line, col, message }`. It MUST provide a `Result<T>` type alias for `std::result::Result<T, Error>`.

#### Scenario: DimensionMismatch display

- GIVEN `Error::DimensionMismatch(3, 5)`
- WHEN `to_string()` is called
- THEN the message includes "Dimension mismatch" and both dimension values

#### Scenario: IndexOutOfBounds display

- GIVEN `Error::IndexOutOfBounds(10, 3)`
- WHEN `to_string()` is called
- THEN the message includes "Index 10 out of bounds for vector of length 3"

### Requirement: Serde Serialize + Deserialize

All core types (`VectorId`, `MetadataValue`, `NormalizedVector`, `CosineDistance`, `EuclideanDistance`, `WeightMask`) MUST implement `serde::Serialize` and `serde::Deserialize`.

#### Scenario: All types derive serde

- GIVEN each core type
- WHEN inspected
- THEN it carries `#[derive(Serialize, Deserialize)]` or equivalent implementation

### Requirement: Bincode roundtrip

Core types MUST serialize and deserialize identically through bincode encoding. Roundtrip SHALL produce byte-identical output for the same input value.

#### Scenario: Bincode roundtrip of VectorId

- GIVEN a `VectorId` value
- WHEN serialized with bincode and deserialized back
- THEN the result equals the original

### Requirement: Debug-level tracing on traits

The `Distance` and `Projection` trait implementations SHOULD emit `tracing::debug!` spans or events for key operations.

#### Scenario: Tracing span emitted on distance call

- GIVEN a `Distance` implementation with tracing instrumented
- WHEN `distance` is called with a subscriber active
- THEN a debug-level trace event is emitted
