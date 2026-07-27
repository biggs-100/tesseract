# Contributing to Tesseract

Thank you for your interest in Tesseract! This document covers everything you
need to build, test, and contribute to the project.

---

## Table of Contents

- [Development Setup](#development-setup)
- [Building](#building)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Workflow](#pull-request-workflow)
- [PostgreSQL Extension](#postgresql-extension)
- [Release Process](#release-process)
- [License](#license)

---

## Development Setup

### Prerequisites

- **Rust** 1.85 or later (install via [rustup](https://rustup.rs/))
- **Cargo** (included with Rust)
- **Docker** and **Docker Compose** (for PostgreSQL extension tests and E2E)
- Optional — **PostgreSQL 16** with development headers (for `tesseract-pg`):

  ```bash
  # Ubuntu / Debian
  sudo apt-get install -y libpq-dev postgresql-server-dev-16

  # macOS (Homebrew)
  brew install postgresql@16
  ```

### Clone and verify

```bash
git clone https://github.com/tesseract-db/tesseract.git
cd tesseract
cargo check --workspace --exclude tesseract-pg
```

---

## Building

Build all workspace crates except the PG extension:

```bash
cargo build --workspace --exclude tesseract-pg
```

Build a specific crate:

```bash
cargo build -p tesseract-api --release
```

The server binary is `tesseract-server` and runs on `http://0.0.0.0:3000` by default:

```bash
./target/release/tesseract-server
```

### Docker

```bash
docker build -t tesseract-server .
docker run -p 3000:3000 tesseract-server
```

Or with Docker Compose (includes PostgreSQL):

```bash
docker compose up -d
```

---

## Testing

### All crates (excluding PG extension)

```bash
cargo test --workspace --exclude tesseract-pg
```

### Specific crate

```bash
cargo test -p tesseract-vql
cargo test -p tesseract-storage
```

### PG extension (requires `cargo-pgrx` and PostgreSQL headers)

```bash
cargo install cargo-pgrx --locked
cargo pgrx init --pg16
cargo pgrx test -p tesseract-pg
```

### PG extension unit tests (no PG runtime needed)

```bash
cargo test -p tesseract-pg --lib --no-default-features
```

### Lint and format

```bash
cargo clippy --all-targets --workspace --exclude tesseract-pg -- -D warnings
cargo fmt --check
```

### Dependency audit

```bash
cargo install cargo-deny --locked
cargo deny check licenses
cargo deny check advisories
```

---

## Code Style

- **Rust edition** — 2024
- **Formatting** — `cargo fmt` (run before every commit)
- **Linting** — `cargo clippy` with `-D warnings` (no warnings allowed)
- **SPDX headers** — Every source file must start with:

  ```rust
  // SPDX-License-Identifier: AGPL-3.0-only
  // SPDX-FileCopyrightText: 2026 Tesseract Contributors
  ```

- **Naming** — Follow Rust conventions: `snake_case` for functions/variables,
  `CamelCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- **Error handling** — Use `thiserror` for library crates, `anyhow` for
  binaries. Propagate errors with `?` — avoid `.unwrap()` and `.expect()` in
  production code.
- **Documentation** — All public items MUST have doc comments. Use `//!` for
  module-level docs, `///` for item docs.
- **Commit messages** — Use [Conventional Commits](https://www.conventionalcommits.org/):

  ```
  feat(scope): description
  fix(scope): description
  docs: description
  test(scope): description
  ```

---

## Pull Request Workflow

1. **Create an issue** — Discuss the change before working on it (unless it's
   a trivial fix).
2. **Fork and branch** — Create a feature branch from `main`:

   ```bash
   git checkout -b feat/my-feature
   ```

3. **Implement** — Follow the coding standards above.
4. **Test** — Ensure all tests pass and no new warnings are introduced.
5. **Keep PRs small** — Aim for under 400 changed lines. Large changes should
   be split into stacked PRs.
6. **Open the PR** — Target `main`. Include a clear description of what the PR
   does and why.
7. **Review** — At least one maintainer must approve. Address all feedback.
8. **Merge** — Squash-merge into `main` with a clean commit message.

### PR Checklist

- [ ] All existing tests pass
- [ ] New code includes tests (unit and/or integration)
- [ ] Documentation updated (doc comments, README, or CHANGELOG as appropriate)
- [ ] `cargo clippy` reports no warnings
- [ ] `cargo fmt` has been run
- [ ] SPDX headers are present on new files
- [ ] No secrets or credentials committed

---

## PostgreSQL Extension

The `tesseract-pg/` crate is a [pgrx](https://github.com/pgcentralfoundation/pgrx)
extension. It proxies SQL function calls to the Tesseract HTTP API.

### Development cycle

```bash
# Start Tesseract
cargo run -p tesseract-api &

# Open a psql session with the extension loaded
cargo pgrx run -p tesseract-pg

# In psql:
CREATE EXTENSION tesseract_fdw;
SELECT tesseract_connect('localhost', 3000);
SELECT * FROM tesseract_query('FIND SIMILARITY(emb, [0.1, 0.2, 0.3]) LIMIT 5');
```

### Architecture

```
PostgreSQL (tesseract_fdw)
    │
    │  HTTP (reqwest)
    ▼
Tesseract API Server (tesseract-server)
    │
    ▼
Storage Engine + HNSW Index
```

The extension is a thin sidecar — every PG query translates to an HTTP
round-trip to the Tesseract process. This avoids in-process FFI complexity
and keeps the extension build independent of Tesseract's internal API.

---

## Release Process

Releases are automated via [release.yml](.github/workflows/release.yml) and
triggered by pushing a `v*` tag:

```bash
# Ensure CHANGELOG.md is up to date
git commit -m "chore: prepare vX.Y.Z"

# Tag and push
git tag vX.Y.Z
git push origin vX.Y.Z
```

The CI pipeline will:
1. Run all checks and tests.
2. Publish all workspace crates to [crates.io](https://crates.io) in
   dependency order.
3. Build and push a Docker image to
   [ghcr.io](https://ghcr.io).
4. Create a GitHub Release with the compiled binary.

---

## License

By contributing, you agree that your contributions will be licensed under
the [AGPL-3.0-only](LICENSE) license. You retain copyright over your
contributions and are free to use them elsewhere under different terms.
