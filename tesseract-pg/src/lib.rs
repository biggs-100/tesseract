// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract PostgreSQL extension — proxy functions to the Tesseract API.
//!
//! When compiled with the `pg_extension` feature, this crate produces a
//! `cdylib` that can be loaded by PostgreSQL via `CREATE EXTENSION tesseract_fdw`.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────┐         ┌──────────────┐         ┌──────────────────┐
//! │ PostgreSQL       │  HTTP   │ Tesseract    │         │ Vector Storage   │
//! │ tesseract_pg     │ ──────► │ API Server   │ ──────► │ + Query Engine   │
//! │ (pgrx extension) │ ◄────── │              │         │                  │
//! └──────────────────┘         └──────────────┘         └──────────────────┘
//! ```
//!
//! # Quick start
//!
//! ```sql
//! CREATE EXTENSION tesseract_fdw;
//! SELECT tesseract_connect('localhost', 8081);
//! SELECT * FROM tesseract_query('...');
//! SELECT tesseract_insert(42, ARRAY[0.1,0.2,0.3], '{"title":"hello"}'::jsonb);
//! ```

pub mod client;
pub mod config;

#[cfg(feature = "pg_extension")]
pub mod pg_entry;
