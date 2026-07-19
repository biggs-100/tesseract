// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HTTP client for proxying queries to the Tesseract API.
//!
//! Maps PG function calls to HTTP requests against `POST /query`
//! and `POST /insert` endpoints.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the Tesseract HTTP client.
#[derive(Debug)]
pub enum ClientError {
    /// Network or connection-level failure (timeout, refused, DNS, etc.).
    ConnectionError(String),
    /// Tesseract responded with an application-level error.
    RequestError { status: u16, message: String },
    /// Failed to parse the response body.
    ParseError(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionError(msg) => write!(f, "connection error: {}", msg),
            Self::RequestError { status, message } => {
                write!(f, "request failed ({}): {}", status, message)
            }
            Self::ParseError(msg) => write!(f, "parse error: {}", msg),
        }
    }
}

impl std::error::Error for ClientError {}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// A single scored result returned by a VQL query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoredResult {
    pub id: u64,
    pub score: f32,
    pub metadata: serde_json::Value,
}

/// Body sent to `POST /query`.
#[derive(Debug, Serialize)]
pub struct QueryArgs {
    pub vql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Response from `POST /query`.
#[derive(Debug, Deserialize)]
pub struct QueryResponse {
    pub success: bool,
    pub results: Vec<ScoredResult>,
    pub total: usize,
    pub error: Option<String>,
}

/// Body sent to `POST /insert`.
#[derive(Debug, Serialize)]
pub struct InsertArgs {
    pub id: u64,
    pub vector: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Response from `POST /insert`.
#[derive(Debug, Deserialize)]
pub struct InsertResponse {
    pub success: bool,
    pub id: u64,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// HTTP Client
// ---------------------------------------------------------------------------

/// Lightweight HTTP client for the Tesseract API.
///
/// Created once per session via [`crate::config::build_client`] or
/// [`TesseractClient::new`] with explicit host/port/timeout.
#[derive(Clone)]
pub struct TesseractClient {
    base_url: String,
    inner: reqwest::Client,
}

impl TesseractClient {
    /// Build a new client targeting `http://{host}:{port}` with the given
    /// timeout (milliseconds).
    pub fn new(host: &str, port: u16, timeout_ms: u64) -> Self {
        let base_url = format!("http://{}:{}", host, port);
        let inner = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
            .expect("reqwest Client construction should not fail with defaults");
        Self { base_url, inner }
    }

    /// Execute a VQL query against `POST /query`.
    ///
    /// Returns the list of scored results on success.
    pub async fn query(&self, vql: &str, user_id: Option<&str>) -> Result<Vec<ScoredResult>, ClientError> {
        let body = QueryArgs { vql: vql.to_string(), user_id: user_id.map(|s| s.to_string()) };

        let resp = self
            .inner
            .post(format!("{}/query", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| ClientError::ConnectionError(e.to_string()))?;

        let status = resp.status();
        let parsed: QueryResponse = resp.json().await.map_err(|e| ClientError::ParseError(e.to_string()))?;

        if status.is_success() && parsed.success {
            Ok(parsed.results)
        } else {
            Err(ClientError::RequestError {
                status: status.as_u16(),
                message: parsed.error.unwrap_or_else(|| "unknown error".into()),
            })
        }
    }

    /// Insert a vector with metadata via `POST /insert`.
    ///
    /// Returns the inserted `id` on success.
    pub async fn insert(
        &self,
        id: u64,
        vector: Vec<f64>,
        metadata: Option<serde_json::Value>,
    ) -> Result<u64, ClientError> {
        let body = InsertArgs { id, vector, metadata };

        let resp = self
            .inner
            .post(format!("{}/insert", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| ClientError::ConnectionError(e.to_string()))?;

        let status = resp.status();
        let parsed: InsertResponse = resp.json().await.map_err(|e| ClientError::ParseError(e.to_string()))?;

        if status.is_success() && parsed.success {
            Ok(parsed.id)
        } else {
            Err(ClientError::RequestError {
                status: status.as_u16(),
                message: parsed.error.unwrap_or_else(|| "unknown error".into()),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: spin up a wiremock server and a client pointing at it.
    async fn setup() -> (MockServer, TesseractClient) {
        let server = MockServer::start().await;
        let port = server.address().port();
        let client = TesseractClient::new("127.0.0.1", port, 5000);
        (server, client)
    }

    #[tokio::test]
    async fn test_query_returns_results() {
        let (server, client) = setup().await;

        let body = serde_json::json!({
            "success": true,
            "results": [
                {"id": 1, "score": 0.95, "metadata": {"title": "doc1"}},
                {"id": 2, "score": 0.87, "metadata": {"title": "doc2"}}
            ],
            "total": 2,
            "error": null
        });

        Mock::given(method("POST"))
            .and(path("/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let results = client.query("test vql", None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, 1);
        assert_eq!(results[0].score, 0.95);
        assert_eq!(results[1].metadata["title"], "doc2");
    }

    #[tokio::test]
    async fn test_query_connection_refused() {
        // Point at a closed port — no server running.
        let client = TesseractClient::new("127.0.0.1", 1, 1000);
        let result = client.query("test", None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ClientError::ConnectionError(_) => { /* expected */ }
            other => panic!("expected ConnectionError, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_query_server_error() {
        let (server, client) = setup().await;

        let body = serde_json::json!({
            "success": false,
            "results": [],
            "total": 0,
            "error": "dimension mismatch"
        });

        Mock::given(method("POST"))
            .and(path("/query"))
            .respond_with(ResponseTemplate::new(400).set_body_json(body))
            .mount(&server)
            .await;

        let err = client.query("bad vql", None).await.unwrap_err();
        match err {
            ClientError::RequestError { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message, "dimension mismatch");
            }
            other => panic!("expected RequestError, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_insert_success() {
        let (server, client) = setup().await;

        let body = serde_json::json!({
            "success": true,
            "id": 42,
            "error": null
        });

        Mock::given(method("POST"))
            .and(path("/insert"))
            .respond_with(ResponseTemplate::new(201).set_body_json(body))
            .mount(&server)
            .await;

        let id = client.insert(42, vec![0.1, 0.2, 0.3], None).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn test_insert_server_error() {
        let (server, client) = setup().await;

        let body = serde_json::json!({
            "success": false,
            "id": 1,
            "error": "dimension mismatch"
        });

        Mock::given(method("POST"))
            .and(path("/insert"))
            .respond_with(ResponseTemplate::new(400).set_body_json(body))
            .mount(&server)
            .await;

        let err = client.insert(1, vec![1.0], None).await.unwrap_err();
        match err {
            ClientError::RequestError { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message, "dimension mismatch");
            }
            other => panic!("expected RequestError, got: {other}"),
        }
    }

    #[test]
    fn test_serialize_query_args() {
        let args = QueryArgs { vql: "test".into(), user_id: Some("alice".into()) };
        let json = serde_json::to_value(&args).unwrap();
        assert_eq!(json["vql"], "test");
        assert_eq!(json["user_id"], "alice");
    }

    #[test]
    fn test_serialize_insert_args() {
        let meta = serde_json::json!({"key": "val"});
        let args = InsertArgs { id: 1, vector: vec![0.1, 0.2], metadata: Some(meta.clone()) };
        let json = serde_json::to_value(&args).unwrap();
        assert_eq!(json["id"], 1);
        assert_eq!(json["vector"], serde_json::json!([0.1, 0.2]));
        assert_eq!(json["metadata"], meta);
    }

    #[test]
    fn test_deserialize_query_response() {
        let raw = r#"{
            "success": true,
            "results": [
                {"id": 10, "score": 0.5, "metadata": {"title": "test"}}
            ],
            "total": 1,
            "error": null
        }"#;
        let resp: QueryResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.success);
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].id, 10);
        assert_eq!(resp.total, 1);
    }

    #[test]
    fn test_deserialize_insert_response() {
        let raw = r#"{"success": true, "id": 99, "error": null}"#;
        let resp: InsertResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.success);
        assert_eq!(resp.id, 99);
    }
}
