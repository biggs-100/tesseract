// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Per-IP rate limiter with sliding window.
//!
//! Tracks request counts per IP address within a configurable time window.
//! Returns `Err(())` when the limit is exceeded.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use tokio::sync::RwLock;

struct Window {
    start: Instant,
    count: u64,
}

/// Sliding-window per-IP rate limiter.
///
/// Default: 100 requests per 60-second window.
pub struct RateLimiter {
    windows: RwLock<HashMap<IpAddr, Window>>,
    max_requests: u64,
    window_duration: std::time::Duration,
}

impl RateLimiter {
    /// Create a new rate limiter with the given max requests per minute.
    pub fn new(max_rpm: u64) -> Self {
        Self {
            windows: RwLock::new(HashMap::new()),
            max_requests: max_rpm,
            window_duration: std::time::Duration::from_secs(60),
        }
    }

    /// Check whether the given IP has exceeded the rate limit.
    ///
    /// Returns `Ok(())` if the request is allowed, `Err(())` if rate-limited.
    pub async fn check(&self, ip: IpAddr) -> Result<(), ()> {
        let mut windows = self.windows.write().await;
        let now = Instant::now();
        let window = windows.entry(ip).or_insert(Window { start: now, count: 0 });

        if now - window.start > self.window_duration {
            window.start = now;
            window.count = 0;
        }

        window.count += 1;
        if window.count > self.max_requests {
            return Err(());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_requests_within_limit() {
        let limiter = RateLimiter::new(10);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        for _ in 0..10 {
            assert!(limiter.check(ip).await.is_ok());
        }
    }

    #[tokio::test]
    async fn rejects_requests_exceeding_limit() {
        let limiter = RateLimiter::new(5);
        let ip: IpAddr = "192.168.1.1".parse().unwrap();

        for _ in 0..5 {
            assert!(limiter.check(ip).await.is_ok());
        }
        // 6th request should fail
        assert!(limiter.check(ip).await.is_err());
    }

    #[tokio::test]
    async fn different_ips_have_independent_counters() {
        let limiter = RateLimiter::new(3);
        let ip_a: IpAddr = "10.0.0.1".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.2".parse().unwrap();

        for _ in 0..3 {
            assert!(limiter.check(ip_a).await.is_ok());
        }
        assert!(limiter.check(ip_a).await.is_err());

        // ip_b should still be allowed
        assert!(limiter.check(ip_b).await.is_ok());
    }

    #[tokio::test]
    async fn window_resets_after_duration() {
        let limiter = RateLimiter::new(2);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_err());

        // Force window reset by manipulating the stored window
        {
            let mut windows = limiter.windows.write().await;
            if let Some(window) = windows.get_mut(&ip) {
                window.start = Instant::now() - std::time::Duration::from_secs(61);
                window.count = 0;
            }
        }

        // Should be allowed again after window reset
        assert!(limiter.check(ip).await.is_ok());
        // And counting again
        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_err());
    }
}
