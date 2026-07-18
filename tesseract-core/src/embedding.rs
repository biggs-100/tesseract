// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use async_trait::async_trait;
use tesseract_common::error::Result;

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
        Err(tesseract_common::error::Error::ServiceError(
            "No embedding service configured. Provide a vector directly or configure an embedding provider.".into(),
        ))
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
}

#[cfg(feature = "openai-embedding")]
impl OpenAIEmbeddingService {
    pub fn new(api_key: String, endpoint: Option<String>) -> Self {
        Self {
            api_key,
            endpoint: endpoint.unwrap_or_else(|| "https://api.openai.com/v1/embeddings".into()),
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "openai-embedding")]
#[async_trait]
impl EmbeddingService for OpenAIEmbeddingService {
    async fn embed(&self, text: &str, model: &str) -> Result<Vec<f64>> {
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
                tesseract_common::error::Error::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            tesseract_common::error::Error::ServiceError(format!("Failed to parse embedding response: {e}"))
        })?;

        let data = body["data"][0]["embedding"].as_array().ok_or_else(|| {
            tesseract_common::error::Error::ServiceError("Unexpected embedding response format".into())
        })?;

        let vector: Vec<f64> = data.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();

        Ok(vector)
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
