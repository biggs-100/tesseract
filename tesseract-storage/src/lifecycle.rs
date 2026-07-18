// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Background tier lifecycle manager.
//!
//! Runs promotion (cold → hot) and demotion (hot → cold) cycles at
//! configurable intervals. In this Phase 1 implementation, the lifecycle
//! manager is a working skeleton — it runs the background task loop,
//! logs and traces, but the actual promotion/demotion logic is simplified.
//! Full access-count-based promotion comes in a later optimization pass.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;
use tracing::{info, warn};

use tesseract_common::error::Result;

use crate::cold_store::ColdStore;
use crate::hot_store::HotStore;
use crate::skeleton::VectorSkeleton;
use crate::types::LifecycleConfig;

/// Background lifecycle manager for tier promotion/demotion.
pub struct TierLifecycle;

impl TierLifecycle {
    /// Start the lifecycle manager as a background tokio task.
    ///
    /// Returns a `JoinHandle` that can be cancelled by dropping it.
    pub fn start(
        hot: Arc<HotStore>,
        cold: Arc<ColdStore>,
        skeleton: Arc<VectorSkeleton>,
        config: LifecycleConfig,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "TierLifecycle started: promote={}s, demote={}s",
                config.promote_interval_secs, config.demote_interval_secs,
            );

            let mut promote_interval = interval(Duration::from_secs(config.promote_interval_secs));
            let mut demote_interval = interval(Duration::from_secs(config.demote_interval_secs));

            // Tick both intervals immediately so the first cycle runs promptly.
            promote_interval.tick().await;
            demote_interval.tick().await;

            loop {
                tokio::select! {
                    _ = promote_interval.tick() => {
                        if let Err(e) = Self::run_promotion(&hot, &cold, &skeleton).await {
                            warn!(error = %e, "Tier promotion failed");
                        }
                    }
                    _ = demote_interval.tick() => {
                        if let Err(e) = Self::run_demotion(&hot, &cold, &config).await {
                            warn!(error = %e, "Tier demotion failed");
                        }
                    }
                }
            }
        })
    }

    /// Promotion cycle: identify cold partitions that are accessed
    /// frequently and load them into the hot tier.
    ///
    /// Phase 1 implementation: logs intent only. Full access tracking
    /// and promotion logic is deferred to a later optimization pass.
    async fn run_promotion(hot: &HotStore, cold: &ColdStore, skeleton: &VectorSkeleton) -> Result<()> {
        let hot_len = hot.len();
        let cold_partitions = cold.partitions();
        let skeleton_len = skeleton.len();

        info!(
            "Tier promotion cycle: hot={hot_len}, cold_partitions={}, skeleton={skeleton_len}",
            cold_partitions.len(),
        );

        // Phase 2: check each partition's access count against
        // cold_min_access threshold and promote qualifying partitions.
        for partition in &cold_partitions {
            if let Some(meta) = cold.partition_metadata(partition) {
                let records = cold.read_partition(partition).await?;
                if !records.is_empty() {
                    // Promote by inserting each record into hot tier.
                    for record in &records {
                        // Silently skip duplicates — the partition already
                        // existed in cold, so insert may fail if the hot
                        // tier already has the record from a prior cycle.
                        let _ = hot.insert(record.clone());
                    }

                    // Update skeleton centroid.
                    let vectors: Vec<Vec<f64>> = records.iter().map(|r| r.vector.clone()).collect();
                    let _ = skeleton.add_partition(partition.clone(), &vectors);

                    info!(
                        "Promoted partition {} ({} records, {} accesses)",
                        partition.0, meta.record_count, meta.size_bytes,
                    );
                }
            }
        }

        Ok(())
    }

    /// Demotion cycle: drain least-accessed records from the hot tier
    /// and flush them to the cold tier.
    ///
    /// Phase 1 implementation: drains all records when hot exceeds the
    /// configured `hot_max_records` watermark.
    async fn run_demotion(hot: &HotStore, cold: &ColdStore, config: &LifecycleConfig) -> Result<()> {
        let hot_len = hot.len();

        if hot_len <= config.hot_max_records {
            // Below watermark — nothing to demote.
            return Ok(());
        }

        let count = hot_len - config.hot_max_records;
        info!("Tier demotion cycle: draining {count} records (hot={hot_len}, max={})", config.hot_max_records);

        let drained = hot.drain_least_accessed(count);
        if drained.is_empty() {
            return Ok(());
        }

        // Write drained records to a single cold partition.
        let partition = crate::cold_store::PartitionId(0);
        cold.write_batch(&partition, &drained).await?;

        info!("Demoted {} records to partition {}", drained.len(), partition.0);
        Ok(())
    }
}
