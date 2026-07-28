// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Authentication module — `AuthProvider` trait with API key and JWT implementations.
//!
//! Supports three modes via `TESSERACT_AUTH_MODE`:
//! - `"none"` (default): no authentication required
//! - `"api-key"`: authenticate via `X-API-Key` header
//! - `"jwt"`: authenticate via `Authorization: Bearer <token>` (HS256)
//! - `"both"`: try both, accept if either succeeds

use std::collections::HashMap;

use axum::http::HeaderMap;

/// Claims extracted from a successful authentication.
#[derive(Debug, Clone)]
pub struct Claims {
    pub sub: String,
    pub role: String,
}

/// Authentication error types.
#[derive(Debug)]
pub enum AuthError {
    MissingCredentials,
    InvalidCredentials(String),
    ExpiredToken,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingCredentials => write!(f, "missing credentials"),
            AuthError::InvalidCredentials(msg) => write!(f, "invalid credentials: {msg}"),
            AuthError::ExpiredToken => write!(f, "token expired"),
        }
    }
}

/// Trait for authentication providers.
pub trait AuthProvider: Send + Sync {
    fn authenticate(&self, headers: &HeaderMap) -> Result<Claims, AuthError>;
}

// ---------------------------------------------------------------------------
// API Key Auth
// ---------------------------------------------------------------------------

/// API key authentication — reads keys from `TESSERACT_API_KEYS` env variable.
///
/// Format: `"key1:role1,key2:role2"`
pub struct ApiKeyAuth {
    keys: HashMap<String, Claims>,
}

impl ApiKeyAuth {
    /// Create from a pre-built key map (avoids env var dependency).
    pub fn new(keys: HashMap<String, Claims>) -> Self {
        Self { keys }
    }

    /// Create from `TESSERACT_API_KEYS` environment variable.
    pub fn from_env() -> Self {
        let csv = std::env::var("TESSERACT_API_KEYS").unwrap_or_default();
        let mut keys = HashMap::new();
        for entry in csv.split(',') {
            if let Some((key, role)) = entry.split_once(':') {
                keys.insert(
                    key.trim().to_string(),
                    Claims {
                        sub: key.trim().to_string(),
                        role: role.trim().to_string(),
                    },
                );
            }
        }
        Self { keys }
    }
}

impl AuthProvider for ApiKeyAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<Claims, AuthError> {
        let key = headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .ok_or(AuthError::MissingCredentials)?;
        self.keys
            .get(key)
            .cloned()
            .ok_or_else(|| AuthError::InvalidCredentials("invalid API key".into()))
    }
}

// ---------------------------------------------------------------------------
// JWT Auth
// ---------------------------------------------------------------------------

/// JWT authentication — verifies HS256 tokens using `TESSERACT_JWT_SECRET`.
pub struct JwtAuth {
    secret: String,
}

impl JwtAuth {
    /// Create from a pre-set secret (for testing).
    pub fn new(secret: String) -> Self {
        Self { secret }
    }

    /// Create from `TESSERACT_JWT_SECRET` environment variable.
    pub fn from_env() -> Self {
        let secret = std::env::var("TESSERACT_JWT_SECRET")
            .unwrap_or_else(|_| "dev-secret-do-not-use-in-prod".into());
        Self { secret }
    }
}

impl AuthProvider for JwtAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<Claims, AuthError> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AuthError::MissingCredentials)?;

        use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
        let token_data = decode::<serde_json::Value>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &Validation::new(Algorithm::HS256),
        )
        .map_err(|e| AuthError::InvalidCredentials(e.to_string()))?;

        let sub = token_data.claims["sub"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let role = token_data.claims["role"]
            .as_str()
            .unwrap_or("user")
            .to_string();
        Ok(Claims { sub, role })
    }
}

// ---------------------------------------------------------------------------
// Multi-auth — try multiple providers
// ---------------------------------------------------------------------------

/// Tries multiple auth providers in sequence, accepting the first success.
pub struct MultiAuth {
    providers: Vec<Box<dyn AuthProvider>>,
}

impl AuthProvider for MultiAuth {
    fn authenticate(&self, headers: &HeaderMap) -> Result<Claims, AuthError> {
        let mut last_err = AuthError::MissingCredentials;
        for provider in &self.providers {
            match provider.authenticate(headers) {
                Ok(claims) => return Ok(claims),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create an auth provider based on the `TESSERACT_AUTH_MODE` env variable.
///
/// Returns `None` for "none" mode (no authentication).
pub fn create_auth_provider() -> Option<Box<dyn AuthProvider>> {
    match std::env::var("TESSERACT_AUTH_MODE").as_deref() {
        Ok("api-key") => Some(Box::new(ApiKeyAuth::from_env())),
        Ok("jwt") => Some(Box::new(JwtAuth::from_env())),
        Ok("both") => Some(Box::new(MultiAuth {
            providers: vec![
                Box::new(ApiKeyAuth::from_env()) as Box<dyn AuthProvider>,
                Box::new(JwtAuth::from_env()) as Box<dyn AuthProvider>,
            ],
        })),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_auth_valid_key() {
        let mut keys = HashMap::new();
        keys.insert(
            "sk-abc".to_string(),
            Claims { sub: "sk-abc".to_string(), role: "admin".to_string() },
        );
        keys.insert(
            "sk-def".to_string(),
            Claims { sub: "sk-def".to_string(), role: "reader".to_string() },
        );
        let auth = ApiKeyAuth::new(keys);
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-abc".parse().unwrap());

        let result = auth.authenticate(&headers);
        assert!(result.is_ok());
        let claims = result.unwrap();
        assert_eq!(claims.sub, "sk-abc");
        assert_eq!(claims.role, "admin");
    }

    #[test]
    fn api_key_auth_invalid_key() {
        let mut keys = HashMap::new();
        keys.insert(
            "sk-abc".to_string(),
            Claims { sub: "sk-abc".to_string(), role: "admin".to_string() },
        );
        let auth = ApiKeyAuth::new(keys);
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-wrong".parse().unwrap());

        let result = auth.authenticate(&headers);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidCredentials(_)));
    }

    #[test]
    fn api_key_auth_missing_header() {
        let mut keys = HashMap::new();
        keys.insert(
            "sk-abc".to_string(),
            Claims { sub: "sk-abc".to_string(), role: "admin".to_string() },
        );
        let auth = ApiKeyAuth::new(keys);
        let headers = HeaderMap::new();

        let result = auth.authenticate(&headers);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::MissingCredentials));
    }

    #[test]
    fn jwt_auth_valid_token() {
        // Create a signed token
        let claims = serde_json::json!({
            "sub": "user123",
            "role": "admin",
            "exp": 9999999999u64,
        });
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret("test-secret".as_bytes()),
        )
        .expect("failed to create test JWT");

        let auth = JwtAuth::new("test-secret".to_string());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let result = auth.authenticate(&headers);
        assert!(result.is_ok());
        let claims = result.unwrap();
        assert_eq!(claims.sub, "user123");
        assert_eq!(claims.role, "admin");
    }

    #[test]
    fn jwt_auth_invalid_secret() {
        let claims = serde_json::json!({
            "sub": "user123",
            "role": "admin",
            "exp": 9999999999u64,
        });
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret("real-secret".as_bytes()),
        )
        .expect("failed to create test JWT");

        let auth = JwtAuth::new("wrong-secret".to_string());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let result = auth.authenticate(&headers);
        assert!(result.is_err());
    }

    #[test]
    fn jwt_auth_missing_bearer_prefix() {
        let auth = JwtAuth::new("test-secret".to_string());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "no-bearer-token".parse().unwrap());

        let result = auth.authenticate(&headers);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::MissingCredentials));
    }

    #[test]
    fn multi_auth_accepts_first_success() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "multi-key".parse().unwrap());

        let mut keys = HashMap::new();
        keys.insert(
            "multi-key".to_string(),
            Claims { sub: "multi-key".to_string(), role: "admin".to_string() },
        );
        let multi = MultiAuth {
            providers: vec![
                Box::new(ApiKeyAuth::new(keys)) as Box<dyn AuthProvider>,
                Box::new(JwtAuth::new("multi-secret".to_string())) as Box<dyn AuthProvider>,
            ],
        };

        let result = multi.authenticate(&headers);
        assert!(result.is_ok());
        let claims = result.unwrap();
        assert_eq!(claims.sub, "multi-key");
        assert_eq!(claims.role, "admin");
    }

    #[test]
    fn multi_auth_fails_when_all_fail() {
        let headers = HeaderMap::new(); // no credentials at all

        let mut keys = HashMap::new();
        keys.insert(
            "key".to_string(),
            Claims { sub: "key".to_string(), role: "admin".to_string() },
        );
        let multi = MultiAuth {
            providers: vec![
                Box::new(ApiKeyAuth::new(keys)) as Box<dyn AuthProvider>,
                Box::new(JwtAuth::new("secret".to_string())) as Box<dyn AuthProvider>,
            ],
        };

        let result = multi.authenticate(&headers);
        assert!(result.is_err());
    }

    #[test]
    fn create_auth_provider_defaults_to_none() {
        // When TESSERACT_AUTH_MODE is not set, should return None
        let provider = create_auth_provider();
        assert!(provider.is_none());
    }
}
