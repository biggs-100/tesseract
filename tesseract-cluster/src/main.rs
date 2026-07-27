// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract cluster node binary.
//!
//! Starts a single cluster node that participates in the Tesseract
//! distributed vector database. Each node operates as both a data node
//! and a query coordinator.

use std::sync::Arc;

use clap::Parser;
use tesseract_storage::{StorageEngine, types::*};

use tesseract_cluster::{ClusterNode, ClusterNodeConfig, NodeIdentity};

#[derive(Parser)]
#[command(name = "tesseract-cluster", version = "0.1.0", about = "Tesseract distributed cluster node")]
struct Cli {
    /// Unique node identifier used for registration and routing.
    #[arg(long, default_value = "node-1")]
    node_id: String,

    /// HTTP listen address (e.g. "127.0.0.1:9001").
    #[arg(long, default_value = "127.0.0.1:9001")]
    listen: String,

    /// Data directory for WAL, index, and storage tiers.
    #[arg(long, default_value = "./data")]
    data_dir: String,

    /// Query timeout in milliseconds.
    #[arg(long, default_value_t = 5000)]
    query_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let data_dir = std::path::PathBuf::from(&cli.data_dir);

    // Ensure the data directory exists.
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(data_dir.join("wal"))?;

    let storage_config = StorageConfig {
        wal: WalConfig { wal_dir: data_dir.join("wal"), ..Default::default() },
        index: IndexConfig { path: data_dir.join("index.hnsw"), ..Default::default() },
        hot: Default::default(),
        cold: Default::default(),
        cache: Default::default(),
        skeleton: Default::default(),
        lifecycle: Default::default(),
        topological: Default::default(),
        merkle: Default::default(),
        shutdown: ShutdownConfig::default(),
    };

    let storage = Arc::new(StorageEngine::open(storage_config).await?);

    let identity = NodeIdentity::new(&cli.node_id, &cli.listen);
    let config =
        ClusterNodeConfig { identity, http_addr: cli.listen.clone(), storage, query_timeout_ms: cli.query_timeout_ms };

    let node = ClusterNode::new(config);
    node.start().await?;

    Ok(())
}
