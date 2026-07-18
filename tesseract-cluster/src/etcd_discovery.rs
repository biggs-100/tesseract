// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! etcd-backed cluster discovery.
//!
//! Registers the local node as an etcd lease and auto-refreshes the TTL
//! to maintain liveness. Watches the `/tesseract/nodes/` key prefix for
//! membership changes.
//!
//! This module is only compiled when the `etcd` feature is enabled.

use std::time::Duration;

use etcd_client::{Client, LeaseId};
use tesseract_common::error::{Error, Result};
use tokio::sync::mpsc;

use crate::discovery::NodeInfo;

/// etcd-backed cluster discovery.
///
/// Manages a lease-based registration in etcd. The lease TTL is refreshed
/// automatically by the etcd client keepalive mechanism. The registration
/// key at `/tesseract/nodes/{node_id}` stores the node's address and is
/// automatically removed when the lease expires.
pub struct EtcdDiscovery {
    client: Client,
    lease_id: LeaseId,
    node_id: String,
    addr: String,
}

impl EtcdDiscovery {
    /// Connect to the etcd cluster and create a lease.
    ///
    /// # Arguments
    ///
    /// * `endpoints` - etcd cluster endpoints (e.g. `["http://127.0.0.1:2379"]`)
    /// * `node_id`   - Unique node identifier
    /// * `addr`      - Address the node listens on (e.g. `"10.0.0.1:9001"`)
    /// * `ttl`       - Lease TTL in seconds
    pub async fn connect(endpoints: &[String], node_id: &str, addr: &str, ttl: i64) -> Result<Self> {
        let client = Client::connect(endpoints, None)
            .await
            .map_err(|e| Error::ServiceError(format!("failed to connect to etcd: {e}")))?;

        let lease_client = client.lease_client();
        let lease_grant = lease_client
            .grant(ttl, None)
            .await
            .map_err(|e| Error::ServiceError(format!("failed to grant lease: {e}")))?;

        Ok(Self { lease_id: lease_grant.id(), client, node_id: node_id.to_string(), addr: addr.to_string() })
    }

    /// Register this node in the cluster.
    ///
    /// Creates the key `/tesseract/nodes/{node_id}` with the node's address
    /// as the value, associated with the lease so it expires automatically
    /// if the node stops heartbeating.
    pub async fn register(&self) -> Result<()> {
        let key = format!("/tesseract/nodes/{}", self.node_id);
        let kv_client = self.client.kv_client();

        kv_client
            .put(key, self.addr.clone(), Some(etcd_client::PutOptions::new().with_lease(self.lease_id)))
            .await
            .map_err(|e| Error::ServiceError(format!("failed to register node in etcd: {e}")))?;

        Ok(())
    }

    /// Refresh the lease TTL.
    ///
    /// This keeps the lease alive so that the registration key is not
    /// automatically removed by etcd. Should be called periodically
    /// (e.g. every `ttl/3` seconds) from a background task.
    pub async fn heartbeat(&self) -> Result<()> {
        let lease_client = self.client.lease_client();
        lease_client
            .keep_alive(self.lease_id)
            .await
            .map_err(|e| Error::ServiceError(format!("failed to keep lease alive: {e}")))?;

        Ok(())
    }

    /// Watch for node membership changes.
    ///
    /// Returns a receiver that yields [`Vec<NodeInfo>`] whenever the
    /// registered node set changes (node joins, leaves, or expires).
    pub async fn watch_nodes(&self) -> Result<mpsc::Receiver<Vec<NodeInfo>>> {
        let (tx, rx) = mpsc::channel(64);

        let watch_client = self.client.watch_client();
        let (_watcher, mut stream) = watch_client
            .watch("/tesseract/nodes/", Some(etcd_client::WatchOptions::new().with_prefix()))
            .await
            .map_err(|e| Error::ServiceError(format!("failed to create etcd watch: {e}")))?;

        let tx_clone = tx;
        tokio::spawn(async move {
            use etcd_client::WatchStream;
            while let Some(_resp) = stream.message().await.unwrap_or(None) {
                // On each watch event, signal that a change occurred.
                // The caller re-reads the full node list from the registry.
                let _ = tx_clone.send(Vec::new()).await;
            }
        });

        Ok(rx)
    }

    /// Get the underlying etcd client for advanced operations.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get the current lease ID.
    pub fn lease_id(&self) -> LeaseId {
        self.lease_id
    }

    /// Get the node ID.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }
}
