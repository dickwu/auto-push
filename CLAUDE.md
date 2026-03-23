# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**auto-push** is a Rust CLI tool that automates the git workflow: pull, stage, generate commit messages (via configurable AI provider), commit, and push — all in one command.

Requires: `git`, `gh` (GitHub CLI), and an AI CLI (`claude`, `codex`, `ollama`, or any custom CLI).

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

Rust binary crate with these modules:

- `src/main.rs` — Entry point, CLI arg parsing (`clap` derive), orchestration flow
- `src/config.rs` — Config types (`AppConfig`, `GenerateConfig`, `ProviderConfig`), global+local layering via `serde_json::Value` deep merge, auto-init on first run with provider detection and `.gitignore` management, per-branch overrides via `globset`, provider presets (claude/codex/ollama), `deny_unknown_fields`
- `src/generate.rs` — AI provider dispatch with template-based args, style suffix injection into all prompts, structured output guard for non-Claude providers, configurable timeout, conflict resolution (Claude-only)
- `src/template.rs` — Shared template engine: `render_shell` (shell-escaped, for hooks) and `render_raw` (no escaping, for provider args)
- `src/hooks.rs` — Hook execution: pre_push/after_push command runner with confirm prompts, interactive mode, on_error handlers, output chaining via template variables
- `src/context.rs` — CLI flags, runtime context, and `AppConfig`
- `src/git.rs` — Git operations via `std::process::Command`, push via `gh` with `git push` fallback
- `src/push.rs` — Push logic with retry, protected branch detection, AI-assisted error recovery
- `src/pull.rs` — Pull with rebase support, conflict detection, provider-guarded conflict resolution
- `src/stage_commit.rs` — Staging, hunk-level commit splitting via AI provider
- `src/stash.rs` — Auto-stash/unstash around pull
- `src/submodule.rs` — Submodule sync, commit message generation via provider, push
- `src/preflight.rs` — Pre-run checks (git repo, remote, branch detection)
- `src/diff.rs` — Diff parsing and hunk extraction

Flow: `git pull` → detect changes → load config (auto-init if missing) → pre-push hooks → `git add -A` → get diff → call AI provider → `git commit` → push → after-push hooks

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
