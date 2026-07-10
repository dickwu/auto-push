# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**auto-push** is a Rust CLI tool that automates the git workflow: pull, stage, generate commit messages (via configurable AI provider), commit, and push — all in one command.

Requires: `git` and an AI CLI (`claude`, `codex`, `ollama`, or any custom CLI).

## Build & Development

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo run -- [args]          # Run with arguments
cargo test                   # Run all tests
cargo test test_name         # Run a single test
cargo clippy -- -D warnings  # Lint (CI fails on any warning)
cargo fmt                    # Format
cargo fmt -- --check         # Check formatting without modifying
```

## Architecture

Rust binary crate (edition 2024). Since v0.7.0 there is no hardcoded git workflow: the binary resolves a JSON **pipeline** (ordered list of shell/argv steps) from layered config and executes it. Default pipeline: `stash → pull → unstash → stage → generate → commit → push` (auto-init inserts a `tests` step when it detects a test command).

- `src/main.rs` — Entry, `clap` CLI; flow: preflight → load config (auto-init if missing) → build template vars → run pipeline
- `src/pipeline.rs` — Execution engine: shell (`sh -c`) vs argv steps, output capture modes, confirm prompts, `--skip`/`--dry-run`, interactive TTY passthrough with piped fallback for CI
- `src/config.rs` — Config types, deep-merge layering (built-in defaults → `~/.auto-push.json` → repo `.auto-push.json` → per-branch overrides via `globset` → CLI flags), heuristic auto-init with `.gitignore` management, provider presets; legacy `pre_push`/`after_push` keys are auto-migrated into `pipeline` with a deprecation warning
- `src/vars.rs` — Template variable registry + validation; `LazyVarResolver` shells out to git on demand
- `src/template.rs` — Template engine: `render_shell` (shell-escaped) and `render_raw` (unescaped)
- `src/generate.rs` — Builds the commit-message system prompt (style suffix injection); the AI call itself is just a pipeline step
- `src/smart_init.rs` — `--smart-init`: AI generates a repo-tailored pipeline config from a scan fingerprint; a `DANGEROUS_PATTERNS` denylist rejects unsafe AI-proposed commands before they're shown or written; interactive accept/edit/remove walkthrough
- `src/scan.rs` — Repo fingerprint scanner feeding smart-init (gitignore-aware walk, manifest/CI detection, credential redaction)
- `src/git.rs` — Git operations via `std::process::Command`
- `src/preflight.rs` — Pre-run checks (git repo, remote, branch detection)
- `src/context.rs` — `CliFlags`, `PreflightResult`

## CI/CD

- `.github/workflows/ci.yml` — Runs on push/PR to main: fmt, clippy, test, build
- `.github/workflows/release.yml` — Triggered by `v*` tags: builds macOS/Linux (x86_64 + aarch64) tarballs, creates the GitHub release, then auto-rewrites the Homebrew formula in the separate `dickwu/homebrew-tap` repo (via `HOMEBREW_TAP_TOKEN`)
- `Formula/auto-push.rb` — Vestigial local copy; the live formula lives in `dickwu/homebrew-tap` and is rewritten by CI each release — don't hand-edit sha256s here
- `install.sh` — Cross-platform install script for Linux/macOS

## Releasing

Bump `version` in `Cargo.toml`, commit (`chore: release vX.Y.Z`), then:

```bash
git tag vX.Y.Z && git push origin vX.Y.Z
```

Everything downstream is automated: binaries, GitHub release, Homebrew tap update. No manual sha256 edits.

## Conventions

- Follow `cargo clippy` and `cargo fmt` defaults
- No `unwrap()` in non-test code — use `?` or explicit error handling
- Validate all external input (CLI args, git output, AI provider responses)

## Gotchas

- CLI flags `--stage-all`, `--no-pre-push`, `--no-after-push`, `--no-hooks` are parsed but never read (dead since the pipeline rewrite); `--no-submodules` maps to `--skip submodules`, which matches no default step. Don't build on them.
- The `structured_output` config field is set but never read — dead knob.
- Deep merge replaces arrays wholesale: a repo config's `pipeline` fully overrides the default pipeline, no concatenation.
- `tests/integration.rs` spawns the real compiled binary against temp git repos, but `HOME` isn't isolated — a real `~/.auto-push.json` can leak into `cargo test` results.
- Known gaps and open design questions are tracked in `TODOS.md` — check it before re-litigating.
