// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract connection configuration.
//!
//! Defines the connection parameters used by [`TesseractClient`].
//! When running inside PostgreSQL, these are backed by GUC variables;
//! see [`crate::pg_entry`] for the GUC registration.

/// Connection parameters for the Tesseract HTTP endpoint.
#[derive(Debug, Clone)]
pub struct TesseractConfig {
    /// Tesseract server hostname or IP address.
    pub host: String,
    /// Tesseract server port.
    pub port: u16,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for TesseractConfig {
    fn default() -> Self {
        Self { host: "localhost".into(), port: 8081, timeout_ms: 5000 }
    }
}

impl TesseractConfig {
    /// Create a new config from explicit values.
    pub fn new(host: impl Into<String>, port: u16, timeout_ms: u64) -> Self {
        Self { host: host.into(), port, timeout_ms }
    }

    /// Return the base URL string (`http://host:port`).
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = TesseractConfig::default();
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 8081);
        assert_eq!(cfg.timeout_ms, 5000);
    }

    #[test]
    fn test_base_url() {
        let cfg = TesseractConfig::new("0.0.0.0", 3000, 1000);
        assert_eq!(cfg.base_url(), "http://0.0.0.0:3000");
    }
}
