// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

// Phase 2+ — API layer (gRPC, HTTP, etc.)

pub mod http;

#[cfg(feature = "grpc")]
pub mod grpc;
