# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Tesseract Contributors

# --------------- Build stage ---------------
FROM rust:1.85-slim-bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for improved layer caching
COPY Cargo.toml Cargo.lock* ./
COPY tesseract-common/Cargo.toml tesseract-common/
COPY tesseract-core/Cargo.toml tesseract-core/
COPY tesseract-storage/Cargo.toml tesseract-storage/
COPY tesseract-index/Cargo.toml tesseract-index/
COPY tesseract-vql/Cargo.toml tesseract-vql/
COPY tesseract-api/Cargo.toml tesseract-api/
COPY tesseract-cluster/Cargo.toml tesseract-cluster/
COPY tesseract-pg/Cargo.toml tesseract-pg/

# Create dummy source so Cargo can resolve and cache dependencies
RUN mkdir -p tesseract-common/src \
    tesseract-core/src \
    tesseract-storage/src \
    tesseract-index/src \
    tesseract-vql/src \
    tesseract-api/src \
    tesseract-cluster/src \
    tesseract-pg/src \
    && touch tesseract-common/src/lib.rs \
    tesseract-core/src/lib.rs \
    tesseract-storage/src/lib.rs \
    tesseract-index/src/lib.rs \
    tesseract-vql/src/lib.rs \
    tesseract-api/src/main.rs \
    tesseract-cluster/src/lib.rs \
    tesseract-pg/src/lib.rs \
    && echo 'fn main() {}' > tesseract-api/src/main.rs

# Build and cache dependencies (this layer is reused when source changes)
RUN cargo build --release -p tesseract-api 2>/dev/null; true

# Copy the full source tree
COPY . .

# Touch source to ensure a fresh build of the actual code
RUN touch tesseract-api/src/main.rs \
    tesseract-common/src/lib.rs \
    tesseract-core/src/lib.rs \
    tesseract-storage/src/lib.rs \
    tesseract-index/src/lib.rs \
    tesseract-vql/src/lib.rs \
    tesseract-cluster/src/lib.rs

# Build the release binary
RUN cargo build --release -p tesseract-api

# -------------- Runtime stage --------------
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/tesseract-server /usr/local/bin/tesseract-server

EXPOSE 3000

ENV TESSERACT_DATA_DIR=/data
ENV TESSERACT_LISTEN_ADDR=0.0.0.0:3000
ENV RUST_LOG=info

VOLUME ["/data"]

HEALTHCHECK --interval=15s --timeout=5s --retries=5 --start-period=20s \
    CMD curl -sf http://localhost:3000/health || exit 1

ENTRYPOINT ["tesseract-server"]
