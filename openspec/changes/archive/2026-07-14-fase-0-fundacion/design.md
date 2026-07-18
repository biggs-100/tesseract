# Design: Phase 0 — Foundation and Key Concepts

## Technical Approach

Phase 0 establishes the Tesseract workspace, six stub crates, VQL parser, and math foundation types — all compile-ready but with zero runtime execution. The dependency hierarchy follows a layered architecture: common is cross-cutting, core + storage + index form the data plane, vql is the query plane, and api is the integration plane. Every `.rs` file carries AGPL v3 headers. CI across three platforms enforces build, clippy, fmt, and license audit.

## Architecture Decisions

| Decision | Alternatives Considered | Chosen | Rationale |
|---|---|---|---|
| WeightMask | `HashMap<usize,f32>` (pointer overhead), `Vec<f32>` (dense waste) | `Vec<(usize,f32)>` | Sparse + cache-local iteration; amenable to SIMD |
| Parser | pest (grammar file + codegen), lalrpop (complex setup) | nom | Zero-copy, composable combinators, minimal deps for a ~15-rule grammar |
| Edition | 2021 | 2024 | Latest Rust edition; `unsafe` prelude hygiene, `impl Trait` in RPIT improvements |
| Async runtime | async-std, smol | tokio | Dominant ecosystem for database internals; spawn_blocking, async I/O |
| Mutex strategy | parking_lot everywhere, std everywhere | Both | `parking_lot::Mutex` for internal hot paths (no poisoning, faster); `std::sync::Mutex` at public API boundaries for safety guarantees |
| Lockfile | omit `.gitignore` lockfile | Commit `Cargo.lock` | Deterministic builds across environments; required for applications |
| CI matrix | single platform | ubuntu + windows + macOS | Smoke test portability from day one |

## Crate Dependency Graph

```
tesseract-api
    └── tesseract-vql
            └── tesseract-core
tesseract-index
    ├── tesseract-core
    ├── tesseract-storage
    └── tesseract-common
tesseract-storage
    ├── tesseract-core
    └── tesseract-common
tesseract-vql
    └── tesseract-core
tesseract-common  (no workspace deps)
tesseract-core    ├── tesseract-common
                   └── (serde, bincode, tracing, thiserror)
```

## File Changes

| File | Action | Description |
|---|---|---|
| `Cargo.toml` | Create | Workspace root with 6 members, dep version mgmt, edition 2024 |
| `tesseract-core/Cargo.toml` | Create | Core crate manifest; deps: serde, bincode, tracing, thiserror, tesseract-common (path) |
| `tesseract-core/src/lib.rs` | Create | Exports `types`, `distance`, `projection` modules |
| `tesseract-core/src/types.rs` | Create | `VectorId(u64)`, `MetadataValue` enum, `Timestamp` newtype |
| `tesseract-core/src/distance.rs` | Create | `Distance` trait, `NormalizedVector`, `CosineDistance`, `EuclideanDistance` |
| `tesseract-core/src/projection.rs` | Create | `Projection` trait, `WeightMask(Vec<(usize, f32)>)` |
| `tesseract-vql/Cargo.toml` | Create | Parser crate manifest; deps: nom, nom_locate, tesseract-core (path) |
| `tesseract-vql/src/lib.rs` | Create | Exports `ast`, `grammar`, `parser` modules |
| `tesseract-vql/src/ast.rs` | Create | `Query`, `SimilarityExpr`, `MetadataWhere`, `OrderBy`, `Limit`, `Within` — all `#[derive(Debug, Clone, PartialEq)]` |
| `tesseract-vql/src/grammar.rs` | Create | nom combinators matching each AST node |
| `tesseract-vql/src/parser.rs` | Create | `pub fn parse(input: &str) -> Result<Query, ParseError>`; wraps internal nom IResult; ParseError defined in tesseract-vql |
| `tesseract-common/Cargo.toml` | Create | Stub manifest; deps: thiserror |
| `tesseract-common/src/lib.rs` | Create | Stub `lib.rs` with module declarations and placeholder exports |
| `tesseract-common/src/error.rs` | Create | `Error` enum (thiserror), `Result<T>` alias; covers dimension mismatch, index out of bounds, parse errors |
| `tesseract-storage/Cargo.toml` | Create | Stub manifest; deps: tesseract-core (path), tesseract-common (path) |
| `tesseract-storage/src/lib.rs` | Create | Stub `lib.rs` with module declarations and placeholder exports |
| `tesseract-index/Cargo.toml` | Create | Stub manifest; deps: tesseract-core (path), tesseract-storage (path), tesseract-common (path) |
| `tesseract-index/src/lib.rs` | Create | Stub `lib.rs` with module declarations and placeholder exports |
| `tesseract-api/Cargo.toml` | Create | Stub manifest; deps: tesseract-vql (path) |
| `tesseract-api/src/lib.rs` | Create | Stub `lib.rs` with module declarations and placeholder exports |
| `.github/workflows/ci.yml` | Create | 5-job matrix (check, lint, fmt, test, audit) × 3 platforms |
| `.rustfmt.toml` | Create | `max_width=120`, `tab_spaces=4`, `use_small_heuristics="Max"` |
| `deny.toml` | Create | AGPL-3.0, MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-DFS-2016, CC0-1.0 |
| `.cargo/config.toml` | Create | Optional CI target-dir override |

## Interfaces / Contracts

```rust
// tesseract-core/src/types.rs
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Hash)]
pub struct VectorId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum MetadataValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(i64),       // Unix timestamp ms
    Array(Vec<MetadataValue>),
}

// tesseract-common/src/error.rs
use thiserror::Error;

/// Unified error type for all tesseract crates.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Dimension mismatch: self has {0} dimensions, other has {1}")]
    DimensionMismatch(usize, usize),

    #[error("Index {0} out of bounds for vector of length {1}")]
    IndexOutOfBounds(usize, usize),

    #[error("Parse error at line {line}, column {col}: {message}")]
    ParseError {
        line: usize,
        col: usize,
        message: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

// tesseract-core/src/distance.rs
pub trait Distance {
    fn distance(&self, other: &Self) -> crate::types::Result<f64>;
}

/// L2-normalized vector wrapper. Construction divides by the L2 norm;
/// panics on zero/non-finite input. The inner Vec<f64> is private — all
/// construction goes through `::new()` which enforces normalization.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(try_from = "Vec<f64>")]
pub struct NormalizedVector(Vec<f64>);

impl NormalizedVector {
    /// Build a NormalizedVector from raw components, asserting L2
    /// normalization invariants (non-zero, finite norm). This is the
    /// ONLY way to construct a NormalizedVector.
    pub fn new(v: Vec<f64>) -> Self {
        let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!(norm.is_finite() && norm > 0.0,
            "NormalizedVector requires a finite, non-zero vector");
        Self(v.into_iter().map(|x| x / norm).collect())
    }
}

/// Custom deserialization — uses `new()` to enforce the invariant.
impl TryFrom<Vec<f64>> for NormalizedVector {
    type Error = String;
    fn try_from(v: Vec<f64>) -> Result<Self, Self::Error> {
        Ok(Self::new(v))
    }
}

impl std::ops::Deref for NormalizedVector {
    type Target = Vec<f64>;
    fn deref(&self) -> &Vec<f64> {
        &self.0
    }
}

/// Cosine distance on L2-normalized vectors: 1.0 - dot_product(a, b).
pub struct CosineDistance(pub NormalizedVector);
impl Distance for CosineDistance {
    fn distance(&self, other: &Self) -> crate::types::Result<f64> {
        if self.0.len() != other.0.len() {
            return Err(crate::types::Error::DimensionMismatch(
                self.0.len(),
                other.0.len(),
            ));
        }
        let dot: f64 = self.0.iter()
            .zip(other.0.iter())
            .map(|(a, b)| a * b)
            .sum();
        Ok(1.0 - dot)
    }
}

/// Standard Euclidean distance: sqrt(sum((a - b)^2)).
pub struct EuclideanDistance(pub Vec<f64>);
impl Distance for EuclideanDistance {
    fn distance(&self, other: &Self) -> crate::types::Result<f64> {
        if self.0.len() != other.0.len() {
            return Err(crate::types::Error::DimensionMismatch(
                self.0.len(),
                other.0.len(),
            ));
        }
        let sum_sq: f64 = self.0.iter()
            .zip(&other.0)
            .map(|(a, b)| (a - b).powi(2))
            .sum();
        Ok(sum_sq.sqrt())
    }
}

// tesseract-core/src/projection.rs
pub struct WeightMask(pub Vec<(usize, f32)>);

pub trait Projection {
    fn project(&self, mask: &WeightMask) -> crate::types::Result<Self>
    where
        Self: Sized;
}

// tesseract-vql/src/ast.rs
pub struct Query {
    pub find: String,
    pub similarity: Option<SimilarityExpr>,
    pub metadata_where: Option<MetadataWhere>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<Limit>,
    pub within: Option<Within>,
}
```

## Testing Strategy

| Layer | What | Approach |
|---|---|---|
| Unit | VQL combinator parsing per clause | `assert_eq!` on AST output for valid/invalid inputs |
| Unit | Core type serde roundtrip | bincode serialize → deserialize → assert_eq |
| Unit | Distance trait math | Hand-computed f64 assertions (cosine 0°, Euclidean 3-4-5) |
| Unit | Projection weight mask | Zero-weight dims produce 0.0 output |
| Integration | Full query parsing | Multi-clause queries produce complete AST |
| CI | Build + lint + fmt | Pipeline gates on `cargo build` / `clippy` / `fmt --check` |

## Threat Matrix

**N/A** — Phase 0 covers project scaffold, VQL grammar, and math traits exclusively. No routing, shell commands, subprocesses, VCS/PR automation, executable-file classification, or process-integration boundaries exist in this scope.

## Migration / Rollout

No migration required. Phase 0 is pure greenfield — no data, no state, no existing consumers. Rollback is a clean `git revert`.

## Open Questions

None identified. All design decisions are resolved per the proposal and specs.
