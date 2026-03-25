# Smart Init: AI-Powered Pipeline Generation

**Date:** 2026-03-25
**Status:** Draft
**Author:** Claude + gwddeveloper

## Summary

Replace the hardcoded heuristic-based `auto_init` with an AI-powered pipeline generator that scans the project, sends a structured fingerprint to the configured AI CLI (claude/codex/ollama), and generates a `.auto-push.json` pipeline tailored to the actual project — with an interactive guided walkthrough for user confirmation.

## Decisions

| Question | Decision |
|----------|----------|
| Who generates the pipeline? | AI CLI (not hardcoded Rust heuristics) |
| Scan depth | Medium: file tree + key config file contents |
| User interaction | Interactive guided: AI generates draft, Rust walks through each step |
| No AI fallback | Heuristic init (current behavior) + upgrade hint to `--smart-init` |
| Ecosystem detection | Ecosystem-agnostic fingerprinting — AI infers language/tools |
| Architecture | Two-phase: AI analysis + optional AI refinement if user modifies steps |

## Architecture

### Flow

```
auto-push --smart-init
  │
  ├─ Phase 1: Fingerprint
  │   └─ scan.rs collects: file tree, git remotes, config files, CI files, workspaces
  │
  ├─ Phase 2: AI Analysis (call #1)
  │   └─ Send fingerprint to AI CLI, receive structured JSON: steps + explanations + confidence
  │
  ├─ Phase 3: Interactive Walkthrough (Rust-driven, no AI calls)
  │   └─ Present each step with [Y/n/e], collect user modifications as delta
  │
  ├─ Phase 4: AI Refinement (call #2, only if user made changes)
  │   └─ Send draft + user deltas to AI, receive final pipeline
  │
  └─ Phase 5: Write .auto-push.json + update .gitignore
```

### Entry Points

```
main.rs Phase 2 (config load):
  ├─ Config exists → load as today
  ├─ No config + --smart-init → smart_init() [new]
  ├─ No config + AI available → heuristic init + print upgrade hint
  └─ No config + no AI → heuristic init (silent)
```

If config already exists and `--smart-init` is passed: prompt "`.auto-push.json` already exists. Overwrite? [y/N]".

## Module: `src/scan.rs` — Project Fingerprint Scanner

### Types

```rust
pub struct ProjectFingerprint {
    pub workspaces: Vec<Workspace>,
    pub git_remotes: Vec<GitRemote>,
    pub ci_files: Vec<ConfigFile>,
    pub build_files: Vec<ConfigFile>,
    pub has_monorepo_markers: bool,
}

pub struct Workspace {
    pub path: String,                // relative: "." or "src-tauri" or "packages/api"
    pub config_files: Vec<ConfigFile>,
    pub label: Option<String>,       // auto-label: "frontend", "tauri-backend", etc.
}

pub struct GitRemote {
    pub name: String,
    pub url: String,
}

pub struct ConfigFile {
    pub path: String,
    pub content: String,  // truncated to 2KB
}
```

### What It Collects

**File tree:** Recursive `fs::read_dir` to 3 levels max. Skips `.git`, `node_modules`, `target`, `vendor`, `dist`, `build`, `__pycache__`, `.next`.

**Git remotes:** `git remote -v` parsed into name + URL pairs.

**Config/manifest files (read contents):**
- `package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, `setup.py`, `setup.cfg`
- `Gemfile`, `composer.json`, `pom.xml`, `build.gradle`, `build.gradle.kts`, `Package.swift`
- `tsconfig.json`, `.eslintrc*`, `biome.json`, `deno.json`, `.prettierrc*`
- `Makefile`, `Justfile`, `Taskfile.yml`

**CI files:**
- `.github/workflows/*.yml`
- `.gitlab-ci.yml`
- `.circleci/config.yml`
- `Jenkinsfile`

**Build files:**
- `Dockerfile`, `docker-compose.yml`, `docker-compose.yaml`

### Workspace Detection

Any directory (up to 3 levels deep) containing a manifest file (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, etc.) that is not the root is flagged as a sub-workspace.

Known patterns get auto-labels:

| Pattern | Label |
|---------|-------|
| `src-tauri/Cargo.toml` | `tauri-backend` |
| `android/build.gradle` | `android` |
| `ios/Podfile` | `ios` |
| `packages/*/package.json` | `monorepo-package` |
| `apps/*/package.json` | `monorepo-app` |

Cap: maximum 20 sub-workspaces. Warn if truncated.

### Token Budget

- Each config file truncated to 2KB
- File tree listing capped at 8KB (truncate deepest levels first)
- Total fingerprint capped at ~30KB
- Truncation priority (least important first): build files, CI files, configs, file tree

### Output

`to_prompt_context(&self) -> String` formats the fingerprint as a structured text block for the AI prompt.

## AI Prompt Design

### Call #1: Analysis

System prompt instructs the AI to return JSON:

```
You are a build pipeline expert. Analyze this project and generate an auto-push pipeline config.

Return ONLY valid JSON with this structure:
{
  "analysis": "Brief project description",
  "steps": [
    {
      "name": "step-name",
      "run": "shell command",
      "description": "Why this step exists",
      "confidence": "high|medium|low",
      "category": "git|test|lint|format|build|deploy|custom",
      "alternatives": ["other command that could work"]
    }
  ],
  "detected": {
    "language": "rust",
    "package_manager": "cargo",
    "remote_name": "origin",
    "remote_url": "...",
    "ci_platform": "github-actions"
  }
}

Rules:
- Always include core steps: stash, pull, unstash, stage, generate, commit, push
- Add test/lint/format steps BETWEEN unstash and stage
- Use the actual remote name from the project (not always "origin")
- Use the actual package manager (bun/pnpm/yarn, not always npm)
- If CI files show specific commands, prefer those over generic ones
- "confidence": "low" means you're guessing — the user should verify
- For multi-workspace projects, generate steps for EACH workspace
- Use --manifest-path or explicit paths so commands work from the repo root
```

User message contains the fingerprint context.

### Intermediate Type: `AiStep`

The AI response is deserialized into a separate intermediate type, **not** into `PipelineCommand` directly. Extra fields (`confidence`, `category`, `alternatives`) are walkthrough-only and do not survive into the final `.auto-push.json`.

```rust
/// Deserialized from AI response. NOT persisted to config.
#[derive(Deserialize)]
pub struct AiResponse {
    pub analysis: String,
    pub steps: Vec<AiStep>,
    pub detected: AiDetected,
}

#[derive(Deserialize)]
pub struct AiStep {
    pub name: String,
    pub run: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub description: Option<String>,
    pub confidence: Option<String>,   // walkthrough display only
    pub category: Option<String>,     // walkthrough display only
    pub alternatives: Option<Vec<String>>, // walkthrough display only
}

#[derive(Deserialize)]
pub struct AiDetected {
    pub language: Option<String>,
    pub package_manager: Option<String>,
    pub remote_name: Option<String>,
    pub remote_url: Option<String>,
    pub ci_platform: Option<String>,
}
```

After the walkthrough, accepted `AiStep` entries are converted to `PipelineCommand` (dropping `confidence`, `category`, `alternatives`).

### Core Step Identification

Core steps are identified by a **hardcoded name list** in Rust:

```rust
const CORE_STEP_NAMES: &[&str] = &[
    "stash", "pull", "unstash", "stage", "generate", "commit", "push"
];
```

The AI prompt instructs the AI to use these exact names for core steps. During the walkthrough, any step whose `name` matches this list is tagged `[core]` and cannot be removed (only edited). If the AI uses a different name for a core concept, it is treated as non-core — the user can remove it.

### Workspace Grouping (Walkthrough Only)

The interactive walkthrough groups steps by workspace for readability, using the `AiStep.category` or name prefix (e.g., `frontend-lint`, `tauri-test`). This grouping is **display-only** and is not persisted to `.auto-push.json`. The final config is a flat pipeline array.

### Call #2: Refinement (only if user made modifications)

```
Here is the draft pipeline you generated: <draft JSON>

The user made these modifications:
- Removed step "lint" (reason: "we don't use eslint")
- Changed step "tests" run from "npm test" to "bun test"
- Accepted all other steps

Generate the final pipeline incorporating these changes.
Return ONLY the pipeline array (valid JSON, same schema as before).
```

### Provider Dispatch

Smart init invokes the AI CLI **directly via `std::process::Command`** (not through `pipeline.rs`). It reads the provider config from `GenerateConfig` to determine the command and args, applies the same timeout, and handles the structured output guard. This is a new function in `config.rs` (or a new `src/smart_init.rs` module) — it does not go through `run_pipeline()` since we need to capture and parse structured JSON, not stream output.

The function signature:

```rust
fn call_ai_for_init(
    provider: &ProviderConfig,
    prompt: &str,
    system_prompt: &str,
    timeout_secs: u64,
) -> Result<String>
```

This builds the appropriate CLI args per provider (claude/codex/ollama/custom), executes the command, and returns the raw stdout.

### JSON Parsing

1. Try `serde_json::from_str` on AI output
2. If fails, strip markdown code fences (` ```json ... ``` `) and retry
3. If still fails, retry AI call once with "return ONLY valid JSON, no markdown" nudge
4. If that fails, fall back to heuristic init with warning showing raw AI output

## Interactive Walkthrough UX

### Terminal Output

```
[init] Scanning project...
[init] Found: Cargo.toml, .github/workflows/ci.yml, Makefile, 2 git remotes
[init] Calling claude to analyze project...
[init] AI detected: Rust project, remote "upstream", cargo test/clippy/fmt

  Pipeline steps:

  1. [core] stash — Stash uncommitted changes
     > git stash push -m 'auto-push auto-stash' || true
     [Y/e] _

  2. [core] pull — Pull latest from upstream
     > git pull upstream main
     [Y/e] _

  ...

  4. tests — Run the project test suite (confidence: high)
     > cargo test
     [Y/n/e] _

  5. lint — Check for common mistakes (confidence: high)
     > cargo clippy -- -D warnings
     [Y/n/e] _

  ...

[init] 9/9 steps confirmed. Generating final config...
[init] Created .auto-push.json
```

### Multi-Workspace Grouping

Steps are grouped by workspace in the walkthrough:

```
  Frontend (root):
    4. frontend-lint — Lint TypeScript
       > npm run lint
       [Y/n/e] _

  Tauri Backend (src-tauri/):
    6. tauri-test — Run Rust tests
       > cargo test --manifest-path src-tauri/Cargo.toml
       [Y/n/e] _
```

### User Actions Per Step

- **Y (default, press Enter)** — accept as-is
- **n** — remove step, optional reason: `Why skip? (optional): _`
- **e** — edit command inline: shows current value, user types replacement

### Core Steps

Steps with `[core]` tag (stash, pull, unstash, stage, generate, commit, push) can be edited but not removed — the pipeline requires them to function.

### Modification Collection

All changes collected as a `Vec<Modification>`:

```rust
enum Modification {
    Removed { name: String, reason: Option<String> },
    Edited { name: String, new_run: String },
}
```

If `modifications.is_empty()` — write directly, skip AI call #2.
If modifications exist — trigger AI call #2 for refinement.

### Non-TTY / CI

Auto-accept all steps. Print: `[init] No TTY -- accepting all AI recommendations`.

## CLI Integration

### New Flag

```rust
#[arg(long)]
smart_init: bool,
```

Invoked: `auto-push --smart-init`

### Upgrade Hint

After heuristic init (when AI is available but `--smart-init` not passed):

```
[config] Basic pipeline generated from project heuristics.
[config] For a project-tailored pipeline, run: auto-push --smart-init
```

### Behavior

`--smart-init` writes `.auto-push.json` and exits. It does not run the pipeline.

## Error Handling

### AI Failures

| Failure | Behavior |
|---------|----------|
| AI CLI not found | Fall back to heuristic init, print upgrade hint |
| AI returns invalid JSON | Retry once with JSON-only nudge |
| Retry also fails | Fall back to heuristic init, warn with raw output |
| AI timeout (default 60s) | Fall back to heuristic init |
| AI returns empty/no steps | Fall back to heuristic init |

### Fingerprint Edge Cases

| Case | Behavior |
|------|----------|
| 100+ packages monorepo | Cap at 20 sub-workspaces, warn |
| Config file > 2KB | Truncate with `... (truncated)` marker |
| Total fingerprint > 30KB | Truncate least-important files first |
| No config files at all | AI still gets file tree + remotes |
| Symlinks | Follow 1 level, skip circular |

### Interactive Edge Cases

| Case | Behavior |
|------|----------|
| Ctrl+C mid-walkthrough | No config written, clean exit |
| Config exists + `--smart-init` | Prompt: "Overwrite? [y/N]" |
| `--smart-init` in CI (no TTY) | Auto-accept all, write, print summary |

### Fallback Guarantee

Every code path produces a valid `.auto-push.json`. Smart init is best-effort; heuristic init is the safety net.

## Testing Strategy

### Unit Tests: `src/scan.rs`

- `test_scan_rust_project` — tempdir with `Cargo.toml`, verify detection
- `test_scan_tauri_project` — root `package.json` + `src-tauri/Cargo.toml`, verify 2 workspaces
- `test_scan_monorepo` — `packages/a/package.json` + `packages/b/package.json`
- `test_scan_empty_project` — no config files, graceful empty fingerprint
- `test_scan_truncation` — oversized config, verify 2KB cap
- `test_scan_max_workspaces` — 25+ sub-dirs, verify cap at 20
- `test_fingerprint_to_prompt` — verify formatted output

### Unit Tests: `src/config.rs` (smart init)

- `test_parse_ai_response_valid` — valid JSON parses correctly
- `test_parse_ai_response_invalid` — garbage returns error
- `test_parse_ai_response_markdown_fence` — strip ` ```json ``` ` and parse
- `test_build_refinement_prompt` — user deltas assembled correctly
- `test_smart_init_falls_back_on_no_provider` — no AI → heuristic init
- `test_smart_init_overwrites_guard` — existing config → overwrite flag

### Integration Tests: `tests/smart_init.rs`

- `test_smart_init_end_to_end` — mock AI CLI (shell script returning JSON), verify config output
- `test_smart_init_fallback_on_ai_failure` — mock AI exits 1, verify heuristic fallback
- `test_smart_init_retry_on_bad_json` — mock returns garbage then valid JSON

### Mock AI CLI Approach

Tests create temp shell scripts as fake AI providers via `ProviderConfig::Custom`. Scripts return predetermined JSON. No real AI calls in tests.

## Files Changed

| File | Change |
|------|--------|
| `src/scan.rs` | **New** — project fingerprint scanner, `ProjectFingerprint`, workspace detection |
| `src/smart_init.rs` | **New** — `smart_init()` orchestrator, `AiStep`/`AiResponse` types, `call_ai_for_init()`, interactive walkthrough, refinement prompt builder |
| `src/config.rs` | Add heuristic init upgrade hint, overwrite guard for `--smart-init` |
| `src/main.rs` | Add `--smart-init` flag, new init routing logic |
| `tests/smart_init.rs` | **New** — integration tests with mock AI CLI |

## Out of Scope

- `auto-push init` as a subcommand (may add later, `--smart-init` flag is simpler)
- Caching fingerprints across runs
- Automatic pipeline updates when project structure changes
- GUI/TUI for the interactive walkthrough (plain stdin/stdout is sufficient)
