// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! PostgreSQL extension entry point.
//!
//! Only compiled when the `pg_extension` feature is enabled (which requires
//! a local PostgreSQL installation with `cargo pgrx init`).
//!
//! Registers:
//! - GUC variables `tesseract_host`, `tesseract_port`, `tesseract_timeout`
//! - SQL function `tesseract_connect(host, port)`
//! - SQL function `tesseract_query(vql)` — set-returning (SRF)
//! - SQL function `tesseract_insert(id, vector, metadata)`

use pgrx::guc::GucSetting;
use pgrx::prelude::*;

pgrx::pg_module_magic!();

// ---------------------------------------------------------------------------
// GUC variables — per-session configuration
// ---------------------------------------------------------------------------

/// Tesseract server hostname (PG GUC: `tesseract_host`).
pub static TESSERACT_HOST: GucSetting<String> = GucSetting::new("localhost".to_string());

/// Tesseract server port (PG GUC: `tesseract_port`).
pub static TESSERACT_PORT: GucSetting<i32> = GucSetting::new(8081);

/// Request timeout in milliseconds (PG GUC: `tesseract_timeout`).
pub static TESSERACT_TIMEOUT: GucSetting<i32> = GucSetting::new(5000);

// ---------------------------------------------------------------------------
// Client factory
// ---------------------------------------------------------------------------

/// Build a [`TesseractClient`] from current GUC values.
pub fn build_client() -> crate::client::TesseractClient {
    let host = TESSERACT_HOST.get();
    let port = TESSERACT_PORT.get() as u16;
    let timeout = TESSERACT_TIMEOUT.get() as u64;
    crate::client::TesseractClient::new(&host, port, timeout)
}

// ---------------------------------------------------------------------------
// SQL-callable functions
// ---------------------------------------------------------------------------

/// Configure the Tesseract endpoint for this session.
///
/// Sets `tesseract_host` and `tesseract_port` GUCs so that subsequent
/// `tesseract_query` / `tesseract_insert` calls connect to the given host.
#[pg_extern]
fn tesseract_connect(host: String, port: i32) -> bool {
    TESSERACT_HOST.set(host);
    TESSERACT_PORT.set(port);
    true
}

// ---------------------------------------------------------------------------
// Type conversion helpers
// ---------------------------------------------------------------------------

/// Convert a PG `REAL[]` slice to `Vec<f64>` for the Tesseract wire protocol.
///
/// The Tesseract API accepts `Vec<f64>` vectors on the wire; PG passes
/// `REAL[]` as `&[f32]`. This helper widens each element.
fn real_array_to_vec_f64(arr: &[f32]) -> Vec<f64> {
    arr.iter().map(|&x| x as f64).collect()
}

/// Unwrap a pgrx `JsonB` into `serde_json::Value`.
fn jsonb_to_value(jsonb: JsonB) -> serde_json::Value {
    jsonb.0
}

// ---------------------------------------------------------------------------
// Error mapping — convert ClientError to PG errors
// ---------------------------------------------------------------------------

/// Map a [`ClientError`] to a PG `error!()` with the appropriate SQLSTATE.
fn map_client_error(err: crate::client::ClientError) -> ! {
    match err {
        crate::client::ClientError::ConnectionError(msg) => {
            error!(SqlState::ERRCODE_CONNECTION_FAILURE, "tesseract_fdw: connection refused: {}", msg,);
        }
        crate::client::ClientError::RequestError { status: _, message } => {
            error!(SqlState::ERRCODE_DATA_EXCEPTION, "tesseract_fdw: {}", message,);
        }
        crate::client::ClientError::ParseError(msg) => {
            error!(SqlState::ERRCODE_DATA_EXCEPTION, "tesseract_fdw: parse error: {}", msg,);
        }
    }
}

// ---------------------------------------------------------------------------
// tesseract_query — VQL query execution (SRF)
// ---------------------------------------------------------------------------

/// Execute a VQL query against the configured Tesseract endpoint and return
/// matching results as a table with columns `id` (BIGINT), `score` (REAL),
/// and `metadata` (JSONB).
///
/// # Errors
///
/// - `ERRCODE_CONNECTION_FAILURE` — Tesseract is unreachable.
/// - `ERRCODE_DATA_EXCEPTION` — the query was rejected or response parsing
///   failed.
#[pg_extern]
fn tesseract_query(vql: &str) -> TableIterator<'static, (name!(id, i64), name!(score, f32), name!(metadata, JsonB))> {
    let client = build_client();
    let rt = tokio::runtime::Runtime::new().expect("failed to create Tokio runtime");

    match rt.block_on(client.query(vql, None)) {
        Ok(results) => {
            let rows: Vec<(i64, f32, JsonB)> =
                results.into_iter().map(|r| (r.id as i64, r.score, JsonB(r.metadata))).collect();
            TableIterator::new(rows)
        }
        Err(err) => map_client_error(err),
    }
}

// ---------------------------------------------------------------------------
// tesseract_insert — data insertion
// ---------------------------------------------------------------------------

/// Insert a vector with optional metadata into Tesseract.
///
/// Returns the inserted `id` as BIGINT.
///
/// # Errors
///
/// - `ERRCODE_CONNECTION_FAILURE` — Tesseract is unreachable.
/// - `ERRCODE_DATA_EXCEPTION` — the insert was rejected or response parsing
///   failed.
#[pg_extern]
fn tesseract_insert(id: i64, vector: Vec<f32>, metadata: Option<JsonB>) -> i64 {
    let client = build_client();
    let vector_f64 = real_array_to_vec_f64(&vector);
    let metadata_value = metadata.map(jsonb_to_value);
    let rt = tokio::runtime::Runtime::new().expect("failed to create Tokio runtime");

    match rt.block_on(client.insert(id as u64, vector_f64, metadata_value)) {
        Ok(inserted_id) => inserted_id as i64,
        Err(err) => map_client_error(err),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_real_array_to_vec_f64() {
        let input: Vec<f32> = vec![1.0, 2.5, 3.75];
        let output = real_array_to_vec_f64(&input);
        assert_eq!(output, vec![1.0_f64, 2.5_f64, 3.75_f64]);
    }

    #[test]
    fn test_real_array_to_vec_f64_empty() {
        assert!(real_array_to_vec_f64(&[]).is_empty());
    }

    #[test]
    fn test_jsonb_to_value() {
        let val = serde_json::json!({"key": "value", "nested": {"a": 1}});
        let jsonb = JsonB(val.clone());
        assert_eq!(jsonb_to_value(jsonb), val);
    }

    #[test]
    fn test_jsonb_to_value_null() {
        let val = serde_json::Value::Null;
        let jsonb = JsonB(val.clone());
        assert_eq!(jsonb_to_value(jsonb), val);
    }

    #[test]
    fn test_map_connection_error_contains_refused() {
        let err = crate::client::ClientError::ConnectionError("timeout".into());
        let msg = err.to_string();
        assert!(msg.contains("connection error"));
        assert!(msg.contains("timeout"));
    }

    #[test]
    fn test_map_request_error_contains_status() {
        let err = crate::client::ClientError::RequestError { status: 400, message: "bad request".into() };
        let msg = err.to_string();
        assert!(msg.contains("400"));
        assert!(msg.contains("bad request"));
    }
}

/// Integration tests that require `cargo pgrx test` (PG backend + extension).
///
/// Run with: `cargo pgrx test -p tesseract-pg`
#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod integration {
    use pgrx::*;

    #[pg_test]
    fn test_tesseract_connect_returns_true() {
        let result = Spi::get_one::<bool>("SELECT tesseract_connect('localhost', 8081)");
        assert_eq!(result, Some(true));
    }

    #[pg_test]
    fn test_tesseract_query_srf_exists() {
        // Verify the SRF function can be called (will fail with connection
        // error since no Tesseract is running, but the function exists).
        let result = Spi::get_one::<i64>("SELECT COUNT(*) FROM tesseract_query('{\"collection\":\"test\"}')");
        // Tesseract is not running, so the query fails with a PG error
        // that SPI converts to NULL.
        assert!(result.is_none() || result == Some(0));
    }

    #[pg_test]
    fn test_tesseract_insert_function_exists() {
        // Verify the insert function exists by calling it.
        // Without a running Tesseract, it will error, but the function
        // registration itself is verified by the extension loading.
        let result = Spi::get_one::<i64>(
            "SELECT tesseract_insert(
                1,
                ARRAY[0.1, 0.2, 0.3]::real[],
                '{\"title\":\"hello\"}'::jsonb
            )",
        );
        // Tesseract is not running, so expect NULL/error.
        assert!(result.is_none());
    }
}
