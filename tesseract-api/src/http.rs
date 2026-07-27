// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HTTP API layer — Axum router, handlers, request/response types.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use tesseract_core::types::VectorId;
use tesseract_storage::engine::StorageEngine;
use tesseract_vql::executor::{QueryExecutor, ScoredResult};

use crate::auth::{AuthError, AuthProvider};

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

/// Shared application state injected into every handler via Axum's `State`.
#[derive(Clone)]
pub struct AppState {
    pub executor: Arc<QueryExecutor>,
    pub storage: Arc<StorageEngine>,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Query request body.
#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub vql: String,
    pub user_id: Option<String>,
}

/// Query response body.
#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub success: bool,
    pub results: Vec<ScoredResult>,
    pub total: usize,
    pub timings: Option<QueryTimings>,
    pub error: Option<String>,
}

/// Per-pipeline-stage timing breakdown, in milliseconds.
#[derive(Debug, Serialize)]
pub struct QueryTimings {
    pub parse_ms: f64,
    pub plan_ms: f64,
    pub search_ms: f64,
    pub total_ms: f64,
}

/// Insert request body.
#[derive(Debug, Deserialize)]
pub struct InsertRequest {
    pub id: u64,
    pub vector: Vec<f64>,
    pub metadata: Option<serde_json::Value>,
}

/// Insert response body.
#[derive(Debug, Serialize)]
pub struct InsertResponse {
    pub success: bool,
    pub id: u64,
    pub error: Option<String>,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Readiness check diagnostics.
#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub status: String,
    pub checks: std::collections::HashMap<String, bool>,
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

/// Auth middleware that checks every request except `/health/*` and `/metrics`.
async fn auth_middleware(
    Extension(auth): axum::Extension<Arc<Box<dyn AuthProvider>>>,
    mut req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Result<Response, Response> {
    let path = req.uri().path();
    if path.starts_with("/health") || path == "/metrics" {
        return Ok(next.run(req).await);
    }

    match auth.authenticate(req.headers()) {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(err) => {
            let (status, msg) = match err {
                AuthError::MissingCredentials => (StatusCode::UNAUTHORIZED, "missing credentials"),
                AuthError::InvalidCredentials(_) => (StatusCode::UNAUTHORIZED, "invalid credentials"),
                AuthError::ExpiredToken => (StatusCode::UNAUTHORIZED, "token expired"),
            };
            Err((status, Json(serde_json::json!({"error": msg}))).into_response())
        }
    }
}

use axum::Extension;

// ---------------------------------------------------------------------------
// Router builders
// ---------------------------------------------------------------------------

/// Build the axum [`Router`] with all HTTP endpoints.
///
/// No auth — use [`build_router_with_auth`] for authenticated deployments.
pub fn build_router(state: AppState) -> Router {
    build_router_with_auth(state, None)
}

/// Build the axum [`Router`] with optional authentication.
pub fn build_router_with_auth(
    state: AppState,
    auth_provider: Option<Box<dyn AuthProvider>>,
) -> Router {
    let public_routes = Router::new()
        .route("/health/liveness", get(liveness_handler))
        .route("/health/readiness", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        // Keep /health as alias for /health/liveness for backwards compat
        .route("/health", get(liveness_handler));

    let mut protected_routes = Router::new()
        .route("/query", post(query_handler))
        .route("/insert", post(insert_handler));

    if let Some(auth) = auth_provider {
        protected_routes = protected_routes
            .layer(middleware::from_fn(auth_middleware))
            .layer(axum::Extension(Arc::new(auth)));
    }

    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /health/liveness` — lightweight liveness probe.
async fn liveness_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "pass".to_string(), version: "0.1.0".to_string() })
}

/// `GET /health/readiness` — checks WAL, index, and HotBuffer status.
async fn readiness_handler(State(state): State<AppState>) -> impl IntoResponse {
    let checks = state.storage.is_ready();
    let all_ok = checks.values().all(|v| *v);
    let status = if all_ok { "pass" } else { "fail" };

    let http_status = if all_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (http_status, Json(ReadinessResponse { status: status.to_string(), checks }))
}

/// `GET /metrics` — basic metrics in Prometheus-like format.
///
/// When the `otel` feature is enabled, this exports full OpenTelemetry metrics.
/// Otherwise returns a lightweight set of counters.
async fn metrics_handler() -> impl IntoResponse {
    // Basic metrics stub — will be replaced by OTel exporter when feature is enabled.
    let body = "# Tesseract Metrics (stub)
# Enable the `otel` feature for full OpenTelemetry/Prometheus export.
tesseract_requests_total 0
tesseract_queries_total 0
tesseract_inserts_total 0
tesseract_errors_total 0
";
    (StatusCode::OK, [("content-type", "text/plain; charset=utf-8")], body)
}

/// `POST /query` — execute a VQL query and return scored results.
async fn query_handler(State(state): State<AppState>, Json(req): Json<QueryRequest>) -> impl IntoResponse {
    match state.executor.execute(&req.vql, req.user_id.as_deref()).await {
        Ok(result) => {
            let timings = result.timings;
            (
                StatusCode::OK,
                Json(QueryResponse {
                    success: true,
                    results: result.results,
                    total: result.total,
                    timings: Some(QueryTimings {
                        parse_ms: timings.parse_ms,
                        plan_ms: timings.plan_ms,
                        search_ms: timings.search_ms,
                        total_ms: timings.total_ms,
                    }),
                    error: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(QueryResponse {
                success: false,
                results: vec![],
                total: 0,
                timings: None,
                error: Some(e.to_string()),
            }),
        ),
    }
}

/// `POST /insert` — insert a vector with metadata.
async fn insert_handler(State(state): State<AppState>, Json(req): Json<InsertRequest>) -> impl IntoResponse {
    let metadata = req.metadata.unwrap_or(serde_json::json!({}));
    match state
        .storage
        .insert(VectorId(req.id), req.vector, metadata, tesseract_storage::types::WriteMode::Durable)
        .await
    {
        Ok(_) => (StatusCode::CREATED, Json(InsertResponse { success: true, id: req.id, error: None })),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(InsertResponse { success: false, id: req.id, error: Some(e.to_string()) }))
        }
    }
}
