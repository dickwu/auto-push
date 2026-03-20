# auto-push

CLI tool that automates the git workflow: pull, stage, generate commit messages with Claude, and push — in one command.

## Prerequisites

- [git](https://git-scm.com)
- [gh](https://cli.github.com) (GitHub CLI)
- [claude](https://claude.ai/code) (Claude Code CLI, must be authenticated)

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
```

## Hooks

auto-push supports running commands before and after pushing via a `.auto-push.json` config file. Pre-push hooks run after `git pull` so they validate the combined state of remote + local changes. After-push hooks run once the push succeeds.

### Setup

Generate a `.auto-push.json` config in your repo root:

```bash
auto-push --init-hooks
```

This detects your project type and creates sensible defaults:

- **Rust** — `cargo test`, `cargo clippy`, `cargo fmt --check`
- **Node.js** — `npm test`, `npm run lint`
- **Python** — `pytest`, `ruff check`
- **Go** — `go test ./...`, `go vet ./...`

View the current hook configuration:

```bash
auto-push --show-hooks
```

### Config format

```json
{
  "pre_push": [
    {
      "name": "tests",
      "description": "Run the project test suite",
      "run": "cargo test"
    },
    {
      "name": "lint",
      "description": "Check for common mistakes and style issues",
      "run": "cargo clippy -- -D warnings",
      "on_error": "echo 'Lint failed — fix warnings before pushing'"
    }
  ],
  "after_push": [
    {
      "name": "notify",
      "description": "Print a summary of the push",
      "run": "echo 'Pushed {{ branch }} ({{ commit_hash }})'"
    }
  ]
}
```

Each command has a `name`, an optional `description`, a `run` string, and an optional `on_error` handler. Pre-push commands run sequentially — if any fails, the push is aborted. After-push commands continue even if one fails.

#### Command fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique identifier within the phase |
| `description` | string | no | Human-readable summary (shown by `--show-hooks`) |
| `run` | string | yes | Shell command to execute (supports template variables) |
| `on_error` | string | no | Shell command to run if `run` fails |
| `confirm` | string | no | Prompt the user for confirmation before running (supports templates) |
| `interactive` | bool | no | Give the command full TTY access for stdin/stdout/stderr |

### Confirmation prompts

Add a `confirm` field to gate a command on user approval:

```json
{
  "name": "deploy",
  "confirm": "Deploy {{ branch }} to production?",
  "run": "deploy.sh {{ branch }}"
}
```

The confirm message supports the same `{{ variable }}` templates as `run`. Behavior:

- **User declines a pre-push confirm** — push is aborted
- **User declines an after-push confirm** — that command is skipped, remaining hooks continue
- **`--force`** — all confirms are auto-accepted
- **No TTY (CI)** — all confirms are auto-accepted (logged for auditability)
- **`--dry-run`** — the confirm message is printed but no prompt is shown

### Interactive commands

Set `interactive` to give a command full terminal access (stdin, stdout, stderr inherited). Use this for tools that need user input during execution:

```json
{
  "name": "select-target",
  "run": "interactive-deploy-picker",
  "interactive": true
}
```

When `interactive` is true, the command's output is **not captured** — `{{ command_output.NAME }}` will be empty for that command. If no TTY is available (e.g. CI), the command falls back to piped mode with captured output.

You can combine `confirm` and `interactive`:

```json
{
  "name": "manual-deploy",
  "confirm": "Run interactive deploy for {{ branch }}?",
  "run": "deploy-wizard",
  "interactive": true
}
```

### Template variables

Commands support `{{ variable }}` substitution:

| Variable | Description |
|---|---|
| `{{ branch }}` | Current branch name |
| `{{ remote }}` | Remote name (e.g. `origin`) |
| `{{ commit_hash }}` | HEAD commit hash |
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

1. Auto-stash dirty working tree (if needed)
2. `git pull` to sync with remote (with rebase if `--rebase`)
3. Sync submodules (if present)
4. Unstash changes
5. Run pre-push hooks (if `.auto-push.json` exists)
6. `git add -A` to stage everything
7. Get the diff and send it to Claude CLI for commit message generation
8. `git commit` with the generated message
9. Push via `gh` (falls back to `git push`)
10. Run after-push hooks

If the pull required a merge, Claude uses a more detailed prompt to describe the merge context. For clean pulls, it uses a simple single-line format.

## Releasing

Tag a version to trigger the release CI:

```bash
git tag v0.1.0
git push origin v0.1.0
```

This builds binaries for macOS (x86_64, aarch64) and Linux (x86_64, aarch64) and creates a GitHub release.

## License

MIT
