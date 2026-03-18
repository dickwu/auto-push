# Resilient Auto-Push: Comprehensive Edge Case Handling

**Date:** 2026-03-18
**Status:** Approved
**Scope:** Full submodule support, pre-flight safety checks, stash management, rebase support, LFS detection, shallow clone handling, enhanced push recovery

## Summary

Enhance auto-push to autonomously handle every common git scenario a power user would encounter. The tool should require zero prompts — it handles everything silently and only stops on truly unrecoverable errors.

Key additions:
- Full git submodule support (recursive pull, commit, push with correct dependency ordering)
- Pre-flight environment validation (detached HEAD, no remote, shallow clone, LFS, leftover conflicts)
- Auto-stash/unstash around pulls
- Rebase-based pull via `--rebase` flag
- Smarter push recovery (upstream auto-setup, branch protection detection, network retry)

## Architecture: Phase Pipeline

Refactor the current procedural `main()` into a sequence of named phases. Each phase is self-contained with its own error recovery.

```
Preflight -> Stash -> Pull -> SubmoduleSync -> Unstash -> Stage -> Commit -> Push(subs->parent)
```

**Note on ordering:** Pull runs before SubmoduleSync so the parent has the latest `.gitmodules` and submodule pointers before we process submodules. This matches `git pull --recurse-submodules` behavior. Unstash runs immediately before Stage so stashed changes are included in the commit (not silently preserved as working-tree leftovers).

### Shared Context

Preflight returns a `PreflightResult` with environment info. This is combined with CLI flags in `main()` to build the `Context`, keeping preflight testable without constructing CLI args:

```rust
pub struct PreflightResult {
    pub repo_root: PathBuf,
    pub branch: String,
    pub remote: String,
    pub is_shallow: bool,
    pub has_submodules: bool,
    pub submodule_paths: Vec<String>,
    pub has_lfs: bool,
    pub has_upstream: bool,
}

pub struct Context {
    pub preflight: PreflightResult,
    pub cli: CliFlags,
}
```

### Module Structure

| File | Purpose |
|---|---|
| `src/main.rs` | CLI parsing + ~50-line phase orchestrator |
| `src/context.rs` | `Context` struct + `CliFlags` |
| `src/preflight.rs` | Environment detection (read-only, never modifies repo) |
| `src/stash.rs` | Auto-stash / unstash |
| `src/submodule.rs` | Submodule operations (sync, dirty detection, recursive pipeline) |
| `src/pull.rs` | Pull with merge/rebase + conflict resolution |
| `src/push.rs` | Submodule-first push + error recovery |
| `src/stage_commit.rs` | Stage + Claude commit planning (extracted from current main.rs) |
| `src/git.rs` | Low-level git command utilities (existing, extended) |
| `src/claude.rs` | Claude CLI interaction (existing, unchanged) |
| `src/diff.rs` | Diff parsing (existing, unchanged) |

## Phase Details

### 1. Preflight

`preflight::check()` gathers environment info into `Context` and fails early on unrecoverable states. It never modifies the repo.

| Check | How | Action |
|---|---|---|
| Not a git repo | `git rev-parse --git-dir` | Bail |
| Detached HEAD | `git symbolic-ref -q HEAD` fails | Bail: "detached HEAD -- checkout a branch first" |
| No remote configured | `git remote` is empty | Bail: "no remote -- add with `git remote add origin <url>`" |
| No upstream tracking | `git rev-parse --abbrev-ref @{u}` fails | Set `ctx.has_upstream = false` (deferred to push phase) |
| Shallow clone | `git rev-parse --is-shallow-repository` | Print warning, continue |
| Has submodules | `.gitmodules` exists | Set `ctx.has_submodules = true`, populate `ctx.submodule_paths` |
| Has LFS | `.gitattributes` contains `filter=lfs` | Ensure `git lfs install` has been run; if not, run it |
| Unmerged paths | `git diff --name-only --diff-filter=U` | Bail: "unresolved conflicts from a previous merge" |

### 2. Stash

`stash::auto_stash()` protects the working tree before pull.

- Detects unstaged/untracked changes
- Runs `git stash push --include-untracked -m "auto-push: pre-pull stash"`
- Returns `StashResult::Stashed { ref_name }` or `StashResult::NotNeeded`

`stash::auto_unstash()` runs **immediately before the Stage phase** (not at the end of the pipeline). This means stashed changes are restored to the working tree and then staged + committed along with everything else. The user's uncommitted work becomes part of the commit.

- `git stash pop`
- On conflict: leaves stash intact, warns user ("Your changes are in `git stash list`. Run `git stash pop` manually.")

**Contract:** Stash exists solely to protect the working tree during pull. Once pull completes, the stash is restored so changes are committed. Users who want to preserve uncommitted changes without committing them should use `--no-stash` (which skips the stash and lets the pull fail if dirty).

Stash is tagged with `auto-push:` prefix for identification. Only occurs when there are uncommitted changes AND a pull is needed. Scoped to parent repo only -- submodules handle their own stash internally.

### 3. Submodule Sync

`submodule::sync()` runs **after pull** (so the parent has the latest `.gitmodules` and submodule pointers).

**Step 1: Init & Update**
```
git submodule update --init --recursive
```

**Step 2: Process dirty submodules**

For each submodule with local changes, run the commit pipeline in-process (not a recursive binary invocation):
- Stash if needed
- Pull
- Stage all changes
- Generate commit message via Claude
- Commit
- (Push is deferred to Phase 6 for centralized ordering)

**Step 3: Stage pointer updates**

After all submodules are committed and pushed, `git add <submodule_path>` in the parent repo stages any updated pointers.

**Edge cases:**
- Nested submodules: handled by `--recursive` flag
- Submodule on detached HEAD: check out the tracking branch configured in `.gitmodules` (`branch = ...`). If no branch configured, fall back to the remote's default branch via `git remote show origin`. If that also fails, skip commit for that submodule with a warning.
- Submodule with no remote: skip push, warn user
- New untracked files inside submodule: staged and committed like the parent

**Push ordering:** Submodules are committed in Phase 3 but NOT pushed yet. All submodule pushes happen in Phase 6 (Push), before the parent push. This keeps push logic centralized in one phase and ensures correct dependency ordering (submodule commits exist on remote before parent references them).

### 4. Pull

`pull::run()` replaces the current inline pull logic.

**Decision tree:**
1. If `ctx.has_upstream == false`: skip pull, return `PullResult::Skipped`
2. If `--rebase` flag: run `git pull --rebase`
3. Otherwise: run `git pull` (merge, current behavior)

**Results:**
| Result | Action |
|---|---|
| AlreadyUpToDate | Continue |
| FastForward | Continue |
| Merged | Set `needs_merge = true` for commit message style |
| Conflict (merge) | Claude resolves (interactive, or auto with `--force`), `git add`, continue |
| Conflict (rebase) | Rebase conflict loop (see below) |
| RebaseConflict unresolvable | `git rebase --abort`, bail |
| Error (no network) | Warn, continue with local changes |

**Rebase conflict loop:** A rebase replays N commits, and conflicts can occur at each step. The resolution loop:
1. Detect conflicted files
2. Invoke Claude to resolve them (same `resolve_conflicts` function, respects `--force`)
3. `git add` resolved files
4. `git rebase --continue`
5. If new conflicts appear, repeat from step 1
6. Budget: max 10 iterations. If exceeded, `git rebase --abort` and bail with "rebase too conflicted -- resolve manually or use merge-based pull"

### 5. Stage & Commit

`stage_commit::run()` — extracted from current `main.rs`.

**Staging behavior:**
- Submodule pointer paths are already staged by Phase 3 (SubmoduleSync). Phase 5 does not re-stage them.
- If `--stage-all` (default true): `git add -A` stages all remaining changes (tracked + untracked)
- If `--stage-all` is false: only already-staged files are committed (user must have staged manually before running auto-push)

**Commit behavior (unchanged):**
- If `--message` provided: single commit with that message
- Otherwise: Claude analyzes diff, plans hunk-level commits
- Execute commit groups with hunk-level staging via `git apply --cached`

### 6. Push

`push::run()` handles submodule-first ordering and smarter recovery.

**Flow:**
1. If `--no-push`: skip
2. Push each submodule with new commits (in dependency order)
3. Push parent repo
   - If `ctx.has_upstream == false`: `git push -u <remote> <branch>`
   - Otherwise: `git push <remote> <branch>`

**Error handling:**
| Error | Action |
|---|---|
| Non-fast-forward rejected | Claude diagnoses, suggests fix commands (git-only allowlist) |
| No permission | UNRECOVERABLE, bail |
| Network error | Retry once after 2s (`std::thread::sleep`), then bail with "committed locally, push manually later" |
| Branch protection | Suggest PR workflow: "Branch is protected. Create a PR instead" |
| Submodule push partial failure | Log which submodules succeeded/failed, skip parent push, tell user to re-run |
| Unknown | Claude diagnoses via `fix_push_error()` (git-only allowlist) |

**Command execution security:** The `run_commands()` function that executes Claude's push-fix suggestions is hardened with a strict allowlist. Only lines starting with `git ` are executed. Any non-git command is rejected and printed for the user to run manually. This prevents arbitrary command injection from LLM output.

**Runtime:** The tool remains fully synchronous. The 2-second network retry delay uses `std::thread::sleep`. No async runtime is needed.

### 7. Unstash

**Note:** Unstash runs immediately before Stage (Phase 5), not at the end. See Phase 2 for the contract. The "Phase 7" numbering is kept for clarity but the actual execution order is:

```
Preflight -> Stash -> Pull -> SubmoduleSync -> Unstash -> Stage -> Commit -> Push
```

`stash::auto_unstash()` restores stashed changes to the working tree so they are included in the commit. On conflict, warns user and leaves stash intact.

## CLI Interface

### New flags

| Flag | Short | Purpose |
|---|---|---|
| `--rebase` | `-r` | Pull with rebase instead of merge |
| `--no-submodules` | | Skip submodule handling |
| `--no-stash` | | Don't auto-stash; if working tree is dirty and pull is needed, bail with "uncommitted changes -- commit or stash manually before running auto-push" |
| `--no-pull` | | Skip pull phase entirely |

### Full flag set (v0.2.0)

```
auto-push [FLAGS]
  -a, --stage-all        Stage all changes (default: true)
  -c, --confirm          Review and confirm before each action
  -n, --dry-run          Preview without making changes
  -m, --message <MSG>    Custom commit message (skip Claude)
  -f, --force            Auto-resolve conflicts without prompts
  -r, --rebase           Pull with rebase instead of merge
      --no-push          Skip pushing to remote
      --no-pull          Skip pulling from remote
      --no-submodules    Skip submodule handling
      --no-stash         Bail if dirty working tree (don't auto-stash)
```

## Output

Each phase prints a one-line status. Autonomous and quiet:

```
auto-push v0.2.0
[preflight] main -> origin (2 submodules, LFS detected)
[stash] Saved 3 uncommitted changes
[pull] Fast-forwarded to latest
[submodule] lib/core: committed + pushed (feat: add parser)
[submodule] lib/utils: clean, skipped
[stash] Restored 3 stashed changes for commit
[stage] Staged 5 files
[commit] 2 commit(s) planned by Claude
  [1/2] feat: add user validation (src/validate.rs, src/main.rs)
  [2/2] test: add validation tests (tests/validate.rs)
[push] Pushed to origin/main
```

## Testing Strategy

### Unit tests (no git required)
- Diff parsing: extend for submodule diffs
- Command output parsing: PullResult classification, conflict detection, submodule status parsing
- Stash result handling: stash/unstash state machine
- Context construction: preflight detection from mock git output

### Integration tests (temporary git repos)
- Create temp repos with `git init`, test full phase pipeline
- Submodule scenarios: parent + child repos, dirty submodule detection, push ordering
- Nested submodule scenarios: submodule inside submodule, recursive init/update/commit/push
- Stash scenarios: dirty tree -> stash -> pull -> unstash
- Detached HEAD, shallow clone, no-remote detection
- New branch / no upstream push

### Claude interaction tests
- Mock `claude` CLI binary with shell script returning canned responses
- Test commit message generation, conflict resolution, push error diagnosis

## Version

This enhancement targets **v0.2.0** as it introduces new CLI flags and restructures the internal architecture.
