# Embedding Service Specification

## Purpose

The embedding service provides a pluggable interface for converting text to vector embeddings, with a no-op fallback and an OpenAI-compatible implementation.

## Requirements

### Requirement: EmbeddingService trait

The system MUST define an `EmbeddingService` trait with method `embed(text: &str, model: &str) -> Result<Vec<f64>>`. The trait MUST be `Send + Sync` for use across async boundaries.

#### Scenario: Trait method called

- GIVEN an implementor of `EmbeddingService`
- WHEN `embed("quantum computing", "text-embedding-3-small")` is called
- THEN the call returns a `Vec<f64>` embedding vector of the configured dimension

### Requirement: NoopEmbedding returns error

A `NoopEmbedding` implementation MUST return an error indicating embedding is not configured.

#### Scenario: NoopEmbedding called

- GIVEN a system configured with `NoopEmbedding`
- WHEN `embed("text", "model")` is called
- THEN the call returns `Err(Error::EmbeddingNotConfigured)`
- AND the error message indicates the user must configure an embedding provider

### Requirement: OpenAIEmbedding implementation

An `OpenAIEmbedding` implementation SHOULD call an OpenAI-compatible API endpoint.

#### Scenario: OpenAIEmbedding calls API

- GIVEN an `OpenAIEmbedding` configured with endpoint URL, API key, and model name
- WHEN `embed("quantum computing", "text-embedding-3-small")` is called
- THEN the implementation sends an HTTP POST to the configured endpoint
- AND returns the parsed embedding vector from the API response

#### Scenario: OpenAI API returns error

- GIVEN an `OpenAIEmbedding` with an invalid API key
- WHEN `embed("text", "model")` is called
- THEN the implementation propagates the API error
- AND returns `Err` with a descriptive message

### Requirement: Dependency injection

The `EmbeddingService` MUST be injectable — the system must accept any implementor of the trait at runtime via dependency injection (e.g., `Arc<dyn EmbeddingService>`).

#### Scenario: Trait object injection

- GIVEN a system expecting an `Arc<dyn EmbeddingService>`
- WHEN a `NoopEmbedding` or `OpenAIEmbedding` is wrapped in `Arc`
- THEN the system accepts the implementation
- AND uses it for all text-to-embedding conversions during query execution

### Requirement: Configurable parameters

The service MUST support configurable model name, API key, and endpoint URL.

#### Scenario: OpenAIEmbedding configured via constructor

- GIVEN an `OpenAIEmbedding::new(endpoint, api_key, model)` constructor
- WHEN the service is instantiated
- THEN the instance uses the provided endpoint, key, and model for all API calls
- AND each parameter can be set independently
