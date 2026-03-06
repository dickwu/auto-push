# auto-push

CLI tool that automates the git workflow: pull, stage, generate commit messages with Claude, and push — in one command.

## Prerequisites

- [git](https://git-scm.com)
- [claude](https://claude.ai/code) (Claude Code CLI, must be authenticated)

## Install

### macOS (Homebrew)

```bash
brew tap dickwu/auto-push https://github.com/dickwu/auto-push
brew install auto-push
```

### Upgrade

```bash
brew update && brew upgrade auto-push
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
```

## How it works

1. `git pull` to sync with remote
2. Detect staged, unstaged, and untracked changes
3. `git add -A` to stage everything
4. Get the diff and send it to Claude CLI for commit message generation
5. `git commit` with the generated message
6. `git push` — if it fails, Claude diagnoses the error and runs the fix automatically

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
