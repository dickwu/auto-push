# Pipeline Engine: Unified Command Runner

**Date**: 2026-03-23
**Status**: Draft
**Scope**: Replace hardcoded generate/push/pull/preflight phases with a single configurable `pipeline` array in `.auto-push.json`. Unify all execution through the existing hooks command runner engine with enhanced template variable support.

---

## 1. Problem

Today auto-push has two separate execution engines:

1. **Hooks engine** (`hooks.rs`): `execute_command()` + `run_phase()` — runs shell commands with template vars, confirm prompts, on_error handlers, output chaining.
2. **Provider engine** (`generate.rs`): `call_provider()` — spawns CLI processes with template args, timeout, structured output parsing.

The main flow in `main.rs` hardcodes 10 phases (preflight → stash → pull → submodule sync → unstash → pre_push hooks → stage+commit → push → after_push hooks). Users can only configure `pre_push` and `after_push` hook arrays — everything else is locked in Rust.

This creates duplication (two command runners), limits flexibility (can't reorder or replace core steps), and makes it hard to support new providers or workflows without code changes.

Additionally, the existing codex preset uses invalid CLI flags (`--quiet`, `--prompt`) that don't work with codex 0.116.0+.

## 2. Solution

Replace `pre_push`/`after_push` with a single ordered `pipeline` array. Every step — pull, test, lint, generate, commit, push, notify — is a shell command that runs through the same engine. The `generate` config section becomes metadata-only (commit style, prompt content) that feeds template variables.

### Design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Pipeline structure | Single `pipeline` array (flat) | Maximum flexibility — users control ordering |
| Generate execution | Pure shell command | No special Rust logic — composable via template vars |
| Hunk splitting | Convention-based (`---` separator) | Simple, works with any provider |
| Variable resolution | Strict registry with validation | Catch errors at config load time |
| Structured access | JSON dot-path + regex extraction | Rich data piping between commands |
| Backward compat | Runtime migration of old config | Old `pre_push`/`after_push` still work |

## 3. Config Schema

### New `AppConfig` struct

The struct accepts both new (`pipeline`, `vars`) and legacy (`pre_push`, `after_push`) fields. `deny_unknown_fields` is **removed** from `AppConfig` to allow forward/backward compat during migration. Individual nested types (`CommitStyle`, `CustomPrompts`) retain `deny_unknown_fields`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub generate: GenerateConfig,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    #[serde(default)]
    pub pipeline: Option<Vec<PipelineCommand>>,  // new — None means legacy mode
    #[serde(default)]
    pub pre_push: Vec<PipelineCommand>,           // legacy — kept for backward compat
    #[serde(default)]
    pub after_push: Vec<PipelineCommand>,          // legacy — kept for backward compat
    #[serde(default)]
    pub branches: serde_json::Map<String, serde_json::Value>,
}
```

If `pipeline` is `Some(...)`, it drives execution. If `pipeline` is `None` and `pre_push`/`after_push` exist, legacy migration kicks in. If all three are absent, auto-init generates defaults.

### New `GenerateConfig` struct

The `provider`, `description`, `structured_output`, and `timeout_secs` fields are kept as optional for legacy config parsing but ignored during pipeline execution. They are only used during legacy migration to construct the generate command's `run` string.

`deny_unknown_fields` is also removed from `GenerateConfig` since legacy fields (`provider`, `description`, `structured_output`, `timeout_secs`) must parse alongside new metadata fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateConfig {
    #[serde(default)]
    pub provider: Option<ProviderConfig>,   // legacy — used only for migration
    #[serde(default)]
    pub commit_style: CommitStyle,
    #[serde(default)]
    pub prompts: CustomPrompts,
    #[serde(default = "default_max_diff_bytes")]
    pub max_diff_bytes: usize,
    #[serde(default)]
    pub description: Option<String>,        // legacy — ignored in pipeline mode
    #[serde(default)]
    pub structured_output: Option<bool>,    // legacy — ignored in pipeline mode
    #[serde(default)]
    pub timeout_secs: u64,                  // legacy — ignored in pipeline mode
}
```

`CustomPrompts` retains all fields including `push_fix` and `conflict_resolve` for backward compatibility:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomPrompts {
    pub simple: Option<String>,
    pub detailed: Option<String>,
    pub plan: Option<String>,
    pub push_fix: Option<String>,          // exposed as {{ push_fix_prompt }} template var
    pub conflict_resolve: Option<String>,  // exposed as {{ conflict_resolve_prompt }} template var
}
```

### New `.auto-push.json` structure

```jsonc
{
  // User-defined variables — flat key-value, available to all pipeline commands
  "vars": {
    "slack_channel": "#deploys",
    "team": "backend"
  },

  // Metadata: feeds template variables, does NOT drive execution
  "generate": {
    "commit_style": {
      "format": "conventional",
      "types": ["feat", "fix", "refactor", "docs", "test", "chore", "perf", "ci"],
      "max_length": 72,
      "include_body": true
    },
    "prompts": {
      "simple": null,
      "detailed": null,
      "plan": null,
      "push_fix": null,
      "conflict_resolve": null
    },
    "max_diff_bytes": 20000
  },

  // Execution: ordered list of commands, all run through the same engine
  "pipeline": [
    { "name": "pull",     "run": "git pull", "description": "Pull latest changes" },
    { "name": "tests",    "run": "cargo test", "description": "Run test suite" },
    { "name": "lint",     "run": "cargo clippy -- -D warnings" },
    { "name": "stage",    "run": "git add -A" },
    {
      "name": "generate",
      "run": "claude -p 'Generate a commit message for this diff:\n\n{{ diff }}' --system-prompt '{{ system_prompt }}' --output-format text --no-session-persistence --tools ''",
      "capture": "commit_message",
      "description": "Generate commit message with AI"
    },
    {
      "name": "commit",
      "run": "git commit -m '{{ commit_message }}'",
      "capture_after": {
        "commit_hash": "git rev-parse --short HEAD",
        "commit_summary": "git log -1 --format=%s"
      }
    },
    { "name": "push", "run": "git push {{ remote }} {{ branch }}" },
    { "name": "notify", "run": "echo 'Pushed {{ commit_hash }}: {{ commit_summary }}'" }
  ],

  // Branch overrides (unchanged behavior)
  "branches": {
    "main": {
      "pipeline": []
    }
  }
}
```

### PipelineCommand type

```rust
struct PipelineCommand {
    name: String,
    run: String,
    description: Option<String>,
    on_error: Option<String>,
    confirm: Option<String>,
    interactive: bool,
    capture: Option<String>,                        // store stdout into named var
    capture_after: Option<HashMap<String, String>>,  // run extra commands post-success, store results
}
```

`PipelineCommand` extends the existing `HookCommand` with `capture` and `capture_after`. The old `HookCommand` type is replaced by `PipelineCommand` everywhere.

## 4. Variable Registry

### Three sources of variables

**Built-in vars** — auto-push always computes these from git state and config metadata. Cannot be overridden by user vars or captures.

| Variable | Source |
|---|---|
| `branch` | `git rev-parse --abbrev-ref HEAD` |
| `remote` | detected default remote |
| `remote_url` | `git remote get-url <remote>` |
| `repo_root` | `git rev-parse --show-toplevel` |
| `diff` | `git diff --cached`, truncated to `max_diff_bytes` |
| `diff_stat` | `git diff --cached --stat` |
| `hunks` | formatted numbered hunks for split planning |
| `staged_files` | newline-separated list of staged file paths |
| `staged_count` | number of staged files |
| `style_suffix` | auto-derived from `generate.commit_style` |
| `system_prompt` | resolved from `generate.prompts.simple` (or built-in default) + `style_suffix` |
| `system_prompt_detailed` | same for merge commits |
| `system_prompt_plan` | same for hunk planning |
| `push_fix_prompt` | from `generate.prompts.push_fix` (or built-in default) |
| `conflict_resolve_prompt` | from `generate.prompts.conflict_resolve` (or built-in default) |
| `max_diff_bytes` | from `generate.max_diff_bytes` |

**User vars** — declared in the `vars` config section. Names must not collide with built-ins.

**Captured vars** — declared via `capture` / `capture_after` on pipeline commands. Names must not collide with built-ins, user vars, or other captures.

### Validation rules (checked at config load time)

```
Rule 1: No duplicate names across all three sources
  built-in ∩ vars      → error
  built-in ∩ capture   → error
  vars ∩ capture       → error
  capture ∩ capture    → error

Rule 2: Every {{ var }} reference must be resolvable at that pipeline index
  For command at index N, scan run/on_error/confirm for {{ var }} references.
  Each var_name (the root before any dot-path or regex) must be:
    (a) a built-in, OR
    (b) a user var, OR
    (c) captured by a command at index < N
  Otherwise → error with actionable message.

Rule 3: Regex patterns in {{ var:/pattern/ }} are validated syntactically.
```

### Error messages

These examples show hypothetical misconfigured pipelines (not the example from Section 3):

```
Error: Variable "commit_message" in command "commit" (index 3) is not available.
       It is captured by command "generate" (index 5) which runs later.
       → Move "generate" before "commit" in your pipeline.

Error: Duplicate capture name "commit_message".
       Captured by both "generate" (index 4) and "backup_gen" (index 5).
       → Each variable name must be unique across vars and captures.

Error: Variable "branch" in vars conflicts with built-in variable.
       → Remove "branch" from vars or use a different name like "target_branch".
```

## 5. Structured Variable Access

Three access modes on any variable:

### Raw access
```
{{ commit_message }}    → the full captured string
```

### JSON dot-path access
When a captured value is valid JSON, fields are accessible via dot-notation:

```
{{ plan.0.message }}        → first array element's "message" field
{{ plan.0.hunks }}          → first element's "hunks" array as string
{{ plan.length }}           → number of elements in JSON array
{{ result.status }}         → field on a JSON object
{{ plan.0.hunks.0 }}        → first hunk ID from first element
```

If the value is not valid JSON, dot-path access produces a runtime error with context.

### Regex extraction
Apply a regex to the variable value, return first capture group (or full match if no groups):

```
{{ version:/v(\d+\.\d+\.\d+)/ }}   → "1.2.3"
{{ output:/error: (.*)/ }}          → "file not found"
{{ sha:/([a-f0-9]{7})/ }}           → "abc1234"
```

This reuses the existing `template::extract_regex()` function (currently `#[allow(dead_code)]` in `template.rs:72`).

### Template expression grammar

```
Expression     = VarName [ DotPath | RegexExtract ]
VarName        = [a-zA-Z_][a-zA-Z0-9_]*
DotPath        = "." Segment ( "." Segment )*
Segment        = [a-zA-Z_][a-zA-Z0-9_]* | [0-9]+   (field name or array index)
RegexExtract   = ":/" Pattern "/"
Pattern        = valid Rust regex
```

### Resolution logic

```rust
fn resolve_expression(expr: &str, vars: &HashMap<String, String>) -> Result<String> {
    // 1. Regex: "var_name:/pattern/"
    if let Some((var_name, pattern)) = parse_regex_expr(expr) {
        let raw = require_var(var_name, vars)?;
        return extract_regex(raw, pattern);
    }

    // 2. Dot-path: "var_name.field.0.nested"
    if let Some((var_name, path)) = parse_dot_path(expr) {
        let raw = require_var(var_name, vars)?;
        let json: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| anyhow!("'{}' is not valid JSON for dot-path access", var_name))?;
        return resolve_json_path(&json, &path);
    }

    // 3. Simple var
    require_var(expr, vars).map(|s| s.clone())
}
```

## 6. Pipeline Execution Engine

### Execution flow

```
1. PREFLIGHT (hardcoded — always runs, not configurable)
   - Validate git repo, not detached HEAD, has remote, no unresolved conflicts
   - Populate built-in vars: branch, remote, remote_url, repo_root

2. LOAD CONFIG
   - Parse .auto-push.json
   - Migrate legacy config if needed (pre_push/after_push → pipeline)
   - Apply branch overrides
   - Validate variable registry (Rules 1-3)
   - Note: static validation uses the full pipeline. --skip is a runtime concern
     and does NOT affect static validation. If --skip removes a command that
     captures a var used by a later command, that produces a runtime error
     (not a config load error). This is intentional — the config is valid,
     the invocation is not.

3. BUILD TEMPLATE CONTEXT
   - Register built-in vars
   - Register user vars from "vars" section
   - Register --var CLI overrides (cannot override built-ins — error if attempted)
   - Compute generate metadata vars (system_prompt, style_suffix, etc.)

4. EXECUTE PIPELINE
   For each command at index i:
     a. Resolve {{ vars }} in run/confirm/on_error templates
        - First resolve structured access (dot-path, regex) to plain strings
        - Then apply shell escaping via render_shell for run/on_error
        - render_shell and render_raw both call resolve_expression() internally
          for each {{ }} match, replacing the old direct HashMap::get
     b. If --skip includes this command name → skip, print [pipeline] [i+1/total] <name> skipped
     c. If -m provided and command has capture: "commit_message" → skip
     d. Print: [pipeline] [i+1/total] <description>...
     e. If --dry-run → print resolved command, continue
     f. If --confirm flag is set → add implicit confirmation prompt
        "Run '<resolved_run>'?" unless the command already has a confirm field.
        Commands with an explicit confirm field use that text instead.
        Skip prompt if --force or no TTY.
     g. If command has explicit confirm field → prompt with that text
        (skip if --force or no TTY)
     h. Execute via sh -c with SEPARATE stdout and stderr pipes:
        - stderr is streamed to terminal in real-time (for user feedback)
        - stdout is BOTH streamed to terminal AND buffered for capture
        - This differs from the old hooks engine which combined stdout+stderr.
          The change is needed so capture only gets stdout (not stderr noise).
        - When interactive=true and TTY available: full passthrough (no capture)
     i. If capture is set → store trimmed stdout buffer as named var
     j. If capture_after is set → run each sub-command with same pipe behavior,
        store trimmed stdout as named vars
     k. On failure:
        - Run on_error handler if set (template vars available including captures from prior commands)
        - Bail with error message: "[pipeline] '<name>' failed. Command: <resolved_run>"
     l. Print: [pipeline] [i+1/total] <name> passed
```

### Multi-commit splitting

Convention: if `{{ commit_message }}` (or any var used in a `git commit -m` command) contains `---` on its own line as a separator, the engine splits into multiple commits:

```
feat: add login flow
---
fix: correct email validation
```

Detection: when a command's resolved `run` matches the pattern `^git commit .+-m\s+` and the interpolated commit message contains `\n---\n`, the engine:
1. Splits the message by `\n---\n`
2. Trims each segment
3. Filters out empty segments
4. For each segment: runs `git add -A && git commit -m '<segment>'`
   (re-stages between commits since earlier commits consume staged changes)
5. Reports: `[pipeline] [6/8] commit: 2 commit(s) created`

If no `---` separator, the command runs as-is (single commit). If the commit message happens to contain `---` in a markdown body (after a blank line), users can opt out by not using the `---` convention — the AI prompt controls the output format.

### What stays hardcoded

| Concern | Location | Rationale |
|---|---|---|
| Preflight checks | Hardcoded in Rust | Safety — must validate git state before any commands |
| Config loading | Hardcoded in Rust | Must parse config to know what pipeline to run |
| Variable registry validation | Hardcoded in Rust | Catches errors before execution |
| Multi-commit split detection | Hardcoded in Rust | Convention applied transparently |
| Template rendering | Hardcoded in Rust | Core engine capability |

Everything else — pull, stash, test, lint, stage, generate, commit, push, notify — moves to pipeline commands.

### Known behavior changes from current version

These features existed in the hardcoded pipeline and are **not replicated** by default pipeline commands. This is intentional — the pipeline model trades built-in magic for explicit configuration.

| Lost behavior | Was in | Mitigation |
|---|---|---|
| AI-assisted merge conflict resolution | `pull.rs` + `generate.rs` | Users can add an `on_error` handler on their pull command that invokes `claude` for conflict resolution. Example in Section 8. Future: may add a built-in `resolve-conflicts` helper command. |
| AI-assisted push error recovery | `push.rs` | Users can add `on_error` on the push command. Network retry example in Section 8. |
| Rebase conflict loop (up to 10 iterations) | `pull.rs` | Not replicated. Users who need this should use `git pull --rebase` and handle conflicts manually or via on_error. |
| Auto-stash/unstash around pull | `stash.rs` | Default pipeline includes explicit stash/unstash commands. |
| Submodule detection, stash, pull, commit, push | `submodule.rs` | Users with submodules add explicit commands. Simple case: `git submodule update --init --recursive`. |
| Push retry on network error (2s delay) | `push.rs` | Users can add `on_error: "sleep 2 && git push {{ remote }} {{ branch }}"` on push command. |
| Protected branch detection | `push.rs` | Push command will fail naturally; error message from git is clear. |

## 7. CLI Flags

### New flag: `--skip <name>`

Replaces all phase-specific flags. Repeatable.

```bash
auto-push --skip pull --skip tests    # skip pull and tests
auto-push --skip push                 # commit but don't push
```

### New flag: `--var <key>=<value>`

Override or add vars from CLI. Repeatable.

```bash
auto-push --var slack_channel=#urgent --var team=infra
```

CLI vars override `vars` section values. Cannot override built-in vars (error if attempted).

### Flag mapping (old → new)

| Old flag | New equivalent | Notes |
|---|---|---|
| `--no-pull` | `--skip pull` | Skip command named "pull" |
| `--no-push` | `--skip push` | Skip command named "push" |
| `--no-pre-push` | removed | No separate phases |
| `--no-after-push` | removed | No separate phases |
| `--no-hooks` | removed | No separate hooks concept |
| `--no-generate` | `--skip generate` | Skip AI generation |
| `--no-stash` | `--skip stash` | Skip stash command |
| `--no-submodules` | `--skip submodules` | If user has a submodule command |
| `-m "msg"` | `-m "msg"` | Pre-registers `commit_message` var; auto-skips any command with `capture: "commit_message"` |
| `--dry-run` | `--dry-run` | Unchanged |
| `--force` | `--force` | Unchanged |
| `--confirm` | `--confirm` | Unchanged |
| `--provider <name>` | `--provider <name>` | Convenience: replaces the `run` of any command named "generate" with the preset template for that provider. E.g., `--provider codex` swaps in the codex exec command. Only works if a command named "generate" exists in the pipeline. |
| `--rebase` | deprecated | Kept for one major version: if present, replaces the `run` of any command named "pull" with `git pull --rebase`. Deprecation warning printed. |
| `--show-config` | `--show-config` | Unchanged |

### Deprecation

Old flags (`--no-pull`, `--no-push`, `--no-generate`, `--no-stash`, `--no-submodules`, `--rebase`) are kept for one major version with deprecation warnings:

```
Warning: --no-pull is deprecated. Use --skip pull instead.
```

## 8. Auto-Init

### Provider detection for default generate command

| Detected CLI | Default generate `run` |
|---|---|
| `claude` | `claude -p 'Generate a commit message for this diff:\n\n{{ diff }}' --system-prompt '{{ system_prompt }}' --output-format text --no-session-persistence --tools ''` |
| `codex` | `codex exec --color never 'Generate a commit message for this diff:\n\n{{ diff }}\n\n{{ system_prompt }}'` (validated against codex 0.116.0+) |
| `ollama` | `ollama run llama3 '{{ system_prompt }}\n\nGenerate a commit message for this diff:\n\n{{ diff }}'` |
| none | Warning printed; generate command left as placeholder |

### Default pipeline for Rust projects

```jsonc
{
  "generate": {
    "commit_style": {
      "format": "conventional",
      "types": ["feat", "fix", "refactor", "docs", "test", "chore", "perf", "ci"],
      "max_length": 72,
      "include_body": true
    }
  },
  "pipeline": [
    { "name": "stash",    "run": "git stash push -m 'auto-push auto-stash' || true", "description": "Stash uncommitted changes" },
    { "name": "pull",     "run": "git pull",                              "description": "Pull latest changes" },
    { "name": "unstash",  "run": "git stash pop || true",                 "description": "Restore stashed changes" },
    { "name": "tests",    "run": "cargo test",                            "description": "Run test suite" },
    { "name": "lint",     "run": "cargo clippy -- -D warnings",           "description": "Run linter" },
    { "name": "fmt",      "run": "cargo fmt -- --check",                  "description": "Check formatting" },
    { "name": "stage",    "run": "git add -A",                            "description": "Stage all changes" },
    { "name": "generate", "run": "<provider-specific>", "capture": "commit_message", "description": "Generate commit message with AI" },
    { "name": "commit",   "run": "git commit -m '{{ commit_message }}'",  "description": "Create commit",
      "capture_after": { "commit_hash": "git rev-parse --short HEAD", "commit_summary": "git log -1 --format=%s" } },
    { "name": "push",     "run": "git push origin {{ branch }}",          "description": "Push to remote",
      "on_error": "sleep 2 && git push origin {{ branch }}" }
  ]
}
```

Similar templates for Node.js (`npm test`, `npm run lint`) and Go (`go test ./...`, `go vet ./...`) projects.

### Advanced pipeline example: conflict resolution + push recovery

For users who want the full behavior of the old hardcoded pipeline:

```jsonc
{
  "pipeline": [
    { "name": "stash",    "run": "git stash push -m 'auto-push' || true" },
    { "name": "pull",     "run": "git pull",
      "on_error": "claude -p 'Resolve merge conflicts in: $(git diff --name-only --diff-filter=U)' --system-prompt 'You are a merge conflict resolver. Read each conflicted file, resolve conflicts, write back, and git add.' --allowedTools 'Edit,Read,Bash' && git add -A" },
    { "name": "unstash",  "run": "git stash pop || true" },
    { "name": "tests",    "run": "cargo test" },
    { "name": "stage",    "run": "git add -A" },
    { "name": "generate", "run": "claude -p 'Generate a commit message:\n\n{{ diff }}' --system-prompt '{{ system_prompt }}' --output-format text --no-session-persistence --tools ''",
      "capture": "commit_message" },
    { "name": "commit",   "run": "git commit -m '{{ commit_message }}'",
      "capture_after": { "commit_hash": "git rev-parse --short HEAD", "commit_summary": "git log -1 --format=%s" } },
    { "name": "push",     "run": "git push origin {{ branch }}",
      "on_error": "sleep 2 && git push origin {{ branch }}" }
  ]
}
```

### Backward compatibility: legacy config migration

When `.auto-push.json` has `pre_push`/`after_push` arrays but no `pipeline`:

1. Build a default pipeline: `[pull, stage, <pre_push commands>, generate, commit, push, <after_push commands>]`
2. Map the old `generate.provider` to a shell command for the generate step
3. Print deprecation warning: `[config] Migrated pre_push/after_push to pipeline. Update your .auto-push.json.`
4. Execute the migrated pipeline

The old `generate.provider` / `ProviderConfig::Preset` / `ProviderConfig::Custom` types are kept for parsing legacy configs only.

## 9. Module Changes

### Files modified

| File | Change |
|---|---|
| `config.rs` | Add `PipelineCommand`, `vars` field, `pipeline` field to `AppConfig`. Add validation logic (var registry). Keep legacy types for backward compat parsing. |
| `template.rs` | Add `resolve_expression()` with dot-path and regex parsing. Promote `extract_regex` from dead code. Add `resolve_json_path()`. |
| `hooks.rs` | Rename to `pipeline.rs`. Generalize `run_phase()` → `run_pipeline()`. Add `capture` / `capture_after` handling. Integrate var registry. |
| `main.rs` | Replace 10-phase orchestration with: preflight → load config → `run_pipeline()`. Remove direct calls to `pull::run`, `push::run`, `stage_commit::run`. |
| `context.rs` | Update `CliFlags`: remove phase-specific booleans, add `skip: Vec<String>`, `var_overrides: Vec<(String, String)>`. |
| `generate.rs` | Becomes thin: only system prompt resolution and style suffix generation (metadata). Remove `call_provider`, `plan_commits`, `fix_push_error`. Keep `resolve_conflicts` for now (Claude-only, interactive). |
| `push.rs` | Remove. Push is a pipeline command. |
| `pull.rs` | Remove. Pull is a pipeline command. |
| `stash.rs` | Remove. Stash is a pipeline command. |
| `stage_commit.rs` | Reduce to multi-commit split detection logic only. The staging and commit execution move to pipeline. |
| `submodule.rs` | Remove or reduce. Submodule sync becomes a pipeline command. |

### Files added

None — this is a consolidation, not expansion.

### Files removed

| File | Reason |
|---|---|
| `push.rs` | Logic moves to pipeline commands |
| `pull.rs` | Logic moves to pipeline commands |
| `stash.rs` | Logic moves to pipeline commands |

## 10. Testing Strategy

### Unit tests

- `template.rs`: dot-path resolution, regex extraction, expression parsing, edge cases (invalid JSON, no match, nested arrays)
- `config.rs`: var registry validation (duplicate detection, forward-reference detection, built-in collision), legacy config migration, pipeline parsing
- `pipeline.rs`: capture behavior, capture_after behavior, multi-commit split detection, `--skip` filtering, `-m` auto-skip logic

### Integration tests

- Full pipeline execution with mock commands (using `echo` and `cat`)
- Legacy config migration produces correct pipeline
- `--skip`, `--var`, `-m` flags work correctly
- Variable chaining: command A captures → command B uses → command C uses B's output
- Error cases: unresolvable var, duplicate capture, invalid regex

### Manual testing

- End-to-end with real `claude` CLI
- Codex CLI with corrected `exec` syntax
- Custom provider via shell command
- Multi-commit splitting with `---` separator
