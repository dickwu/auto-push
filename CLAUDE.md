# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**auto-push** is a Rust CLI tool that automates the git workflow: pull, stage, generate commit messages (via local `claude` CLI), commit, and push — all in one command.

Requires: `git`, `gh` (GitHub CLI), `claude` (Claude Code CLI, authenticated).

## Build & Development

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo run -- [args]          # Run with arguments
cargo test                   # Run all tests
cargo test test_name         # Run a single test
cargo clippy                 # Lint
cargo fmt                    # Format
cargo fmt -- --check         # Check formatting without modifying
```

## Architecture

Rust binary crate with three modules:

- `src/main.rs` — Entry point, CLI arg parsing (`clap` derive), orchestration flow
- `src/git.rs` — Git operations via `std::process::Command`, push via `gh` with `git push` fallback
- `src/claude.rs` — Invokes local `claude -p` CLI with diff to generate commit messages; uses simple prompt for clean pulls, detailed prompt when merge occurred

Flow: `git pull` → detect changes → `git add -A` → get diff → call `claude` CLI → `git commit` → push via `gh`

## CI/CD

- `.github/workflows/ci.yml` — Runs on push/PR to main: fmt, clippy, test, build
- `.github/workflows/release.yml` — Triggered by `v*` tags: builds for macOS (x86_64, aarch64) and Linux (x86_64, aarch64), creates GitHub release with tarballs + sha256
- `Formula/auto-push.rb` — Homebrew formula (sha256 placeholders updated after first release)
- `install.sh` — Cross-platform install script for Linux/macOS

## Releasing

```bash
git tag v0.1.0 && git push origin v0.1.0
```

After release CI completes, update sha256 hashes in `Formula/auto-push.rb`.

## Conventions

- Follow `cargo clippy` and `cargo fmt` defaults
- No `unwrap()` in non-test code — use `?` or explicit error handling
- Validate all external input (CLI args, git output, Claude CLI responses)
