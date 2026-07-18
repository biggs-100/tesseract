// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HTTP API layer — Axum router, handlers, request/response types.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use tesseract_core::types::VectorId;
use tesseract_storage::engine::StorageEngine;
use tesseract_vql::executor::{QueryExecutor, ScoredResult};

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

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the axum [`Router`] with all HTTP endpoints using the given
/// application state.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/query", post(query_handler))
        .route("/insert", post(insert_handler))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /health` — simple liveness probe.
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok".to_string(), version: "0.1.0".to_string() })
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
