// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use std::time::Duration;

use async_trait::async_trait;
use tesseract_common::error::{Error, Result};

/// Pluggable text-to-vector embedding service.
#[async_trait]
pub trait EmbeddingService: Send + Sync {
    /// Convert text to a vector using the specified model.
    async fn embed(&self, text: &str, model: &str) -> Result<Vec<f64>>;
}

/// No-op embedding service that returns an error.
/// Used when no embedding provider is configured.
pub struct NoopEmbeddingService;

#[async_trait]
impl EmbeddingService for NoopEmbeddingService {
    async fn embed(&self, _text: &str, _model: &str) -> Result<Vec<f64>> {
        Err(Error::ServiceError(
            "No embedding service configured. Provide a vector directly or configure an embedding provider.".into(),
        ))
    }
}

/// Configuration for the OpenAI embedding HTTP client.
///
/// Controls per-request timeout, retry policy, and backoff.
#[derive(Debug, Clone)]
#[cfg(feature = "openai-embedding")]
pub struct OpenAIEmbeddingConfig {
    /// Per-request timeout in seconds (default: 30).
    pub timeout_secs: u64,
    /// Maximum number of retries on retryable HTTP status codes (429, 5xx).
    pub max_retries: u32,
    /// Base delay in milliseconds for exponential backoff.
    /// Actual delay = base_delay_ms * 2^(attempt - 1).
    pub base_delay_ms: u64,
}

#[cfg(feature = "openai-embedding")]
impl Default for OpenAIEmbeddingConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_retries: 3,
            base_delay_ms: 1000,
        }
    }
}

/// OpenAI-compatible embedding service.
///
/// Only available when the `openai-embedding` feature is enabled.
#[cfg(feature = "openai-embedding")]
pub struct OpenAIEmbeddingService {
    api_key: String,
    endpoint: String,
    client: reqwest::Client,
    config: OpenAIEmbeddingConfig,
}

#[cfg(feature = "openai-embedding")]
impl OpenAIEmbeddingService {
    /// Create a new `OpenAIEmbeddingService`.
    ///
    /// Reads `TESSERACT_EMBEDDING_TIMEOUT_SECS` (default 30) and
    /// `TESSERACT_EMBEDDING_RETRY_MAX` (default 3) from the environment.
    pub fn new(api_key: String, endpoint: Option<String>) -> Self {
        let timeout_secs = std::env::var("TESSERACT_EMBEDDING_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        let max_retries = std::env::var("TESSERACT_EMBEDDING_RETRY_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);

        let base_delay_ms = 1000;

        let config = OpenAIEmbeddingConfig { timeout_secs, max_retries, base_delay_ms };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("reqwest::Client::builder() should not fail with default options");

        Self {
            api_key,
            endpoint: endpoint.unwrap_or_else(|| "https://api.openai.com/v1/embeddings".into()),
            client,
            config,
        }
    }

    /// Internal method: perform a single HTTP call to the OpenAI API.
    async fn call_openai(&self, text: &str, model: &str) -> Result<Vec<f64>> {
        let resp = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&serde_json::json!({
                "input": text,
                "model": model,
            }))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    Error::ServiceError(format!("Embedding request timed out after {}s", self.config.timeout_secs))
                } else {
                    Error::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                }
            })?;

        let status = resp.status();
        if status.is_server_error() || status.as_u16() == 429 {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(Error::ServiceError(format!("Embedding rate limited (429): {body}")));
            }
            return Err(Error::ServiceError(format!("Embedding server error ({}): {body}", status.as_u16())));
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::ServiceError(format!("Embedding request failed ({}): {body}", status.as_u16())));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            Error::ServiceError(format!("Failed to parse embedding response: {e}"))
        })?;

        let data = body["data"][0]["embedding"].as_array().ok_or_else(|| {
            Error::ServiceError("Unexpected embedding response format".into())
        })?;

        let vector: Vec<f64> = data.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();

        Ok(vector)
    }

    /// Return `true` if this status code should trigger a retry.
    fn is_retryable_status(status: u16) -> bool {
        status == 429 || (500..=599).contains(&status)
    }
}

#[cfg(feature = "openai-embedding")]
#[async_trait]
impl EmbeddingService for OpenAIEmbeddingService {
    async fn embed(&self, text: &str, model: &str) -> Result<Vec<f64>> {
        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay_ms = self.config.base_delay_ms * 2u64.pow(attempt - 1);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            match self.call_openai(text, model).await {
                Ok(vec) => return Ok(vec),
                Err(e) => {
                    // Check if the error is a retryable HTTP status.
                    let is_retryable = matches!(&e, Error::ServiceError(msg) if {
                        // Extract status code from error message for retryable check
                        msg.starts_with("Embedding rate limited") || msg.starts_with("Embedding server error")
                    });

                    if is_retryable && attempt < self.config.max_retries {
                        last_error = Some(e);
                    } else if is_retryable && attempt == self.config.max_retries {
                        last_error = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            Error::ServiceError("Embedding retries exhausted".into())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_embedding_returns_error() {
        let svc = NoopEmbeddingService;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(svc.embed("test", "model"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            tesseract_common::error::Error::ServiceError(msg) => {
                assert!(msg.contains("No embedding service configured"));
            }
            _ => panic!("Expected ServiceError, got {err}"),
        }
    }

    #[test]
    fn embedding_trait_is_object_safe() {
        // Verify the trait can be used as a trait object
        let svc: Box<dyn EmbeddingService> = Box::new(NoopEmbeddingService);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(svc.embed("test", "model"));
        assert!(result.is_err());
    }
}
