// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

/// Compile the protobuf definitions into Rust types using tonic-build
/// when the `grpc` feature is enabled.
fn main() {
    // tonic-build is an optional build-dependency enabled via the `grpc`
    // feature.  The cfg gate prevents compilation errors when it is absent.
    #[cfg(feature = "tonic-build")]
    {
        tonic_build::compile_protos("proto/tesseract.proto").expect("failed to compile protos");
    }
}
