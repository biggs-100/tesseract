// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract API server binary.
//!
//! Starts an HTTP (or HTTPS with `--features tls`) server exposing the VQL
//! query engine and vector storage over a REST API.
//!
//! ## Environment variables
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `TESSERACT_DATA_DIR` | `./data` | Data directory |
//! | `TESSERACT_LISTEN_ADDR` | `0.0.0.0:3000` | HTTP(S) bind address |
//! | `TESSERACT_AUTH_MODE` | `none` | Auth mode: `none`, `api-key`, `jwt`, `both` |
//! | `TESSERACT_API_KEYS` | — | Comma-separated `key:role` pairs |
//! | `TESSERACT_JWT_SECRET` | `dev-secret` | HMAC secret for JWT |
//! | `TESSERACT_RATE_LIMIT_RPM` | `100` | Max requests/min per IP |
//! | `TESSERACT_QUERY_TIMEOUT_SECS` | `30` | Implicit query timeout |
//! | `TESSERACT_LOG_FORMAT` | `text` | Log format: `text` or `json` |
//! | `TESSERACT_SHUTDOWN_TIMEOUT_SECS` | `30` | Shutdown drain timeout |
//! | `TESSERACT_TLS_CERT_PATH` | — | Path to TLS certificate (PEM) |
//! | `TESSERACT_TLS_KEY_PATH` | — | Path to TLS private key (PEM) |

use std::sync::Arc;

use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

use tesseract_api::auth::create_auth_provider;
use tesseract_api::http::{self, AppState};
use tesseract_api::rate_limiter::RateLimiter;
use tesseract_core::embedding::NoopEmbeddingService;
use tesseract_core::episodic::EpisodicMemory;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;
use tesseract_vql::executor::QueryExecutor;
use tesseract_vql::planner::PlannerConfig;

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        let mut int = signal::unix::signal(signal::unix::SignalKind::interrupt())
            .expect("failed to install SIGINT handler");
        tokio::select! {
            _ = term.recv() => info!("Received SIGTERM, starting shutdown..."),
            _ = int.recv() => info!("Received SIGINT, starting shutdown..."),
            _ = signal::ctrl_c() => info!("Received Ctrl+C, starting shutdown..."),
        }
    }
    #[cfg(not(unix))]
    {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
        info!("Received Ctrl+C, starting shutdown...");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging with configurable format.
    let log_format = std::env::var("TESSERACT_LOG_FORMAT").unwrap_or_else(|_| "text".into());
    match log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .init();
        }
    }

    // Load configuration from environment.
    let data_dir = std::env::var("TESSERACT_DATA_DIR").unwrap_or_else(|_| "./data".into());
    let listen_addr = std::env::var("TESSERACT_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());

    let shutdown_timeout = std::env::var("TESSERACT_SHUTDOWN_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);

    let query_timeout_secs = std::env::var("TESSERACT_QUERY_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);

    let rate_limit_rpm = std::env::var("TESSERACT_RATE_LIMIT_RPM")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(100);

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
        topological: Default::default(),
        merkle: Default::default(),
        shutdown: ShutdownConfig { timeout_secs: shutdown_timeout },
    };

    let storage = Arc::new(StorageEngine::open(storage_config).await?);
    let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn tesseract_core::embedding::EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());

    let planner_config = PlannerConfig::default();
    let executor = Arc::new(QueryExecutor::new(
        storage.clone(),
        embedder,
        episodic,
        planner_config,
        std::time::Duration::from_secs(query_timeout_secs),
    ));

    // Initialize auth provider.
    let auth_provider = create_auth_provider();
    if auth_provider.is_some() {
        info!("Authentication enabled");
    } else {
        info!("Authentication disabled (dev mode)");
    }

    // Initialize rate limiter.
    let rate_limiter = Arc::new(RateLimiter::new(rate_limit_rpm));
    info!("Rate limit: {rate_limit_rpm} req/min per IP");

    let state = AppState { executor: executor.clone(), storage: storage.clone() };
    let router = http::build_router_with_config(state, auth_provider, Some(rate_limiter));

    // Start the gRPC server in a background task when the `grpc` feature is enabled.
    #[cfg(feature = "grpc")]
    {
        let grpc_addr = std::env::var("TESSERACT_GRPC_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into());
        let grpc_executor = executor.clone();
        let grpc_storage = storage.clone();
        let grpc_addr_clone = grpc_addr.clone();
        let grpc_auth = create_auth_provider().map(|a| Arc::new(a));
        tokio::spawn(async move {
            if let Err(e) =
                tesseract_api::grpc::serve_grpc(&grpc_addr_clone, grpc_executor, grpc_storage, grpc_auth).await
            {
                tracing::error!("gRPC server error: {e}");
            }
        });
        tracing::info!("Tesseract gRPC listening on {}", grpc_addr);
    }

    let storage_for_shutdown = storage.clone();

    #[cfg(feature = "tls")]
    {
        let cert_path = std::env::var("TESSERACT_TLS_CERT_PATH").ok();
        let key_path = std::env::var("TESSERACT_TLS_KEY_PATH").ok();

        let addr: std::net::SocketAddr = listen_addr.parse()?;

        if let (Some(cert), Some(key)) = (cert_path, key_path) {
            tracing::info!("Tesseract API listening on https://{addr}");
            let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            let handle = axum_server::Handle::new();
            let server_handle = handle.clone();
            let shutdown = shutdown_signal();

            // Run the server in the background so we can await the signal.
            let server = tokio::spawn(async move {
                axum_server::bind_rustls(addr, tls_config)
                    .handle(server_handle)
                    .serve(router.into_make_service())
                    .await
            });

            // Wait for shutdown signal.
            shutdown.await;

            // Trigger graceful shutdown with timeout.
            handle.graceful_shutdown(Some(std::time::Duration::from_secs(shutdown_timeout)));
            server.await??;
        } else {
            tracing::info!("Tesseract API listening on http://{addr}");
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
    }

    #[cfg(not(feature = "tls"))]
    {
        tracing::info!("Tesseract API listening on http://{listen_addr}");
        let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    }

    // After axum stops accepting new connections, run storage engine shutdown.
    storage_for_shutdown.shutdown().await?;

    Ok(())
}
