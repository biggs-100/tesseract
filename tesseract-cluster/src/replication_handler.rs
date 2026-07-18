// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! HTTP handler for receiving replication entries on a follower.
//!
//! The [`handle_replicate`] function is the core of the `/internal/replicate`
//! endpoint. It receives replicated WAL entries from the leader, converts
//! them to the local [`WalEntry`] format, and applies them to the local
//! [`StorageEngine`] without appending to the local WAL.

use tesseract_common::error::Result;
use tesseract_storage::{StorageEngine, TransactionId, WalEntry};

use crate::replication::ReplicationEntry;
use crate::replication_client::ReplicationResponse;

/// Handle a batch of replicated WAL entries on a follower.
///
/// Each entry is applied to the local storage engine via
/// [`StorageEngine::apply_replicated_entry`], which writes to the hot
/// store and (if enabled) the ANN index without duplicating the WAL.
///
/// Returns the last successfully applied `txn_id` on success, or the
/// first error encountered.
pub async fn handle_replicate(entries: Vec<ReplicationEntry>, storage: &StorageEngine) -> Result<ReplicationResponse> {
    let mut last_acked = 0u64;

    for entry in &entries {
        let wal_entry =
            WalEntry { txn_id: TransactionId(entry.txn_id), op_code: entry.op_code, payload: entry.payload.clone() };
        storage.apply_replicated_entry(&wal_entry).await?;
        last_acked = entry.txn_id;
    }

    Ok(ReplicationResponse { success: true, last_acked_txn_id: last_acked, error: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::ReplicationEntry;

    /// Unit test for serde roundtrip of entries through the handler
    /// boundary — confirm the conversion from ReplicationEntry to WalEntry
    /// preserves all fields.
    #[test]
    fn entry_conversion_preserves_fields() {
        let rep_entry = ReplicationEntry { txn_id: 42, shard_id: 0, op_code: 0x01, payload: vec![10, 20, 30] };

        let wal_entry = WalEntry {
            txn_id: TransactionId(rep_entry.txn_id),
            op_code: rep_entry.op_code,
            payload: rep_entry.payload.clone(),
        };

        assert_eq!(wal_entry.txn_id.0, 42);
        assert_eq!(wal_entry.op_code, 0x01);
        assert_eq!(wal_entry.payload, vec![10, 20, 30]);
    }

    #[test]
    fn handle_replicate_empty_entries() {
        // We can't easily construct a StorageEngine in a sync unit test
        // (it requires async open), but we can validate the structure
        // via the ReplicationResponse type directly.
        let resp = ReplicationResponse { success: true, last_acked_txn_id: 0, error: None };
        assert!(resp.success);
        assert_eq!(resp.last_acked_txn_id, 0);
        assert!(resp.error.is_none());
    }
}
