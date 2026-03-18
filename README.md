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

## Pre-push checks

auto-push supports running checks (tests, linting, etc.) before committing and pushing. Checks run after `git pull` so they validate the combined state of remote + local changes.

### Setup

Generate a `.pre-push.json` config in your repo root:

```bash
auto-push --init-pre-push
```

This detects your project type and creates a sensible default:

- **Rust** — `cargo test`, `cargo clippy`, `cargo fmt --check`
- **Node.js** — `npm test`, `npm run lint`
- **Python** — `pytest`, `ruff check`
- **Go** — `go test ./...`, `go vet ./...`

### Config format

```json
{
  "commands": [
    {
      "name": "tests",
      "run": "cargo test"
    },
    {
      "name": "lint",
      "run": "cargo clippy -- -D warnings"
    }
  ]
}
```

Commands run sequentially. If any command fails, the push is aborted (your changes remain uncommitted).

### Skip checks

```bash
auto-push --no-pre-push
```

## How it works

1. `git pull` to sync with remote (with auto-stash if needed)
2. Run pre-push checks if `.pre-push.json` exists
3. Detect staged, unstaged, and untracked changes
4. `git add -A` to stage everything
5. Get the diff and send it to Claude CLI for commit message generation
6. `git commit` with the generated message
7. Push via `git push`

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
