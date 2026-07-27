// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

// Phase 2+ — API layer (gRPC, HTTP, etc.)

pub mod auth;
pub mod http;
pub mod rate_limiter;

#[cfg(feature = "grpc")]
pub mod grpc;
