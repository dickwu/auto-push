# auto-push

CLI tool that automates the git workflow: pull, stage, generate commit messages with AI, and push — in one command. Supports multiple AI providers (Claude, Codex, Ollama, or any custom CLI).

## Prerequisites

- [git](https://git-scm.com)
- [gh](https://cli.github.com) (GitHub CLI)
- An AI CLI: [claude](https://claude.ai/code) (default), [codex](https://github.com/openai/codex), [ollama](https://ollama.com), or any custom CLI

## Install

### macOS (Homebrew)

```bash
brew install dickwu/tap/auto-push
```

### Linux / macOS (script)

```bash
curl -fsSL https://raw.githubusercontent.com/dickwu/auto-push/main/install.sh | bash
```

Or install to a custom directory:

```bash
INSTALL_DIR=~/.local/bin curl -fsSL https://raw.githubusercontent.com/dickwu/auto-push/main/install.sh | bash
```

### From source

```bash
cargo install --git https://github.com/dickwu/auto-push
```

## Usage

```bash
# Pull, stage all, generate commit message, commit, and push
auto-push

# Review the generated message before committing
auto-push --confirm

# Dry run — see what would happen
auto-push --dry-run

# Commit only, skip push
auto-push --no-push

# Use your own commit message
auto-push -m "feat: add user auth"

# Pull with rebase instead of merge
auto-push --rebase

# Use a different AI provider for this run
auto-push --provider codex

# Skip AI generation (requires -m)
auto-push --no-generate -m "chore: manual commit"

# Show the merged config for the current branch
auto-push --show-config
```

## Configuration

auto-push is configured via `.auto-push.json`. On first run, it auto-creates one with smart defaults — detecting your AI provider and project type.

### Config layering (like git config)

| Layer | Location | Purpose |
|-------|----------|---------|
| Built-in defaults | hardcoded | Claude as provider, conventional commits |
| Global config | `~/.auto-push.json` | User-level preferences across all repos |
| Repo config | `<repo>/.auto-push.json` | Project-level settings |
| Branch overrides | `branches` key in repo config | Branch-specific rules |
| CLI flags | `--provider`, `--message`, etc. | One-time overrides |

Each layer deep-merges into the previous. Arrays (like hooks) are replaced, not merged.

### Full config example

```json
{
  "generate": {
    "provider": "claude",
    "commit_style": {
      "format": "conventional",
      "types": ["feat", "fix", "refactor", "docs", "test", "chore", "perf", "ci"],
      "max_length": 72,
      "include_body": true
    },
    "max_diff_bytes": 20000,
    "timeout_secs": 0
  },
  "pre_push": [
    {
      "name": "tests",
      "description": "Run the project test suite",
      "run": "cargo test"
    },
    {
      "name": "lint",
      "run": "cargo clippy -- -D warnings",
      "on_error": "echo 'Lint failed — fix warnings before pushing'"
    }
  ],
  "after_push": [
    {
      "name": "notify",
      "run": "echo 'Pushed {{ branch }} ({{ commit_hash }})'"
    }
  ],
  "branches": {
    "main": {
      "generate": {
        "commit_style": { "max_length": 50, "include_body": true }
      }
    },
    "feature/*": {
      "generate": {
        "commit_style": { "max_length": 100 }
      }
    }
  }
}
```

### AI providers

Switch providers with one field change:

```json
{ "generate": { "provider": "codex" } }
```

Built-in presets:

| Provider | Default | Structured output | Notes |
|----------|---------|-------------------|-------|
| `claude` | yes | yes | Full features: hunk splitting, push recovery, conflict resolution |
| `codex` | no | no | Commit message generation only |
| `ollama` | no | no | Requires `model` field (e.g. `"llama3"`) |

Custom provider:

```json
{
  "generate": {
    "provider": {
      "command": "my-ai-tool",
      "args": ["--prompt", "{{ prompt }}"],
      "model": "my-model"
    }
  }
}
```

Providers without structured output gracefully degrade: hunk-level commit splitting and push error recovery are disabled, and conflict resolution falls back to manual.

### Commit style

The `commit_style` config controls the style rules injected into AI prompts:

| Field | Default | Description |
|-------|---------|-------------|
| `format` | `"conventional"` | Commit format name |
| `types` | `["feat", "fix", ...]` | Allowed commit types |
| `max_length` | `72` | Max first-line length |
| `include_body` | `true` | Whether to include a commit body |

### Branch overrides

Override any config per branch using glob patterns:

```json
{
  "branches": {
    "main": { "generate": { "commit_style": { "max_length": 50 } } },
    "feature/*": { "pre_push": [] }
  }
}
```

An empty array `[]` disables all hooks for that branch. Patterns use [globset](https://docs.rs/globset) syntax.

## Hooks

Pre-push hooks run after `git pull` to validate the combined state. After-push hooks run once the push succeeds.

### Command fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique identifier within the phase |
| `description` | string | no | Human-readable summary (auto-generated if omitted) |
| `run` | string | yes | Shell command to execute (supports template variables) |
| `on_error` | string | no | Shell command to run if `run` fails |
| `confirm` | string | no | Prompt the user for confirmation before running |
| `interactive` | bool | no | Give the command full TTY access |

### Confirmation prompts

```json
{
  "name": "deploy",
  "confirm": "Deploy {{ branch }} to production?",
  "run": "deploy.sh {{ branch }}"
}
```

- **User declines a pre-push confirm** — push is aborted
- **User declines an after-push confirm** — that command is skipped
- **`--force`** — all confirms are auto-accepted
- **No TTY (CI)** — all confirms are auto-accepted

### Interactive commands

Set `interactive` for commands that need user input:

```json
{ "name": "select-target", "run": "interactive-deploy-picker", "interactive": true }
```

Output is not captured for interactive commands (`{{ command_output.NAME }}` will be empty).

### Template variables

| Variable | Description |
|---|---|
| `{{ branch }}` | Current branch name |
| `{{ remote }}` | Remote name (e.g. `origin`) |
| `{{ commit_hash }}` | HEAD commit hash |
| `{{ commit_summary }}` | Subject line of the latest commit (after-push only) |
| `{{ command_name }}` | Name of the current command |
| `{{ command_output.NAME }}` | Stdout of a previously run command |
| `{{ command_output.NAME \| /regex/ }}` | Regex extraction from a command's output |

### Skip hooks

```bash
auto-push --no-hooks        # Skip all hooks
auto-push --no-pre-push     # Skip pre-push hooks only
auto-push --no-after-push   # Skip after-push hooks only
```

## How it works

```
         ┌─────────────┐
         │  auto-push   │
         └──────┬───────┘
                │
         ┌──────▼───────┐
      1. │  Auto-stash   │  protect dirty working tree
         └──────┬───────┘
                │
         ┌──────▼───────┐
      2. │  git pull      │  sync with remote (--rebase optional)
         └──────┬───────┘
                │
         ┌──────▼───────┐
      3. │  Submodule     │  sync .gitmodules
         │  sync          │
         └──────┬───────┘
                │
         ┌──────▼───────┐
      4. │  Unstash       │  restore local changes
         └──────┬───────┘
                │
         ┌──────▼───────┐     ┌─────────────────┐
      5. │  Pre-push      │────▶  .auto-push.json  │
         │  hooks         │◀────  (confirm, etc.)  │
         └──────┬───────┘     └─────────────────┘
                │  bail on failure
         ┌──────▼───────┐     ┌─────────────────┐
      6. │  git add -A    │    │  AI provider     │
         │  → get diff    │────▶  (configurable)   │
      7. │  → gen message │◀────  claude / codex   │
         └──────┬───────┘     └─────────────────┘
                │
         ┌──────▼───────┐
      8. │  git commit    │
         └──────┬───────┘
                │
         ┌──────▼───────┐
      9. │  Push via gh   │  fallback to git push
         └──────┬───────┘
                │
         ┌──────▼───────┐
     10. │  After-push    │  {{ commit_summary }} available
         │  hooks         │
         └──────┬───────┘
                │
                ▼
             Done
```

## Releasing

Tag a version to trigger the release CI:

```bash
git tag v0.1.0
git push origin v0.1.0
```

This builds binaries for macOS (x86_64, aarch64) and Linux (x86_64, aarch64) and creates a GitHub release.

## License

MIT
