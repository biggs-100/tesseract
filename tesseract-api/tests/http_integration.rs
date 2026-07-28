// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Integration tests for the HTTP API layer.
//!
//! Exercises the axum router, request parsing, handler error handling,
//! and response formatting against a live (in-process) server.
//!
//! Covers: query/insert endpoints, health checks, authentication
//! (API key + JWT), rate limiting, and public-route bypass.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::util::ServiceExt;

use tesseract_api::auth::{self, AuthProvider, Claims};
use tesseract_api::http::{self, AppState};
use tesseract_api::rate_limiter::RateLimiter;
use tesseract_core::embedding::EmbeddingService;
use tesseract_core::episodic::EpisodicMemory;
use tesseract_core::types::VectorId;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;
use tesseract_vql::executor::QueryExecutor;
use tesseract_vql::planner::PlannerConfig;

// ---------------------------------------------------------------------------
// Test embedding service — returns a fixed 4-dimensional vector
// ---------------------------------------------------------------------------

struct TestEmbeddingService;

#[async_trait::async_trait]
impl EmbeddingService for TestEmbeddingService {
    async fn embed(&self, _text: &str, _model: &str) -> tesseract_common::error::Result<Vec<f64>> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn test_storage_config(tmp: &tempfile::TempDir) -> StorageConfig {
    let root = tmp.path().to_path_buf();
    StorageConfig {
        wal: WalConfig { wal_dir: root.join("wal"), ..Default::default() },
        hot: HotStoreConfig { max_records: 200 },
        cold: ColdStoreConfig { data_dir: root.join("cold"), ..Default::default() },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        cache: PageCacheConfig { capacity: 100 },
        index: IndexConfig { enabled: true, dim: 4, hnsw: Default::default(), path: root.join("index.bin") },
        lifecycle: LifecycleConfig::default(),
        topological: Default::default(),
        merkle: Default::default(),
        shutdown: ShutdownConfig::default(),
    }
}

fn planner_config() -> PlannerConfig {
    PlannerConfig {
        default_ef_search: 50,
        dim: 4,
        estimated_vector_count: 100,
        cost_buffer: 0.0,
        cost_per_distance_ms: 0.000_001,
        topological_alpha: 0.3,
        merkle_enabled: false,
    }
}

/// Build an in-process axum router with a fresh storage engine, seeded
/// with 10 test vectors. Returns the router and a `TempDir` guard whose
/// lifetime is bound to the test scope.
async fn build_test_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());

    // Seed 10 4-dimensional vectors.
    for i in 0..10u64 {
        let v: Vec<f64> = (0..4).map(|d| 0.1 + (i as f64 * 0.1) + (d as f64 * 0.01)).collect();
        storage.insert(VectorId(i), v, serde_json::json!({"idx": i}), WriteMode::Fast).await.unwrap();
    }

    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(storage.clone(), embedder, episodic, planner_config(), std::time::Duration::from_secs(30)));
    let state = AppState { executor, storage };
    let app = http::build_router(state);

    (app, tmp)
}

/// Collect the full response body and deserialise it as JSON.
async fn collect_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_check_returns_200() {
    let (app, _tmp) = build_test_app().await;

    let response = app.oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap()).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = collect_json(response).await;
    assert_eq!(body["status"], "pass");
    assert_eq!(body["version"], "0.1.0");
}

#[tokio::test]
async fn query_with_valid_vql_returns_200() {
    let (app, _tmp) = build_test_app().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = collect_json(response).await;
    assert_eq!(body["success"], true);
    assert!(body["results"].is_array());
    assert!(body["total"].as_u64().is_some_and(|t| t <= 5));
    assert!(body["timings"].is_object());
}

#[tokio::test]
async fn query_with_invalid_vql_returns_400() {
    let (app, _tmp) = build_test_app().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"vql": "INVALID SYNTAX!!!"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = collect_json(response).await;
    assert_eq!(body["success"], false);
    assert!(body["error"].is_string());
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(err.contains("parse") || err.contains("error"), "expected parse error, got: {err}");
}

#[tokio::test]
async fn insert_valid_vector_returns_201() {
    let (app, _tmp) = build_test_app().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/insert")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id": 100, "vector": [0.1, 0.2, 0.3, 0.4], "metadata": {"title": "test"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let body = collect_json(response).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["id"], 100);
    assert!(body["error"].is_null());
}

#[tokio::test]
async fn insert_duplicate_id_handles_gracefully() {
    let (app, _tmp) = build_test_app().await;

    // First insert — should succeed.
    let body1 = r#"{"id": 200, "vector": [0.5, 0.5, 0.5, 0.5], "metadata": {"title": "first"}}"#;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/insert")
                .header("content-type", "application/json")
                .body(Body::from(body1))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // Second insert with the same ID — must not crash.
    let body2 = r#"{"id": 200, "vector": [0.9, 0.9, 0.9, 0.9], "metadata": {"title": "second"}}"#;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/insert")
                .header("content-type", "application/json")
                .body(Body::from(body2))
                .unwrap(),
        )
        .await
        .unwrap();

    // The storage engine should handle this gracefully — either 201 (silent
    // overwrite) or a client error code.
    assert!(
        response.status().is_success() || response.status().is_client_error(),
        "duplicate insert should not crash; got status {}",
        response.status(),
    );
}

// ---------------------------------------------------------------------------
// Authentication tests
// ---------------------------------------------------------------------------

/// Build an in-process app with API key auth enabled.
fn make_api_key_auth(keys_csv: &str) -> Option<Box<dyn AuthProvider>> {
    use std::collections::HashMap;
    let mut keys = HashMap::new();
    for entry in keys_csv.split(',') {
        if let Some((key, role)) = entry.split_once(':') {
            keys.insert(
                key.trim().to_string(),
                Claims { sub: key.trim().to_string(), role: role.trim().to_string() },
            );
        }
    }
    if keys.is_empty() { None } else { Some(Box::new(auth::ApiKeyAuth::new(keys))) }
}

fn make_jwt_auth(secret: &str) -> Option<Box<dyn AuthProvider>> {
    Some(Box::new(auth::JwtAuth::new(secret.to_string())))
}

async fn build_auth_app(api_keys: &str) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(
        storage.clone(),
        embedder,
        episodic,
        planner_config(),
        std::time::Duration::from_secs(30),
    ));

    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, make_api_key_auth(api_keys), None);
    (app, tmp)
}

fn auth_header(key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_str(key).unwrap());
    headers
}

#[tokio::test]
async fn auth_valid_api_key_allows_query() {
    let (app, _tmp) = build_auth_app("sk-valid:admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .header("x-api-key", "sk-valid")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_missing_key_returns_401() {
    let (app, _tmp) = build_auth_app("sk-valid:admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_invalid_key_returns_401() {
    let (app, _tmp) = build_auth_app("sk-valid:admin").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .header("x-api-key", "sk-wrong")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_bypasses_public_routes() {
    let (app, _tmp) = build_auth_app("sk-valid:admin").await;

    // /health/liveness must be accessible without auth
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/health/liveness").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // /health/readiness must be accessible without auth (may return 503
    // if some components are not ready, but never 401 Unauthorized)
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/health/readiness").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED, "readiness endpoint should not require auth");

    // /metrics must be accessible without auth
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    // /metrics returns 200 when router exists (may be empty if no OTel configured)
    assert!(resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Rate limiting tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limiter_accepts_under_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(storage.clone(), embedder, episodic, planner_config(), std::time::Duration::from_secs(30)));

    // Rate limiter set to 100 RPM — well above our 10 test requests.
    let limiter = Arc::new(RateLimiter::new(100));
    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, None, Some(limiter));

    for _ in 0..10 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn rate_limiter_rejects_over_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(storage.clone(), embedder, episodic, planner_config(), std::time::Duration::from_secs(30)));

    // Rate limiter set to exactly 5 RPM.
    let limiter = Arc::new(RateLimiter::new(5));
    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, None, Some(limiter));

    // First 5 requests should be OK
    for _ in 0..5 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 6th request should be rate-limited
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// Combined: auth + rate limiting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_and_rate_limit_together() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(
        storage.clone(),
        embedder,
        episodic,
        planner_config(),
        std::time::Duration::from_secs(30),
    ));

    // Enable both auth and rate limiting.
    let auth = make_api_key_auth("sk-test:admin");

    // Increase RPM to account for the pre-auth request.
    let limiter = Arc::new(RateLimiter::new(6));
    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, auth, Some(limiter));

    // Without auth → 401 (counts as one request to the rate limiter)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // With valid auth → rate limited after 5 more queries (total 6 RPM)
    let req = || {
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .header("x-api-key", "sk-test")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
    };

    for _ in 0..5 {
        assert_eq!(req().await.unwrap().status(), StatusCode::OK);
    }
    assert_eq!(req().await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// JWT auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_jwt_valid_token_allows_query() {
    use jsonwebtoken::{encode, EncodingKey, Header};

    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(
        storage.clone(),
        embedder,
        episodic,
        planner_config(),
        std::time::Duration::from_secs(30),
    ));

    let auth = make_jwt_auth("test-secret");

    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, auth, None);

    // Create a valid JWT
    let claims = serde_json::json!({
        "sub": "test-user",
        "role": "admin",
        "exp": 9999999999u64,
    });
    let token = encode(&Header::default(), &claims, &EncodingKey::from_secret("test-secret".as_ref())).unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_jwt_invalid_token_returns_401() {
    let tmp = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(StorageEngine::open(test_storage_config(&tmp)).await.unwrap());
    let embedder = Arc::new(TestEmbeddingService) as Arc<dyn EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());
    let executor = Arc::new(QueryExecutor::new(
        storage.clone(),
        embedder,
        episodic,
        planner_config(),
        std::time::Duration::from_secs(30),
    ));

    unsafe {
        std::env::set_var("TESSERACT_AUTH_MODE", "jwt");
        std::env::set_var("TESSERACT_JWT_SECRET", "test-secret");
    }
    let auth = auth::create_auth_provider();
    unsafe {
        std::env::remove_var("TESSERACT_AUTH_MODE");
        std::env::remove_var("TESSERACT_JWT_SECRET");
    }

    let state = AppState { executor, storage };
    let app = http::build_router_with_config(state, auth, None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/json")
                .header("authorization", "Bearer invalid-token")
                .body(Body::from(r#"{"vql": "FIND SIMILARITY(emb, 'test') LIMIT 5"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
