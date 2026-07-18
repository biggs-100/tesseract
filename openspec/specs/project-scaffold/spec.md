# Project Scaffold Specification

## Purpose

Define the Rust workspace structure, tooling configuration, CI pipeline, and licensing standards for the Tesseract project. This is the foundational build scaffold that all subsequent phases build upon.

## Requirements

### Requirement: Workspace compiles all six crates

The Rust workspace MUST compile `tesseract-core`, `tesseract-storage`, `tesseract-index`, `tesseract-vql`, `tesseract-api`, and `tesseract-common` as member crates. Each crate MUST expose a public `lib.rs` that compiles without errors.

#### Scenario: All crate stubs compile

- GIVEN a workspace root `Cargo.toml` listing all six crate paths as members
- WHEN `cargo build --workspace` is executed
- THEN all six crates compile without errors

### Requirement: CI pipeline runs build, clippy, and format

The CI pipeline (GitHub Actions) MUST execute `cargo build`, `cargo clippy --all-targets`, and `cargo fmt --check` on every push and pull request to the main branch.

#### Scenario: CI passes for a valid commit

- GIVEN a commit pushed to main
- WHEN the CI pipeline triggers
- THEN `cargo build`, `cargo clippy --all-targets`, and `cargo fmt --check` all pass

#### Scenario: CI fails on clippy warnings

- GIVEN a commit with clippy warnings
- WHEN the CI pipeline runs
- THEN the pipeline fails with clippy violation details

### Requirement: AGPL v3 license headers on all source files

Every `.rs` file in the workspace MUST carry an AGPL v3 license header as a block comment at the top of the file.

#### Scenario: New `.rs` file includes header

- GIVEN a new `.rs` file added to any crate
- WHEN the file is compiled
- THEN it contains the AGPL v3 license header

### Requirement: Zero-warning lint and format pass

`cargo clippy --all-targets` and `cargo fmt --check` MUST pass with zero warnings for all crates. Workspace `.rustfmt.toml` and `.clippy.toml` (or `clippy.toml`) MAY define project-wide settings.

#### Scenario: Clippy passes on clean code

- GIVEN the workspace with all crate code
- WHEN `cargo clippy --all-targets` is run
- THEN it exits with code 0 and produces zero warnings

### Requirement: Dependency license auditing

A `deny.toml` configuration MUST declare allowed licenses and block unapproved dependencies. The `cargo-deny` tool SHALL be used to audit the dependency graph.

#### Scenario: Approved license dependency passes

- GIVEN a dependency with an approved license
- WHEN `cargo deny check` is run
- THEN the check passes

#### Scenario: Unapproved license is rejected

- GIVEN a dependency with an unapproved license
- WHEN `cargo deny check` is run
- THEN the check fails with the violating license identified

### Requirement: Toolchain is stable 1.80+

The workspace MUST specify `rust-version = "1.80"` in the root `Cargo.toml` and the CI runner MUST use the stable 1.80+ toolchain.

#### Scenario: Minimum toolchain enforced

- GIVEN a Rust toolchain at version 1.80
- WHEN `cargo build` is executed
- THEN the workspace compiles without edition-related errors

### Requirement: Lockfile pins dependency versions

The workspace SHOULD commit `Cargo.lock` to version control to pin transitive dependency versions across environments.

#### Scenario: Deterministic builds from lockfile

- GIVEN a committed `Cargo.lock`
- WHEN `cargo build` is run on two different machines
- THEN both produce identical dependency trees
