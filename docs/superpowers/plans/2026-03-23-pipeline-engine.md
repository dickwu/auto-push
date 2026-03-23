# Pipeline Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hardcoded generate/push/pull phases with a unified pipeline engine driven by `.auto-push.json`.

**Architecture:** A single `pipeline` array in config drives all execution. Each entry is a `PipelineCommand` (shell mode or argv mode) run through one engine with a strict variable registry. Template vars support JSON dot-path and regex extraction via a scanning parser. Dynamic built-ins (diff, staged_files) use lazy resolvers that recompute when git state changes.

**Tech Stack:** Rust, serde_json, regex, clap, anyhow, globset

**Spec:** `docs/superpowers/specs/2026-03-23-pipeline-engine-design.md`

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `src/template.rs` | Scanning parser, resolve_expression, dot-path, regex, json_path | Modify |
| `src/vars.rs` | Variable registry: built-in defs, lazy resolvers, validation | Create |
| `src/config.rs` | PipelineCommand type, AppConfig with pipeline/vars, legacy migration | Modify |
| `src/pipeline.rs` | Unified execution engine (renamed from hooks.rs) | Rename+Modify |
| `src/context.rs` | Updated CliFlags (--skip, --var) | Modify |
| `src/generate.rs` | Thin metadata: system prompt resolution, style suffix only | Modify |
| `src/main.rs` | Simplified: preflight → load config → run_pipeline | Modify |
| `src/push.rs` | Remove (push is a pipeline command) | Delete |
| `src/pull.rs` | Remove (pull is a pipeline command) | Delete |
| `src/stash.rs` | Remove (stash is a pipeline command) | Delete |
| `src/stage_commit.rs` | Remove (stage/commit are pipeline commands) | Delete |
| `src/submodule.rs` | Remove (submodule sync is a pipeline command) | Delete |

---

### Task 1: Template Scanner Parser

Replace the regex-based `{{ }}` matcher with a scanning parser that handles `:/regex/` bodies containing `}` characters.

**Files:**
- Modify: `src/template.rs`

- [ ] **Step 1: Write failing tests for the scanner**

Add to `src/template.rs` `mod tests`:

```rust
#[test]
fn test_scan_simple_var() {
    let spans = scan_template_expressions("hello {{ name }} world");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].2, "name");
}

#[test]
fn test_scan_regex_with_brace() {
    // The } inside the regex must not break the parser
    let spans = scan_template_expressions("{{ val:/\\d{7}/ }}");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].2, "val:/\\d{7}/");
}

#[test]
fn test_scan_dot_path() {
    let spans = scan_template_expressions("{{ plan.0.message }}");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].2, "plan.0.message");
}

#[test]
fn test_scan_multiple() {
    let spans = scan_template_expressions("{{ a }} and {{ b.x }}");
    assert_eq!(spans.len(), 2);
}

#[test]
fn test_scan_no_expressions() {
    let spans = scan_template_expressions("no templates here");
    assert_eq!(spans.len(), 0);
}

#[test]
fn test_scan_unclosed_left_asis() {
    let spans = scan_template_expressions("{{ unclosed");
    assert_eq!(spans.len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib template::tests::test_scan -- 2>&1 | head -30`
Expected: compilation errors (function doesn't exist)

- [ ] **Step 3: Implement `scan_template_expressions`**

Add to `src/template.rs`:

```rust
/// Scan a template string for {{ expression }} spans.
/// Handles :/regex/ bodies that may contain } characters.
/// Returns Vec of (start_byte, end_byte, trimmed_expression).
pub fn scan_template_expressions(input: &str) -> Vec<(usize, usize, &str)> {
    let mut results = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i;
            i += 2; // skip {{

            // Skip leading whitespace
            while i < len && bytes[i] == b' ' {
                i += 1;
            }
            let expr_start = i;
            let mut in_regex = false;

            // Scan for }} but respect :/ ... / regex bodies
            while i + 1 < len {
                if !in_regex && bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b'/' {
                    in_regex = true;
                    i += 2;
                    continue;
                }
                if in_regex && bytes[i] == b'/' {
                    // Check it's not an escaped slash
                    let escaped = i > 0 && bytes[i - 1] == b'\\';
                    if !escaped {
                        in_regex = false;
                        i += 1;
                        continue;
                    }
                }
                if !in_regex && bytes[i] == b'}' && bytes[i + 1] == b'}' {
                    // Found closing }}
                    let expr_end = i;
                    // Trim trailing whitespace from expression
                    let expr = input[expr_start..expr_end].trim();
                    if !expr.is_empty() {
                        results.push((start, i + 2, expr));
                    }
                    i += 2;
                    break;
                }
                i += 1;
            }
            // If we ran off the end without finding }}, skip (unclosed)
        } else {
            i += 1;
        }
    }

    results
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib template::tests::test_scan -v`
Expected: all 6 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/template.rs
git commit -m "feat: add scanning template parser for regex-safe {{ }} expressions"
```

---

### Task 2: Expression Resolver (dot-path + regex)

Add `resolve_expression()` that handles simple vars, JSON dot-path, and regex extraction. Wire it into `render_shell` and `render_raw`.

**Files:**
- Modify: `src/template.rs`

- [ ] **Step 1: Write failing tests for expression resolution**

```rust
#[test]
fn test_resolve_simple_var() {
    let v = vars(&[("name", "hello")]);
    assert_eq!(resolve_expression("name", &v).unwrap(), "hello");
}

#[test]
fn test_resolve_unknown_var_errors() {
    let v = vars(&[]);
    assert!(resolve_expression("missing", &v).is_err());
}

#[test]
fn test_resolve_dot_path_object() {
    let v = vars(&[("data", r#"{"status":"ok","count":3}"#)]);
    assert_eq!(resolve_expression("data.status", &v).unwrap(), "ok");
    assert_eq!(resolve_expression("data.count", &v).unwrap(), "3");
}

#[test]
fn test_resolve_dot_path_array() {
    let v = vars(&[("items", r#"[{"name":"a"},{"name":"b"}]"#)]);
    assert_eq!(resolve_expression("items.0.name", &v).unwrap(), "a");
    assert_eq!(resolve_expression("items.1.name", &v).unwrap(), "b");
}

#[test]
fn test_resolve_dot_path_length() {
    let v = vars(&[("arr", r#"[1,2,3]"#)]);
    assert_eq!(resolve_expression("arr.length", &v).unwrap(), "3");
}

#[test]
fn test_resolve_dot_path_not_json_errors() {
    let v = vars(&[("plain", "just text")]);
    assert!(resolve_expression("plain.field", &v).is_err());
}

#[test]
fn test_resolve_regex_capture_group() {
    let v = vars(&[("ver", "release v1.2.3 deployed")]);
    assert_eq!(
        resolve_expression("ver:/v(\\d+\\.\\d+\\.\\d+)/", &v).unwrap(),
        "1.2.3"
    );
}

#[test]
fn test_resolve_regex_no_match() {
    let v = vars(&[("text", "no numbers here")]);
    // No match returns empty string (consistent with existing extract_regex)
    assert_eq!(resolve_expression("text:/\\d+/", &v).unwrap(), "");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib template::tests::test_resolve -- 2>&1 | head -20`
Expected: compilation errors

- [ ] **Step 3: Implement expression resolver**

Add to `src/template.rs`:

```rust
use anyhow::{Result, anyhow};

/// Parse "var_name:/pattern/" into (var_name, pattern)
fn parse_regex_expr(expr: &str) -> Option<(&str, &str)> {
    let idx = expr.find(":/")?;
    let var_name = &expr[..idx];
    let rest = &expr[idx + 2..];
    let pattern = rest.strip_suffix('/')?;
    Some((var_name.trim(), pattern))
}

/// Parse "var_name.field.0.nested" into (var_name, path_segments)
fn parse_dot_path(expr: &str) -> Option<(&str, Vec<&str>)> {
    let dot_idx = expr.find('.')?;
    let var_name = &expr[..dot_idx];
    let path_str = &expr[dot_idx + 1..];
    let segments: Vec<&str> = path_str.split('.').collect();
    if segments.is_empty() {
        return None;
    }
    Some((var_name.trim(), segments))
}

/// Navigate a serde_json::Value by dot-path segments.
fn resolve_json_path(value: &serde_json::Value, segments: &[&str]) -> Result<String> {
    let mut current = value;

    for segment in segments {
        if *segment == "length" {
            if let Some(arr) = current.as_array() {
                return Ok(arr.len().to_string());
            }
            return Err(anyhow!("'length' used on non-array value"));
        }

        // Try as array index
        if let Ok(idx) = segment.parse::<usize>() {
            current = current
                .get(idx)
                .ok_or_else(|| anyhow!("array index {idx} out of bounds"))?;
        } else {
            current = current
                .get(*segment)
                .ok_or_else(|| anyhow!("field '{}' not found", segment))?;
        }
    }

    // Convert final value to string
    match current {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Null => Ok("null".to_string()),
        other => Ok(other.to_string()),
    }
}

/// Resolve a template expression to its string value.
/// Supports: simple var, dot-path (JSON), regex extraction.
pub fn resolve_expression(expr: &str, vars: &HashMap<String, String>) -> Result<String> {
    // 1. Regex: "var_name:/pattern/"
    if let Some((var_name, pattern)) = parse_regex_expr(expr) {
        let raw = vars
            .get(var_name)
            .ok_or_else(|| anyhow!("unknown variable: '{var_name}'"))?;
        return Ok(extract_regex(raw, pattern));
    }

    // 2. Dot-path: "var_name.field.0.nested"
    if let Some((var_name, segments)) = parse_dot_path(expr) {
        let raw = vars
            .get(var_name)
            .ok_or_else(|| anyhow!("unknown variable: '{var_name}'"))?;
        let json: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| anyhow!("variable '{var_name}' is not valid JSON for dot-path access"))?;
        return resolve_json_path(&json, &segments);
    }

    // 3. Simple var
    vars.get(expr)
        .cloned()
        .ok_or_else(|| anyhow!("unknown variable: '{expr}'"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib template::tests::test_resolve -v`
Expected: all 8 tests PASS

- [ ] **Step 5: Wire scanner + resolver into render_shell/render_raw**

Replace the bodies of `render_shell` and `render_raw` to use `scan_template_expressions` + `resolve_expression` instead of the old regex + HashMap::get. Keep backward compat: if `resolve_expression` fails (unknown var), fall back to leaving `{{ expr }}` as-is (matching current behavior for unresolved vars in shell mode).

- [ ] **Step 6: Run ALL existing template tests**

Run: `cargo test --lib template::tests -v`
Expected: all existing + new tests PASS

- [ ] **Step 7: Commit**

```bash
git add src/template.rs
git commit -m "feat: add expression resolver with JSON dot-path and regex extraction"
```

---

### Task 3: Config Types (PipelineCommand, AppConfig, CaptureMode)

Update config types to support the new pipeline schema while keeping backward compat.

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing tests for new config types**

```rust
#[test]
fn test_pipeline_command_shell_mode() {
    let json = r#"{"name": "test", "run": "cargo test"}"#;
    let cmd: PipelineCommand = serde_json::from_str(json).unwrap();
    assert_eq!(cmd.name, "test");
    assert_eq!(cmd.run.as_deref(), Some("cargo test"));
    assert!(cmd.command.is_none());
}

#[test]
fn test_pipeline_command_argv_mode() {
    let json = r#"{"name": "gen", "command": "claude", "args": ["-p", "{{ diff }}"]}"#;
    let cmd: PipelineCommand = serde_json::from_str(json).unwrap();
    assert_eq!(cmd.command.as_deref(), Some("claude"));
    assert_eq!(cmd.args.as_ref().unwrap().len(), 2);
    assert!(cmd.run.is_none());
}

#[test]
fn test_pipeline_command_capture() {
    let json = r#"{"name": "gen", "run": "echo hello", "capture": "msg"}"#;
    let cmd: PipelineCommand = serde_json::from_str(json).unwrap();
    assert_eq!(cmd.capture.as_deref(), Some("msg"));
}

#[test]
fn test_pipeline_command_capture_after_ordered() {
    let json = r#"{"name": "c", "run": "git commit", "capture_after": [{"name": "hash", "run": "git rev-parse HEAD"}, {"name": "msg", "run": "git log -1 --format=%s"}]}"#;
    let cmd: PipelineCommand = serde_json::from_str(json).unwrap();
    let ca = cmd.capture_after.unwrap();
    assert_eq!(ca.len(), 2);
    assert_eq!(ca[0].name, "hash");
    assert_eq!(ca[1].name, "msg");
}

#[test]
fn test_pipeline_command_capture_mode() {
    let json = r#"{"name": "t", "run": "cargo test", "capture": "out", "capture_mode": "both"}"#;
    let cmd: PipelineCommand = serde_json::from_str(json).unwrap();
    assert!(matches!(cmd.capture_mode, Some(CaptureMode::Both)));
}

#[test]
fn test_app_config_with_pipeline() {
    let json = r#"{"pipeline": [{"name": "test", "run": "cargo test"}]}"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    assert!(config.pipeline.is_some());
    assert_eq!(config.pipeline.unwrap().len(), 1);
}

#[test]
fn test_app_config_with_vars() {
    let json = r#"{"vars": {"team": "backend"}, "pipeline": []}"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.vars.get("team").unwrap(), "backend");
}

#[test]
fn test_app_config_legacy_still_parses() {
    let json = r#"{"pre_push": [{"name": "test", "run": "cargo test"}], "after_push": []}"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    assert!(config.pipeline.is_none());
    assert_eq!(config.pre_push.len(), 1);
}

#[test]
fn test_generate_config_legacy_provider_parses() {
    let json = r#"{"generate": {"provider": "claude"}, "pipeline": []}"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    assert!(config.generate.provider.is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::tests::test_pipeline -- 2>&1 | head -20`
Expected: compilation errors (types don't exist yet)

- [ ] **Step 3: Implement new config types**

Update `src/config.rs`:
- Add `PipelineCommand` struct (with `run`, `command`, `args`, `capture`, `capture_after`, `capture_mode`)
- Add `CaptureAfterEntry` struct
- Add `CaptureMode` enum
- Update `AppConfig` (remove `deny_unknown_fields`, add `vars`, `pipeline`)
- Update `GenerateConfig` (remove `deny_unknown_fields`, make `provider` optional)
- Keep all existing types (`HookCommand` → rename to `PipelineCommand`, `ProviderConfig`, etc.)

- [ ] **Step 4: Fix compilation across all modules**

Update all `use` statements and type references in other files that reference `HookCommand` to use `PipelineCommand`. Run `cargo check` until clean.

- [ ] **Step 5: Run ALL config tests**

Run: `cargo test --lib config::tests -v`
Expected: all old + new tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/hooks.rs src/main.rs
git commit -m "feat: add PipelineCommand, CaptureMode, and pipeline/vars to AppConfig"
```

---

### Task 4: Variable Registry & Validation

Implement the variable registry with built-in definitions, duplicate detection, and forward-reference validation.

**Files:**
- Create: `src/vars.rs`
- Modify: `src/config.rs` (call validation from `load()`)

- [ ] **Step 1: Write failing tests**

Create `src/vars.rs` with test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_names_defined() {
        let names = builtin_var_names();
        assert!(names.contains("branch"));
        assert!(names.contains("diff"));
        assert!(names.contains("system_prompt"));
    }

    #[test]
    fn test_validate_no_duplicate_vars_captures() {
        // capture collides with another capture
        let pipeline = vec![
            cmd_with_capture("gen1", "msg"),
            cmd_with_capture("gen2", "msg"), // duplicate!
        ];
        let vars = HashMap::new();
        assert!(validate_var_registry(&pipeline, &vars).is_err());
    }

    #[test]
    fn test_validate_var_collides_with_builtin() {
        let pipeline = vec![];
        let mut vars = HashMap::new();
        vars.insert("branch".to_string(), "override".to_string());
        assert!(validate_var_registry(&pipeline, &vars).is_err());
    }

    #[test]
    fn test_validate_forward_reference_error() {
        // commit uses {{ msg }} but gen (which captures msg) comes after
        let pipeline = vec![
            cmd_using_var("commit", "git commit -m '{{ msg }}'"),
            cmd_with_capture("gen", "msg"),
        ];
        let vars = HashMap::new();
        assert!(validate_var_registry(&pipeline, &vars).is_err());
    }

    #[test]
    fn test_validate_correct_order_passes() {
        let pipeline = vec![
            cmd_with_capture("gen", "msg"),
            cmd_using_var("commit", "git commit -m '{{ msg }}'"),
        ];
        let vars = HashMap::new();
        assert!(validate_var_registry(&pipeline, &vars).is_ok());
    }

    #[test]
    fn test_validate_user_var_resolves() {
        let pipeline = vec![
            cmd_using_var("notify", "echo {{ team }}"),
        ];
        let mut vars = HashMap::new();
        vars.insert("team".to_string(), "backend".to_string());
        assert!(validate_var_registry(&pipeline, &vars).is_ok());
    }

    #[test]
    fn test_validate_interactive_with_capture_rejected() {
        let mut cmd = cmd_with_capture("gen", "msg");
        cmd.interactive = true;
        let pipeline = vec![cmd];
        let vars = HashMap::new();
        assert!(validate_var_registry(&pipeline, &vars).is_err());
    }

    #[test]
    fn test_validate_run_or_command_required() {
        let cmd = PipelineCommand {
            name: "bad".into(),
            run: None,
            command: None,
            ..Default::default()
        };
        let pipeline = vec![cmd];
        let vars = HashMap::new();
        assert!(validate_var_registry(&pipeline, &vars).is_err());
    }
}
```

- [ ] **Step 2: Implement variable registry**

In `src/vars.rs`:
- `builtin_var_names() -> HashSet<String>` — returns all built-in var names
- `validate_var_registry(pipeline, vars) -> Result<()>` — runs Rules 1-3 from spec
- Helper: `extract_var_references(template: &str) -> Vec<String>` — uses `scan_template_expressions` to find var names
- Validation for `run` XOR `command`, `interactive` + `capture` rejection

- [ ] **Step 3: Run tests**

Run: `cargo test --lib vars::tests -v`
Expected: all tests PASS

- [ ] **Step 4: Wire validation into config::load**

In `src/config.rs`, after loading and merging config, call `vars::validate_var_registry()`.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/vars.rs src/config.rs src/main.rs
git commit -m "feat: add variable registry with duplicate and forward-reference validation"
```

---

### Task 5: Pipeline Execution Engine

Rename `hooks.rs` → `pipeline.rs`. Implement `run_pipeline()` with capture, capture_after, argv mode, lazy dynamic vars, and `--skip`/`-m` logic.

**Files:**
- Rename: `src/hooks.rs` → `src/pipeline.rs`
- Modify: `src/pipeline.rs`
- Modify: `src/main.rs` (update `mod` declaration)

- [ ] **Step 1: Rename hooks.rs to pipeline.rs**

```bash
git mv src/hooks.rs src/pipeline.rs
```

Update `src/main.rs`: `mod hooks;` → `mod pipeline;`
Update all references from `hooks::` to `pipeline::`.

- [ ] **Step 2: Verify rename compiles**

Run: `cargo check`
Expected: clean (pure rename, no logic changes)

- [ ] **Step 3: Commit rename**

```bash
git add -A
git commit -m "refactor: rename hooks.rs to pipeline.rs"
```

- [ ] **Step 4: Write failing tests for capture**

Add to `src/pipeline.rs` tests:

```rust
#[test]
fn test_execute_with_capture_stdout_only() {
    let (output, success) = execute_command_split("echo hello && echo err >&2", false).unwrap();
    assert!(success);
    assert_eq!(output.trim(), "hello");
}

#[test]
fn test_execute_with_capture_both() {
    let (output, success) = execute_command_both("echo out && echo err >&2", false).unwrap();
    assert!(success);
    assert!(output.contains("out"));
    assert!(output.contains("err"));
}
```

- [ ] **Step 5: Implement split stdout/stderr capture**

Add `execute_command_split()` that captures stdout separately from stderr (stderr streams to terminal only). Refactor existing `execute_command()` to delegate to either split or combined mode based on `CaptureMode`.

- [ ] **Step 6: Write failing tests for run_pipeline**

```rust
#[test]
fn test_run_pipeline_basic() {
    let commands = vec![
        PipelineCommand { name: "greet".into(), run: Some("echo hello".into()), ..Default::default() },
    ];
    let mut ctx = make_pipeline_ctx();
    let result = run_pipeline(&commands, &mut ctx, &[], false, false);
    assert!(result.is_ok());
}

#[test]
fn test_run_pipeline_capture_chains() {
    let commands = vec![
        PipelineCommand { name: "gen".into(), run: Some("echo world".into()), capture: Some("msg".into()), ..Default::default() },
        PipelineCommand { name: "use".into(), run: Some("echo {{ msg }}".into()), ..Default::default() },
    ];
    let mut ctx = make_pipeline_ctx();
    let result = run_pipeline(&commands, &mut ctx, &[], false, false);
    assert!(result.is_ok());
    assert_eq!(ctx.vars.get("msg").unwrap(), "world");
}

#[test]
fn test_run_pipeline_skip() {
    let commands = vec![
        PipelineCommand { name: "skip_me".into(), run: Some("exit 1".into()), ..Default::default() },
        PipelineCommand { name: "keep".into(), run: Some("echo ok".into()), ..Default::default() },
    ];
    let mut ctx = make_pipeline_ctx();
    let result = run_pipeline(&commands, &mut ctx, &["skip_me".to_string()], false, false);
    assert!(result.is_ok());
}
```

- [ ] **Step 7: Implement run_pipeline**

Replace `run_phase()` with `run_pipeline()`:
- Iterate commands
- Check `--skip` list
- Check `-m` auto-skip for capture: "commit_message"
- Resolve templates (shell mode → `render_shell`, argv mode → `render_raw` per arg)
- Handle confirm prompts (explicit + `--confirm` implicit)
- Execute (shell mode vs argv mode)
- Handle capture / capture_after
- Handle on_error
- Invalidate dynamic var cache after git-mutating commands

- [ ] **Step 8: Implement argv mode execution**

Add `execute_argv(command: &str, args: &[String], interactive: bool) -> Result<(String, bool)>` that uses `Command::new(command).args(args)` without shell.

- [ ] **Step 9: Run all pipeline tests**

Run: `cargo test --lib pipeline::tests -v`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat: implement unified pipeline engine with capture, argv mode, and --skip"
```

---

### Task 6: Lazy Dynamic Vars

Implement the lazy resolver for dynamic built-ins (diff, staged_files, etc.) that recomputes when git state changes.

**Files:**
- Modify: `src/vars.rs`
- Modify: `src/pipeline.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_lazy_var_computes_on_first_access() {
    let mut resolver = LazyVarResolver::new(20_000);
    // In a git repo with staged changes, diff should be non-empty
    // For unit test, use a mock
    resolver.register_mock("diff", "mock diff content");
    assert_eq!(resolver.get("diff").unwrap(), "mock diff content");
}

#[test]
fn test_lazy_var_cache_invalidation() {
    let mut resolver = LazyVarResolver::new(20_000);
    resolver.register_mock("diff", "v1");
    assert_eq!(resolver.get("diff").unwrap(), "v1");
    resolver.invalidate_cache();
    resolver.register_mock("diff", "v2");
    assert_eq!(resolver.get("diff").unwrap(), "v2");
}

#[test]
fn test_is_git_mutating_command() {
    assert!(is_git_mutating("git add -A"));
    assert!(is_git_mutating("git commit -m 'msg'"));
    assert!(is_git_mutating("git stash pop"));
    assert!(is_git_mutating("git pull"));
    assert!(!is_git_mutating("cargo test"));
    assert!(!is_git_mutating("echo hello"));
}
```

- [ ] **Step 2: Implement LazyVarResolver**

In `src/vars.rs`:
- `LazyVarResolver` struct with cached values and invalidation
- `is_git_mutating(cmd: &str) -> bool` — checks if command modifies git state
- Integration into pipeline: after each command executes, check if it's git-mutating and invalidate cache

- [ ] **Step 3: Run tests**

Run: `cargo test --lib vars -v`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/vars.rs src/pipeline.rs
git commit -m "feat: add lazy resolver for dynamic built-in vars with cache invalidation"
```

---

### Task 7: CLI Flags (--skip, --var, deprecation)

Update clap args and CliFlags for the new pipeline model.

**Files:**
- Modify: `src/main.rs`
- Modify: `src/context.rs`

- [ ] **Step 1: Update CliFlags struct**

In `src/context.rs`, replace phase-specific booleans with:
```rust
pub skip: Vec<String>,
pub var_overrides: Vec<(String, String)>,
```

Keep `no_push`, `no_pull`, `no_generate`, `no_stash`, `rebase` for deprecation.

- [ ] **Step 2: Update clap args in main.rs**

Add `--skip` (repeatable), `--var` (repeatable, `key=value` format).
Keep old flags with deprecation warnings that map to `--skip`.

- [ ] **Step 3: Write tests**

```rust
#[test]
fn test_deprecation_mapping() {
    let mut skip = vec![];
    apply_deprecation_flags(&mut skip, true, false, false, false, false);
    assert!(skip.contains(&"pull".to_string()));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib context -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/context.rs
git commit -m "feat: add --skip and --var CLI flags with deprecation wrappers"
```

---

### Task 8: Legacy Config Migration

Implement runtime migration from `pre_push`/`after_push` to `pipeline`.

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_migrate_legacy_config() {
    let json = r#"{
        "generate": {"provider": "claude"},
        "pre_push": [{"name": "test", "run": "cargo test"}],
        "after_push": [{"name": "notify", "run": "echo done"}]
    }"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    let pipeline = migrate_to_pipeline(&config).unwrap();
    // Should have: stash, pull, unstash, stage, test (pre_push), generate, commit, push, notify (after_push)
    assert!(pipeline.iter().any(|c| c.name == "pull"));
    assert!(pipeline.iter().any(|c| c.name == "test"));
    assert!(pipeline.iter().any(|c| c.name == "notify"));
    // Generate should use argv mode for claude
    let gen = pipeline.iter().find(|c| c.name == "generate").unwrap();
    assert!(gen.command.is_some());
    assert_eq!(gen.capture.as_deref(), Some("commit_message"));
    // Migrated hooks should have capture_mode: Both
    let test_cmd = pipeline.iter().find(|c| c.name == "test").unwrap();
    assert!(matches!(test_cmd.capture_mode, Some(CaptureMode::Both)));
}

#[test]
fn test_migrate_custom_provider() {
    let json = r#"{
        "generate": {"provider": {"command": "my-ai", "args": ["--prompt", "{{ prompt }}"]}},
        "pre_push": []
    }"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    let pipeline = migrate_to_pipeline(&config).unwrap();
    let gen = pipeline.iter().find(|c| c.name == "generate").unwrap();
    assert_eq!(gen.command.as_deref(), Some("my-ai"));
}

#[test]
fn test_no_migration_when_pipeline_present() {
    let json = r#"{"pipeline": [{"name": "test", "run": "echo ok"}]}"#;
    let config: AppConfig = serde_json::from_str(json).unwrap();
    // pipeline is Some, no migration needed
    assert!(config.pipeline.is_some());
}
```

- [ ] **Step 2: Implement migrate_to_pipeline**

In `src/config.rs`:
- `migrate_to_pipeline(config: &AppConfig) -> Result<Vec<PipelineCommand>>`
- Map provider presets to argv mode commands
- Insert stash/pull/unstash/stage before pre_push commands
- Insert generate/commit/push after pre_push, before after_push
- Set `capture_mode: Both` on migrated hooks

- [ ] **Step 3: Wire into config::load**

In `load()`: if `pipeline` is None and `pre_push`/`after_push` exist, call `migrate_to_pipeline()` and print deprecation warning.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib config -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add legacy config migration from pre_push/after_push to pipeline"
```

---

### Task 9: Simplify main.rs & Slim generate.rs

Replace the 10-phase orchestration with: preflight → load config → build vars → run_pipeline.

**Files:**
- Modify: `src/main.rs`
- Modify: `src/generate.rs`

- [ ] **Step 1: Slim down generate.rs**

Remove: `call_provider`, `plan_commits`, `fix_push_error`, `wait_with_timeout`, `extract_commands`, `generate_commit_message`.
Keep: `resolve_system_prompt` (renamed to `pub fn build_system_prompt`), `style_suffix`, `resolve_conflicts` (for now), built-in prompt constants.

- [ ] **Step 2: Rewrite main.rs**

Replace phases 3-10 with:
```rust
// Build template context with built-in + user vars
let mut vars = vars::build_initial_context(&ctx, &app_config)?;

// Resolve pipeline (from config or migration)
let pipeline = app_config.pipeline.unwrap_or_else(|| {
    config::migrate_to_pipeline(&app_config).unwrap_or_default()
});

// Execute
pipeline::run_pipeline(&pipeline, &mut vars, &ctx.cli.skip, ctx.cli.dry_run, ctx.cli.force)?;
```

- [ ] **Step 3: Remove unused mod declarations**

Remove from `main.rs`:
```rust
// mod pull;    — removed
// mod push;    — removed
// mod stash;   — removed
// mod stage_commit; — removed
// mod submodule;    — removed
```

- [ ] **Step 4: Verify build**

Run: `cargo check`
Expected: clean (with warnings about unused files we'll delete next)

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: PASS (some old integration tests may need updating)

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/generate.rs
git commit -m "refactor: simplify main.rs to preflight → config → run_pipeline"
```

---

### Task 10: Delete Removed Modules & Update Auto-Init

Remove dead code and update auto-init to generate pipeline configs.

**Files:**
- Delete: `src/push.rs`, `src/pull.rs`, `src/stash.rs`, `src/stage_commit.rs`, `src/submodule.rs`
- Modify: `src/config.rs` (auto_init generates pipeline)

- [ ] **Step 1: Delete removed modules**

```bash
git rm src/push.rs src/pull.rs src/stash.rs src/stage_commit.rs src/submodule.rs
```

- [ ] **Step 2: Verify build**

Run: `cargo check`
Expected: clean

- [ ] **Step 3: Update auto_init to generate pipeline config**

In `src/config.rs`, update `auto_init()`:
- Detect provider → generate argv-mode command
- Detect project type → generate test/lint commands
- Build full pipeline array with stash/pull/unstash/tests/lint/stage/generate/commit/push
- Write as pipeline config (not pre_push/after_push)

- [ ] **Step 4: Write test for new auto_init**

```rust
#[test]
fn test_auto_init_creates_pipeline_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    auto_init(dir.path()).unwrap();
    let content = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(val["pipeline"].is_array());
    assert!(val.get("pre_push").is_none()); // no legacy format
}
```

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo clippy -- -D warnings && cargo fmt -- --check`
Expected: clean

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: complete pipeline engine — remove legacy modules, update auto-init"
```

---

## Errata (from plan review)

These fixes address issues found by the plan reviewer. **Read before implementing.**

### E1: `PipelineCommand` needs `#[derive(Default)]`

Add `#[derive(Default)]` to `PipelineCommand` in Task 3. All fields are `Option`, `bool`, or `String` so this derives cleanly. Test code uses `..Default::default()`.

### E2: `capture_after` JSON shape mismatch in spec examples

The spec's JSON examples show `capture_after` as an object (`{"hash": "git rev-parse HEAD"}`), but the Rust type is `Vec<CaptureAfterEntry>`. During implementation, use the **array** form in JSON: `[{"name": "hash", "run": "git rev-parse HEAD"}]`. This is correct per the `PipelineCommand` struct. The spec examples are outdated.

### E3: Task 5 Step 7 breakdown (run_pipeline is too coarse)

Break "Implement run_pipeline" into these sub-steps:
1. **Iteration + skip logic**: loop over commands, check `--skip` and `-m` auto-skip
2. **Template resolution**: shell mode uses `render_shell`, argv mode uses `render_raw` per arg
3. **Confirm prompt handling**: explicit `confirm` field + `--confirm` implicit prompt
4. **Execution dispatch**: shell mode → `execute_command`, argv mode → `execute_argv`
5. **Capture + capture_after**: store stdout, run post-commands
6. **Error handling**: on_error dispatch, bail on failure
7. **Cache invalidation**: after each command, check `is_git_mutating` and invalidate lazy var cache

### E4: Missing `--provider` flag

In Task 7, add `--provider` to clap args. Implementation: if `--provider <name>` is set, find the pipeline command named "generate" and replace its `run`/`command`/`args` with the preset for that provider (same mapping as legacy migration in Task 8).

### E5: Missing tests

Add these tests during their respective tasks:

**Task 5 — argv mode execution:**
```rust
#[test]
fn test_execute_argv_basic() {
    let (output, success) = execute_argv("echo", &["hello".to_string()], false).unwrap();
    assert!(success);
    assert_eq!(output.trim(), "hello");
}
```

**Task 5 — `-m` auto-skip:**
```rust
#[test]
fn test_run_pipeline_m_flag_skips_generate() {
    let commands = vec![
        PipelineCommand { name: "gen".into(), run: Some("exit 1".into()), capture: Some("commit_message".into()), ..Default::default() },
        PipelineCommand { name: "commit".into(), run: Some("echo ok".into()), ..Default::default() },
    ];
    let mut ctx = make_pipeline_ctx();
    ctx.vars.insert("commit_message".to_string(), "manual msg".to_string());
    let result = run_pipeline(&commands, &mut ctx, &[], false, false);
    assert!(result.is_ok()); // gen skipped because commit_message already set
}
```

**Task 5 — dry-run:**
```rust
#[test]
fn test_run_pipeline_dry_run_does_not_execute() {
    let commands = vec![
        PipelineCommand { name: "fail".into(), run: Some("exit 1".into()), ..Default::default() },
    ];
    let mut ctx = make_pipeline_ctx();
    let result = run_pipeline(&commands, &mut ctx, &[], true, false); // dry_run=true
    assert!(result.is_ok()); // should not fail because command was not executed
}
```

**Task 7 — `--var` override + built-in rejection:**
```rust
#[test]
fn test_var_override() {
    let mut vars = HashMap::new();
    vars.insert("team".to_string(), "original".to_string());
    apply_var_overrides(&mut vars, &[("team".to_string(), "override".to_string())]).unwrap();
    assert_eq!(vars.get("team").unwrap(), "override");
}

#[test]
fn test_var_override_builtin_rejected() {
    let mut vars = HashMap::new();
    let result = apply_var_overrides(&mut vars, &[("branch".to_string(), "x".to_string())]);
    assert!(result.is_err());
}
```

### E6: `render_shell`/`render_raw` replacement algorithm

When replacing the regex-based `replace_all` with the scanner in Task 2 Step 5:
1. Call `scan_template_expressions(template)` to get `Vec<(start, end, expr)>`
2. Build output string by concatenating:
   - `template[prev_end..span.start]` (literal text between expressions)
   - resolved value (from `resolve_expression`, shell-escaped for `render_shell`)
3. Append remaining `template[last_end..]`
4. If `resolve_expression` fails, use the original `{{ expr }}` text (backward compat)

### E7: Module disposition

- `src/diff.rs` — **Keep for now.** The `{{ hunks }}` built-in var needs `diff::parse_diff` and `diff::format_hunks_for_prompt`. Remove only the hunk-to-patch functions unused by the pipeline.
- `src/git.rs` — **Keep.** Still needed for preflight checks, lazy var computation (`git diff --cached`, `git rev-parse`), and may be used by future built-in helpers.
- `src/preflight.rs` — **Keep unchanged.** Its `PreflightResult` feeds the var registry via `vars::build_initial_context()` in Task 6.

### E8: Var name format validation

In Task 4, add validation that all var names (from `vars` section and `capture` fields) match `^[a-zA-Z_][a-zA-Z0-9_]*$`. This prevents names containing `:/` or `.` from colliding with the dot-path and regex expression syntax.

---

## Post-Implementation

After all 10 tasks pass:
1. Run `cargo build --release` to verify release build
2. Manual test: `cargo run -- --show-config` in a test repo
3. Manual test: `cargo run -- --dry-run` to verify pipeline execution output
4. Manual test: `cargo run` end-to-end with real claude CLI
5. Update `CLAUDE.md` architecture section to reflect new module structure
