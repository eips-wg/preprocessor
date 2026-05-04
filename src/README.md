# Preprocessor Module And Test Ownership

This file documents how the preprocessor crate is organized and where new
tests should live.

## Module Ownership

- `main.rs` owns top-level orchestration, build locking, and runtime dispatch.
- `changed.rs` owns changed-file command execution and proposal path filtering.
- `cli.rs` owns the clap command surface and command-specific argument types.
- `context.rs` owns command input path resolution.
- `layout.rs` owns shared build layout names.

## Test Ownership

New tests should generally live in the module that owns the behavior. Use
module-local `#[cfg(test)]` tests for focused parsing or helper behavior and
cross-module tests only when behavior spans several modules.
