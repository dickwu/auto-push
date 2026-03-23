# auto-push

CLI tool that automates the git workflow: pull, stage, generate commit messages with AI, and push — in one command. Fully configurable via a pipeline of shell commands in `.auto-push.json`.

## Prerequisites

- [git](https://git-scm.com)
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

# Review before each step
auto-push --confirm

# Dry run — see what would happen
auto-push --dry-run

# Skip specific pipeline steps
auto-push --skip pull --skip tests

# Commit only, skip push
auto-push --skip push

# Use your own commit message (auto-skips AI generation)
auto-push -m "feat: add user auth"

# Override template variables from CLI
auto-push --var slack_channel=#urgent

# Use a different AI provider for this run
auto-push --provider codex

# Show the merged config for the current branch
auto-push --show-config
```

## Configuration

auto-push is configured via `.auto-push.json`. On first run, it auto-creates one with smart defaults — detecting your AI provider and project type.

### Config layering (like git config)

| Layer | Location | Purpose |
|-------|----------|---------|
| Built-in defaults | hardcoded | Conventional commits, standard pipeline |
| Global config | `~/.auto-push.json` | User-level preferences across all repos |
| Repo config | `<repo>/.auto-push.json` | Project-level settings |
| Branch overrides | `branches` key in repo config | Branch-specific rules |
| CLI flags | `--skip`, `--var`, `-m`, etc. | One-time overrides |

Each layer deep-merges into the previous. Arrays (like `pipeline`) are replaced, not merged.

### Pipeline

The `pipeline` array defines every step of the workflow. Each entry is a command that runs in order:

```json
{
  "pipeline": [
    { "name": "stash",    "run": "git stash push -m 'auto-push' || true" },
    { "name": "pull",     "run": "git pull" },
    { "name": "unstash",  "run": "git stash pop || true" },
    { "name": "tests",    "run": "cargo test" },
    { "name": "lint",     "run": "cargo clippy -- -D warnings" },
    { "name": "stage",    "run": "git add -A" },
    {
      "name": "generate",
      "command": "claude",
      "args": ["-p", "{{ diff }}", "--system-prompt", "{{ system_prompt }}", "--output-format", "text", "--no-session-persistence", "--tools", ""],
      "capture": "commit_message"
    },
    {
      "name": "commit",
      "run": "git commit -m '{{ commit_message }}'",
      "capture_after": [
        { "name": "commit_hash", "run": "git rev-parse --short HEAD" },
        { "name": "commit_summary", "run": "git log -1 --format=%s" }
      ]
    },
    {
      "name": "push",
      "run": "git push origin {{ branch }}",
      "on_error": "sleep 2 && git push origin {{ branch }}"
    }
  ]
}
```

### Two execution modes

**Shell mode** (`run` field) — command passed to `sh -c`. Good for simple commands:

```json
{ "name": "tests", "run": "cargo test" }
```

**Argv mode** (`command` + `args` fields) — arguments passed directly, no shell escaping issues. Recommended for AI providers where prompts contain quotes, newlines, and large diffs:

```json
{
  "name": "generate",
  "command": "claude",
  "args": ["-p", "{{ diff }}", "--system-prompt", "{{ system_prompt }}"],
  "capture": "commit_message"
}
```

### Command fields

| Field | Type | Description |
|---|---|---|
| `name` | string | Unique identifier (used by `--skip`) |
| `run` | string | Shell command (mutually exclusive with `command`) |
| `command` | string | Program to execute directly (mutually exclusive with `run`) |
| `args` | string[] | Arguments for `command` mode |
| `description` | string | Human-readable summary (auto-generated if omitted) |
| `capture` | string | Store stdout into a named template variable |
| `capture_after` | array | Run additional commands post-success to capture variables |
| `capture_mode` | string | What to capture: `"stdout"` (default), `"stderr"`, `"both"` |
| `on_error` | string | Shell command to run if the main command fails |
| `confirm` | string | Prompt for confirmation before running |
| `interactive` | bool | Give the command full TTY access (disables capture) |

### Template variables

Variables are available in `run`, `args`, `on_error`, and `confirm` fields via `{{ var_name }}` syntax.

**Built-in variables:**

| Variable | Description |
|---|---|
| `{{ branch }}` | Current branch name |
| `{{ remote }}` | Remote name (e.g. `origin`) |
| `{{ remote_url }}` | Remote URL |
| `{{ repo_root }}` | Repository root path |
| `{{ diff }}` | Staged diff (dynamic, recomputed after git changes) |
| `{{ diff_stat }}` | Staged diff stats |
| `{{ staged_files }}` | List of staged files |
| `{{ staged_count }}` | Number of staged files |
| `{{ system_prompt }}` | AI system prompt (from generate config) |
| `{{ style_suffix }}` | Commit style rules |

**Captured variables** — output from earlier pipeline commands:

| Variable | Source |
|---|---|
| `{{ commit_message }}` | Captured from `generate` command |
| `{{ commit_hash }}` | Captured via `capture_after` on `commit` |
| `{{ commit_summary }}` | Captured via `capture_after` on `commit` |

**User variables** — defined in config:

```json
{
  "vars": { "slack_channel": "#deploys", "team": "backend" },
  "pipeline": [
    { "name": "notify", "run": "echo 'Pushed by {{ team }} to {{ slack_channel }}'" }
  ]
}
```

### Structured variable access

**JSON dot-path** — access fields when a captured value is JSON:

```
{{ plan.0.message }}     → first element's "message" field
{{ result.status }}      → field on an object
{{ items.length }}       → array length
```

**Regex extraction** — extract parts of a value:

```
{{ version:/v(\d+\.\d+\.\d+)/ }}   → "1.2.3"
{{ output:/error: (.*)/ }}          → "file not found"
```

### Variable validation

auto-push validates all template variables at config load time:
- No duplicate variable names across built-ins, user vars, and captures
- Every `{{ var }}` reference must be resolvable at that point in the pipeline
- Forward references (using a var before the command that captures it) produce clear errors

### AI providers

The `generate` metadata section configures commit style and system prompts:

```json
{
  "generate": {
    "commit_style": {
      "format": "conventional",
      "types": ["feat", "fix", "refactor", "docs", "test", "chore", "perf", "ci"],
      "max_length": 72,
      "include_body": true
    },
    "max_diff_bytes": 20000
  }
}
```

The actual AI invocation is a pipeline command — you control exactly how it's called.

### Branch overrides

Override any config per branch using glob patterns:

```json
{
  "branches": {
    "main": { "pipeline": [] },
    "feature/*": {
      "generate": { "commit_style": { "max_length": 100 } }
    }
  }
}
```

### Legacy config migration

Old configs using `pre_push`/`after_push` arrays are automatically migrated to the `pipeline` format at runtime with a deprecation warning. Update your config to use `pipeline` directly.

## How it works

```
         ┌─────────────┐
         │  auto-push   │
         └──────┬───────┘
                │
         ┌──────▼───────┐
      1. │  Preflight     │  validate git state
         └──────┬───────┘
                │
         ┌──────▼───────┐
      2. │  Load config   │  .auto-push.json (auto-init if missing)
         └──────┬───────┘
                │
         ┌──────▼───────┐
      3. │  Build vars    │  static + dynamic + user vars
         └──────┬───────┘
                │
         ┌──────▼───────┐
      4. │  Pipeline      │  execute each command in order:
         │  engine        │  stash → pull → unstash → tests →
         │                │  stage → generate → commit → push
         └──────┬───────┘
                │
                ▼
             Done
```

Each pipeline step is a configurable shell command. Skip any step with `--skip <name>`.

## CLI Reference

| Flag | Description |
|---|---|
| `--skip <name>` | Skip a pipeline command by name (repeatable) |
| `--var <key>=<value>` | Override or add a template variable (repeatable) |
| `-m <message>` | Use a manual commit message (auto-skips generate) |
| `--dry-run` | Preview without executing |
| `--confirm` | Prompt before each step |
| `--force` | Auto-accept all confirmation prompts |
| `--provider <name>` | Override the generate command's provider |
| `--show-config` | Show merged config and exit |

**Deprecated flags** (kept for one major version):

| Old flag | Use instead |
|---|---|
| `--no-pull` | `--skip pull` |
| `--no-push` | `--skip push` |
| `--no-generate` | `--skip generate` |
| `--no-stash` | `--skip stash` |
| `--rebase` | Set `git pull --rebase` in your pipeline |

## Releasing

Tag a version to trigger the release CI:

```bash
git tag v0.1.0
git push origin v0.1.0
```

This builds binaries for macOS (x86_64, aarch64) and Linux (x86_64, aarch64) and creates a GitHub release.

## License

MIT
