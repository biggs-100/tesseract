// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract API server binary.
//!
//! Starts an HTTP server exposing the VQL query engine and vector storage
//! over a REST API. Configured via environment variables:
//!
//! - `TESSERACT_DATA_DIR` — data directory (default: `./data`)
//! - `TESSERACT_LISTEN_ADDR` — bind address (default: `0.0.0.0:3000`)

use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use tesseract_api::http::{self, AppState};
use tesseract_core::embedding::NoopEmbeddingService;
use tesseract_core::episodic::EpisodicMemory;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;
use tesseract_vql::executor::QueryExecutor;
use tesseract_vql::planner::PlannerConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Load configuration from environment.
    let data_dir = std::env::var("TESSERACT_DATA_DIR").unwrap_or_else(|_| "./data".into());
    let listen_addr = std::env::var("TESSERACT_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());

    let storage_config = StorageConfig {
        wal: WalConfig { wal_dir: std::path::PathBuf::from(&data_dir).join("wal"), ..Default::default() },
        hot: HotStoreConfig { max_records: 100_000 },
        cold: ColdStoreConfig { data_dir: std::path::PathBuf::from(&data_dir).join("cold"), ..Default::default() },
        cache: PageCacheConfig { capacity: 1000 },
        skeleton: SkeletonConfig { wake_threshold: 0.5 },
        lifecycle: LifecycleConfig {
            promote_interval_secs: 3600,
            demote_interval_secs: 3600,
            hot_max_records: 100_000,
            cold_min_access: 5,
        },
        index: IndexConfig {
            enabled: true,
            dim: 384,
            hnsw: Default::default(),
            path: std::path::PathBuf::from(&data_dir).join("index.hnsw"),
        },
    };

    let storage = Arc::new(StorageEngine::open(storage_config).await?);
    let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn tesseract_core::embedding::EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());

    let planner_config = PlannerConfig::default();
    let executor = Arc::new(QueryExecutor::new(storage.clone(), embedder, episodic, planner_config));

    let state = AppState { executor, storage };
    let router = http::build_router(state);

    tracing::info!("Tesseract API listening on {}", listen_addr);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
