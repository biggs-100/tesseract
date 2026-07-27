// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Per-IP rate limiter with sliding window (stub for A6).
//!
//! Full implementation added in A7.

use std::net::IpAddr;

/// Placeholder — will be replaced by full implementation in A7.
pub struct RateLimiter;

impl RateLimiter {
    pub fn new(_max_rpm: u64) -> Self {
        Self
    }

    pub async fn check(&self, _ip: IpAddr) -> Result<(), ()> {
        Ok(())
    }
}
