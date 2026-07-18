// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HTTP client for sending replication entries to followers.
//!
//! Wraps `reqwest::Client` with a configurable timeout and sends
//! batches of [`ReplicationEntry`] to the follower's `/internal/replicate`
//! endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tesseract_common::error::Result;

use crate::replication::ReplicationEntry;

/// Response from a replication request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationResponse {
    pub success: bool,
    pub last_acked_txn_id: u64,
    pub error: Option<String>,
}

/// HTTP client for sending replication entries to followers.
pub struct ReplicationClient {
    client: reqwest::Client,
    timeout: Duration,
}

impl ReplicationClient {
    /// Create a new `ReplicationClient` with the given timeout (ms).
    pub fn new(timeout_ms: u64) -> Self {
        Self { client: reqwest::Client::new(), timeout: Duration::from_millis(timeout_ms) }
    }

    /// Send replication entries to a follower node.
    ///
    /// Entries are JSON-serialized and POSTed to the follower's
    /// `/internal/replicate` endpoint.
    pub async fn replicate(&self, follower_addr: &str, entries: &[ReplicationEntry]) -> Result<ReplicationResponse> {
        let url = format!("http://{}/internal/replicate", follower_addr);

        let resp = self.client.post(&url).json(entries).timeout(self.timeout).send().await.map_err(|e| {
            tesseract_common::error::Error::ServiceError(format!("replication request to {follower_addr} failed: {e}"))
        })?;

        let rep_resp: ReplicationResponse = resp.json().await.map_err(|e| {
            tesseract_common::error::Error::ServiceError(format!(
                "failed to parse replication response from {follower_addr}: {e}"
            ))
        })?;

        Ok(rep_resp)
    }
}

// ─── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replication_response_serde_roundtrip() {
        let resp = ReplicationResponse { success: true, last_acked_txn_id: 100, error: None };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ReplicationResponse = serde_json::from_str(&json).unwrap();

        assert!(decoded.success);
        assert_eq!(decoded.last_acked_txn_id, 100);
        assert!(decoded.error.is_none());
    }

    #[test]
    fn replication_response_with_error() {
        let resp = ReplicationResponse { success: false, last_acked_txn_id: 42, error: Some("storage error".into()) };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ReplicationResponse = serde_json::from_str(&json).unwrap();

        assert!(!decoded.success);
        assert_eq!(decoded.last_acked_txn_id, 42);
        assert_eq!(decoded.error.as_deref(), Some("storage error"));
    }
}
