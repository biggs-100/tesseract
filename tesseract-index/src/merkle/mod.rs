// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Progressive Merkle Tree — data freshness layer.
//!
//! Provides a Merkle-tree-based index over vector centroids that
//! supports incremental insertion through a hot buffer, async merge,
//! nearest-centroid search, and tree-verified proof of data.
//!
//! # Architecture
//!
//! ```text
//! insert() → HotBuffer (memory, immediately queryable)
//!             ↓ when full, async merge
//!          MerkleTree.insert_batch()
//!             ↓
//!          Update centroids + recompute hashes
//! ```

pub mod hot_buffer;
pub mod node;
pub mod tree;

pub use hot_buffer::*;
pub use node::*;
pub use tree::*;
