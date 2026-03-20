use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const CONFIG_FILE: &str = ".auto-push.json";

// ---------------------------------------------------------------------------
// Task 3: Config types and loading
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub pre_push: Vec<HookCommand>,
    #[serde(default)]
    pub after_push: Vec<HookCommand>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HookCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub run: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    /// Optional confirmation prompt shown before running the command.
    /// Supports `{{ variable }}` template substitution.
    /// If the user declines: pre_push hooks abort the push, after_push hooks skip the command.
    /// Auto-accepted when `--force` is set or no TTY is available (e.g. CI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    /// When true, the command gets full TTY passthrough (stdin/stdout/stderr inherited).
    /// Output is NOT captured, so `{{ command_output.NAME }}` will be empty.
    /// Falls back to piped mode when no TTY is available.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interactive: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookPhase {
    PrePush,
    AfterPush,
}

impl HookPhase {
    pub fn label(&self) -> &'static str {
        match self {
            HookPhase::PrePush => "pre_push",
            HookPhase::AfterPush => "after_push",
        }
    }
}

pub struct TemplateContext {
    pub branch: String,
    pub remote: String,
    pub commit_hash: String,
    pub command_outputs: HashMap<String, String>,
}

pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(CONFIG_FILE)
}

pub fn load_config(repo_root: &Path) -> Result<Option<HooksConfig>> {
    let path = config_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let config: HooksConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    validate_config(&config)?;

    Ok(Some(config))
}

pub fn validate_config(config: &HooksConfig) -> Result<()> {
    validate_unique_names(&config.pre_push, "pre_push")?;
    validate_unique_names(&config.after_push, "after_push")?;
    Ok(())
}

pub fn validate_unique_names(commands: &[HookCommand], phase: &str) -> Result<()> {
    let mut seen = HashMap::new();
    for cmd in commands {
        if seen.insert(cmd.name.as_str(), true).is_some() {
            bail!("duplicate command name '{}' in {} phase", cmd.name, phase);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Task 4: Template engine
// ---------------------------------------------------------------------------

/// Sanitize a value before interpolating it into a shell command template.
/// Trims whitespace, escapes shell metacharacters, normalises newlines,
/// and truncates to 4096 chars.
pub fn sanitize_template_value(raw: &str) -> String {
    let trimmed = raw.trim();

    // Normalise carriage returns first so we can process line-by-line
    let no_cr = trimmed.replace("\r\n", "\n").replace('\r', "");

    // Escape shell metacharacters including backslash.
    let shell_chars = [
        '\'', '"', '`', '$', '!', '(', ')', '|', '&', ';', '<', '>', '\\',
    ];
    let mut escaped = String::with_capacity(no_cr.len());
    for ch in no_cr.chars() {
        if shell_chars.contains(&ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }

    // Replace actual newline characters with the two-char sequence `\n`
    // (after shell-char escaping so the inserted backslash is not re-escaped).
    let normalised = escaped.replace('\n', "\\n");

    // Truncate to 4096 chars (by char count to avoid splitting multi-byte),
    // appending a marker so callers know the value was truncated.
    if normalised.chars().count() > 4096 {
        let truncated: String = normalised.chars().take(4096).collect();
        format!("{truncated}...(truncated)")
    } else {
        normalised
    }
}

fn template_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{\s*([^}]+?)\s*\}\}").expect("static regex"))
}

/// Render a template string, replacing `{{ var }}` patterns using the
/// provided context.  Variables that cannot be resolved are left as-is.
pub fn render_template(
    template: &str,
    ctx: &TemplateContext,
    command_name: &str,
    command_run: &str,
    phase: HookPhase,
) -> String {
    let re = template_regex();

    re.replace_all(template, |caps: &regex::Captures| {
        let expr = caps[1].trim();
        resolve_expression(expr, ctx, command_name, command_run, phase)
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

fn resolve_expression(
    expr: &str,
    ctx: &TemplateContext,
    command_name: &str,
    command_run: &str,
    phase: HookPhase,
) -> Option<String> {
    match expr {
        "branch" => Some(sanitize_template_value(&ctx.branch)),
        "remote" => Some(sanitize_template_value(&ctx.remote)),
        "commit_hash" => Some(sanitize_template_value(&ctx.commit_hash)),
        "command_name" => Some(sanitize_template_value(command_name)),
        "command_run" => Some(sanitize_template_value(command_run)),
        "command_type" => Some(phase.label().to_string()),
        _ if expr.starts_with("command_output.") => Some(resolve_command_output(
            &expr["command_output.".len()..],
            ctx,
        )),
        _ => None,
    }
}

fn resolve_command_output(expr: &str, ctx: &TemplateContext) -> String {
    // Check for pipe: `NAME | /regex/`
    if let Some(pipe_pos) = expr.find('|') {
        let name = expr[..pipe_pos].trim();
        let rest = expr[pipe_pos + 1..].trim();

        let raw_output = ctx
            .command_outputs
            .get(name)
            .map(String::as_str)
            .unwrap_or("");

        // Extract regex pattern from /pattern/ — operate on raw output first,
        // then sanitize the extracted result.
        if rest.starts_with('/') && rest.ends_with('/') && rest.len() > 2 {
            let pattern = &rest[1..rest.len() - 1];
            let extracted = extract_regex(raw_output, pattern);
            sanitize_template_value(&extracted)
        } else {
            sanitize_template_value(raw_output)
        }
    } else {
        let name = expr.trim();
        let raw_output = ctx
            .command_outputs
            .get(name)
            .map(String::as_str)
            .unwrap_or("");
        sanitize_template_value(raw_output)
    }
}

/// Apply a regex to `text`.  Returns:
/// - the first capture group if present,
/// - or the full match if no capture groups,
/// - or an empty string if no match.
pub fn extract_regex(text: &str, pattern: &str) -> String {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    if let Some(caps) = re.captures(text) {
        if caps.len() > 1 {
            // Return first capture group
            caps.get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        } else {
            // No capture groups — return full match
            caps.get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        }
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Task 5: Command execution
// ---------------------------------------------------------------------------

/// Prompt the user with a yes/no question. Defaults to No.
fn prompt_confirm(question: &str) -> Result<bool> {
    use std::io::Write;

    print!("{question} [y/N] ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Run all commands for the given phase.
///
/// - `PrePush`: bail on the first failure (after running `on_error` if set).
/// - `AfterPush`: warn on failure but continue running remaining commands.
/// - `dry_run`: print the resolved command without executing it.
/// - `force`: auto-accept all `confirm` prompts without asking.
pub fn run_phase(
    phase: HookPhase,
    config: &HooksConfig,
    template_ctx: &mut TemplateContext,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let commands = match phase {
        HookPhase::PrePush => &config.pre_push,
        HookPhase::AfterPush => &config.after_push,
    };

    if commands.is_empty() {
        return Ok(());
    }

    let label = phase.label();
    let total = commands.len();
    println!("[{label}] Running {total} command(s)...");

    for (i, cmd) in commands.iter().enumerate() {
        let step = i + 1;

        let resolved_run = render_template(&cmd.run, template_ctx, &cmd.name, &cmd.run, phase);
        println!("[{label}] [{step}/{total}] {}...", cmd.name);

        if dry_run {
            if let Some(ref confirm_tmpl) = cmd.confirm {
                let resolved =
                    render_template(confirm_tmpl, template_ctx, &cmd.name, &cmd.run, phase);
                println!("[{label}] [dry-run] Would confirm: {resolved}");
            }
            println!("[{label}] [dry-run] Would run: {resolved_run}");
            if cmd.interactive {
                println!("[{label}] [dry-run] (interactive mode)");
            }
            continue;
        }

        // Handle confirm prompt
        if let Some(ref confirm_tmpl) = cmd.confirm {
            let resolved = render_template(confirm_tmpl, template_ctx, &cmd.name, &cmd.run, phase);

            if force {
                println!("[{label}] [{step}/{total}] Confirm auto-accepted (--force): {resolved}");
            } else if !std::io::stdin().is_terminal() {
                println!("[{label}] [{step}/{total}] Confirm auto-accepted (no TTY): {resolved}");
            } else if !prompt_confirm(&resolved)? {
                match phase {
                    HookPhase::PrePush => {
                        bail!(
                            "{label} hook '{}' was not confirmed. Push aborted.",
                            cmd.name
                        );
                    }
                    HookPhase::AfterPush => {
                        println!(
                            "[{label}] [{step}/{total}] {} skipped (not confirmed)",
                            cmd.name
                        );
                        continue;
                    }
                }
            }
        }

        let (output, success) = execute_command(&resolved_run, cmd.interactive)?;

        // Store output keyed by name for chaining
        template_ctx
            .command_outputs
            .insert(cmd.name.clone(), output.clone());

        if success {
            println!("[{label}] [{step}/{total}] {} passed", cmd.name);
        } else {
            // Run on_error handler if present
            if let Some(on_error_tmpl) = &cmd.on_error {
                let resolved_on_error =
                    render_template(on_error_tmpl, template_ctx, &cmd.name, &cmd.run, phase);
                println!("[{label}] [on_error] Running: {resolved_on_error}");
                let _ = execute_command(&resolved_on_error, false);
            }

            match phase {
                HookPhase::PrePush => {
                    bail!(
                        "{label} check '{}' failed.\nCommand: {}\nPush aborted. Fix the issue and try again.",
                        cmd.name,
                        resolved_run
                    );
                }
                HookPhase::AfterPush => {
                    eprintln!(
                        "[{label}] WARNING: '{}' failed (command: {}). Continuing.",
                        cmd.name, resolved_run
                    );
                }
            }
        }
    }

    if !dry_run {
        match phase {
            HookPhase::PrePush => println!("[{label}] All checks passed"),
            HookPhase::AfterPush => println!("[{label}] All hooks completed"),
        }
    }

    Ok(())
}

/// Execute a shell command, streaming output to the terminal in real-time
/// while also capturing it for template variable chaining.
///
/// When `interactive` is true and a TTY is available, the command gets full
/// stdin/stdout/stderr passthrough (no output capture). Falls back to piped
/// mode when no TTY is detected (e.g. CI).
///
/// Returns `(combined_output, success)`.
fn execute_command(cmd: &str, interactive: bool) -> Result<(String, bool)> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::thread;

    // Interactive mode: inherit all stdio for full TTY passthrough.
    // Output is not captured (returns empty string).
    if interactive && std::io::stdin().is_terminal() {
        let status = Command::new("sh")
            .args(["-c", cmd])
            .status()
            .with_context(|| format!("failed to run: {}", cmd))?;
        return Ok((String::new(), status.success()));
    }

    if interactive {
        eprintln!("[hooks] Note: interactive mode unavailable (no TTY); capturing output instead");
    }

    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run: {}", cmd))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("stderr pipe unavailable"))?;

    let t1 = thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .inspect(|line| println!("{line}"))
            .collect::<Vec<_>>()
            .join("\n")
    });

    let t2 = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .inspect(|line| eprintln!("{line}"))
            .collect::<Vec<_>>()
            .join("\n")
    });

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for: {}", cmd))?;

    let stdout_output = t1.join().unwrap_or_default();
    let stderr_output = t2.join().unwrap_or_default();

    let mut combined = stdout_output;
    if !stderr_output.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr_output);
    }
    Ok((combined, status.success()))
}

// ---------------------------------------------------------------------------
// Task 6: init_config and show_config
// ---------------------------------------------------------------------------

pub fn init_config(repo_root: &Path) -> Result<()> {
    let path = config_path(repo_root);
    if path.exists() {
        bail!("{CONFIG_FILE} already exists at {}", path.display());
    }

    let pre_push = default_pre_push_commands(repo_root);
    let after_push = default_after_push_commands();
    let config = HooksConfig {
        pre_push,
        after_push,
    };

    let content =
        serde_json::to_string_pretty(&config).context("failed to serialize default config")?;

    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    println!("Created {CONFIG_FILE} at {}", path.display());
    println!("Edit it to customise your pre-push checks and after-push hooks.");
    Ok(())
}

fn format_command(i: usize, cmd: &HookCommand) -> String {
    let desc = cmd
        .description
        .as_deref()
        .map(|s| format!(" — {s}"))
        .unwrap_or_default();
    let on_error = cmd
        .on_error
        .as_deref()
        .map(|s| format!(" (on_error: {s})"))
        .unwrap_or_default();
    let confirm_info = cmd
        .confirm
        .as_deref()
        .map(|s| format!(" [confirm: {s}]"))
        .unwrap_or_default();
    let interactive_info = if cmd.interactive {
        " [interactive]"
    } else {
        ""
    };
    format!(
        "  {}) {}{}: {}{}{}{}",
        i + 1,
        cmd.name,
        desc,
        cmd.run,
        on_error,
        confirm_info,
        interactive_info
    )
}

pub fn show_config(repo_root: &Path) -> Result<()> {
    let config = load_config(repo_root)?;
    match config {
        Some(cfg) => {
            let path = config_path(repo_root);
            println!("Config: {}", path.display());

            if cfg.pre_push.is_empty() {
                println!("[pre_push] No commands configured.");
            } else {
                println!("[pre_push] {} command(s):", cfg.pre_push.len());
                for (i, cmd) in cfg.pre_push.iter().enumerate() {
                    println!("{}", format_command(i, cmd));
                }
            }

            if cfg.after_push.is_empty() {
                println!("[after_push] No commands configured.");
            } else {
                println!("[after_push] {} command(s):", cfg.after_push.len());
                for (i, cmd) in cfg.after_push.iter().enumerate() {
                    println!("{}", format_command(i, cmd));
                }
            }
        }
        None => {
            println!("No config found. Run with --init-hooks to create one.");
        }
    }
    Ok(())
}

pub fn default_pre_push_commands(repo_root: &Path) -> Vec<HookCommand> {
    if repo_root.join("Cargo.toml").exists() {
        vec![
            HookCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: "cargo test".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
            HookCommand {
                name: "lint".into(),
                description: Some("Check for common mistakes and style issues".into()),
                run: "cargo clippy -- -D warnings".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
            HookCommand {
                name: "format check".into(),
                description: Some("Verify code formatting matches rustfmt rules".into()),
                run: "cargo fmt -- --check".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
        ]
    } else if repo_root.join("package.json").exists() {
        vec![
            HookCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: "npm test".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
            HookCommand {
                name: "lint".into(),
                description: Some("Check for common mistakes and style issues".into()),
                run: "npm run lint".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
        ]
    } else if repo_root.join("pyproject.toml").exists() || repo_root.join("setup.py").exists() {
        vec![
            HookCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: "python -m pytest".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
            HookCommand {
                name: "lint".into(),
                description: Some("Check for common mistakes and style issues".into()),
                run: "python -m ruff check .".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
        ]
    } else if repo_root.join("go.mod").exists() {
        vec![
            HookCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: "go test ./...".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
            HookCommand {
                name: "vet".into(),
                description: Some("Check for suspicious constructs and potential bugs".into()),
                run: "go vet ./...".into(),
                on_error: None,
                confirm: None,
                interactive: false,
            },
        ]
    } else {
        vec![HookCommand {
            name: "example".into(),
            description: Some("Placeholder hook — replace with your own checks".into()),
            run: "echo 'Replace with your pre-push checks'".into(),
            on_error: None,
            confirm: None,
            interactive: false,
        }]
    }
}

fn default_after_push_commands() -> Vec<HookCommand> {
    vec![HookCommand {
        name: "example".into(),
        description: Some("Print a summary of the push".into()),
        run: "echo 'Pushed {{ branch }} ({{ commit_hash }})'".into(),
        on_error: None,
        confirm: None,
        interactive: false,
    }]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- helpers ---

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            branch: "main".into(),
            remote: "origin".into(),
            commit_hash: "abc1234".into(),
            command_outputs: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Config tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_config_path() {
        let root = Path::new("/tmp/repo");
        assert_eq!(
            config_path(root),
            PathBuf::from("/tmp/repo/.auto-push.json")
        );
    }

    #[test]
    fn test_load_config_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_config(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_config_valid() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push": [{"name": "tests", "description": "Run tests", "run": "cargo test"}],
            "after_push": [{"name": "notify", "description": "Send notification", "run": "echo done"}]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert_eq!(config.pre_push.len(), 1);
        assert_eq!(config.pre_push[0].name, "tests");
        assert_eq!(config.after_push.len(), 1);
        assert_eq!(config.after_push[0].name, "notify");
    }

    #[test]
    fn test_load_config_optional_sections() {
        // Neither pre_push nor after_push is present — should default to empty vecs
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".auto-push.json"), "{}").unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert!(config.pre_push.is_empty());
        assert!(config.after_push.is_empty());
    }

    #[test]
    fn test_load_config_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".auto-push.json"), "not json").unwrap();

        let result = load_config(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_duplicate_names_pre_push() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push": [
                {"name": "tests", "run": "cargo test"},
                {"name": "tests", "run": "cargo test --release"}
            ]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let result = load_config(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn test_load_config_same_name_across_phases_ok() {
        // Same name in pre_push and after_push is allowed
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push":   [{"name": "tests", "run": "cargo test"}],
            "after_push": [{"name": "tests", "run": "echo done"}]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let result = load_config(dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_load_config_on_error_field() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push": [
                {"name": "tests", "run": "cargo test", "on_error": "echo failed"}
            ]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert_eq!(config.pre_push[0].on_error.as_deref(), Some("echo failed"));
    }

    #[test]
    fn test_hook_phase_label() {
        assert_eq!(HookPhase::PrePush.label(), "pre_push");
        assert_eq!(HookPhase::AfterPush.label(), "after_push");
    }

    // -----------------------------------------------------------------------
    // Sanitization tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_sanitize_basic_text() {
        assert_eq!(sanitize_template_value("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_trims_whitespace() {
        assert_eq!(sanitize_template_value("  hello  "), "hello");
    }

    #[test]
    fn test_sanitize_escapes_shell_chars() {
        let result = sanitize_template_value("$HOME");
        assert!(result.contains("\\$"), "dollar sign should be escaped");

        let result2 = sanitize_template_value("foo`bar`");
        assert!(result2.contains("\\`"), "backtick should be escaped");

        let result3 = sanitize_template_value("say 'hi'");
        assert!(result3.contains("\\'"), "single quote should be escaped");
    }

    #[test]
    fn test_sanitize_newlines() {
        let result = sanitize_template_value("line1\nline2");
        assert_eq!(result, "line1\\nline2");
    }

    #[test]
    fn test_sanitize_truncates_long_output() {
        let long_string = "a".repeat(5000);
        let result = sanitize_template_value(&long_string);
        assert!(
            result.ends_with("...(truncated)"),
            "should end with truncation marker"
        );
        assert!(
            result.len() > 4096,
            "total length should exceed 4096 due to appended marker"
        );
    }

    // -----------------------------------------------------------------------
    // Template tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_render_template_git_vars() {
        let ctx = make_ctx();
        let out = render_template(
            "push {{ branch }} to {{ remote }} at {{ commit_hash }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::PrePush,
        );
        assert_eq!(out, "push main to origin at abc1234");
    }

    #[test]
    fn test_render_template_command_context_vars() {
        let ctx = make_ctx();
        let out = render_template(
            "running {{ command_name }} ({{ command_run }}) in {{ command_type }}",
            &ctx,
            "my-cmd",
            "echo hi",
            HookPhase::AfterPush,
        );
        assert_eq!(out, "running my-cmd (echo hi) in after_push");
    }

    #[test]
    fn test_render_template_command_output() {
        let mut ctx = make_ctx();
        ctx.command_outputs
            .insert("build".into(), "Build succeeded\n".into());

        let out = render_template(
            "result: {{ command_output.build }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::AfterPush,
        );
        assert!(out.contains("Build succeeded"));
    }

    #[test]
    fn test_render_template_command_output_missing() {
        let ctx = make_ctx();
        let out = render_template(
            "result: {{ command_output.nonexistent }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::AfterPush,
        );
        // Missing output → empty string substituted
        assert_eq!(out, "result: ");
    }

    #[test]
    fn test_render_template_command_output_with_regex() {
        let mut ctx = make_ctx();
        ctx.command_outputs
            .insert("version".into(), "v1.2.3-rc1".into());

        let out = render_template(
            "ver={{ command_output.version | /v(\\d+\\.\\d+\\.\\d+)/ }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::AfterPush,
        );
        assert_eq!(out, "ver=1.2.3");
    }

    #[test]
    fn test_render_template_regex_no_capture_group() {
        let mut ctx = make_ctx();
        ctx.command_outputs.insert("step".into(), "DONE".into());

        // Pattern with no capture group — should return the full match
        let out = render_template(
            "{{ command_output.step | /DONE/ }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::AfterPush,
        );
        assert_eq!(out, "DONE");
    }

    #[test]
    fn test_render_template_regex_no_match() {
        let mut ctx = make_ctx();
        ctx.command_outputs
            .insert("step".into(), "nothing here".into());

        let out = render_template(
            "{{ command_output.step | /^v\\d+/ }}",
            &ctx,
            "cmd",
            "run",
            HookPhase::AfterPush,
        );
        assert_eq!(out, "");
    }

    #[test]
    fn test_render_template_preserves_non_template_text() {
        let ctx = make_ctx();
        let out = render_template(
            "plain text with no templates",
            &ctx,
            "cmd",
            "run",
            HookPhase::PrePush,
        );
        assert_eq!(out, "plain text with no templates");
    }

    // -----------------------------------------------------------------------
    // Execution tests
    // -----------------------------------------------------------------------

    fn cmd(name: &str, run: &str) -> HookCommand {
        HookCommand {
            name: name.into(),
            description: None,
            run: run.into(),
            on_error: None,
            confirm: None,
            interactive: false,
        }
    }

    fn simple_config(commands: Vec<HookCommand>) -> HooksConfig {
        HooksConfig {
            pre_push: commands,
            after_push: vec![],
        }
    }

    fn after_config(commands: Vec<HookCommand>) -> HooksConfig {
        HooksConfig {
            pre_push: vec![],
            after_push: commands,
        }
    }

    #[test]
    fn test_run_phase_pre_push_success() {
        let config = simple_config(vec![cmd("trivial", "true")]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_phase_pre_push_failure_bails() {
        let config = simple_config(vec![cmd("failing", "false")]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failing"));
        assert!(msg.contains("Push aborted"));
    }

    #[test]
    fn test_run_phase_after_push_failure_warns_continues() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let config = after_config(vec![
            cmd("fail-first", "false"),
            cmd("create-marker", &format!("touch {}", marker.display())),
        ]);
        let mut ctx = make_ctx();
        // Should NOT bail — after_push continues on failure
        let result = run_phase(HookPhase::AfterPush, &config, &mut ctx, false, false);
        assert!(result.is_ok(), "after_push should not bail on failure");
        assert!(marker.exists(), "second command should still have run");
    }

    #[test]
    fn test_run_phase_captures_output() {
        let config = simple_config(vec![cmd("greet", "echo hello")]);
        let mut ctx = make_ctx();
        run_phase(HookPhase::PrePush, &config, &mut ctx, false, false).unwrap();
        let captured = ctx.command_outputs.get("greet").unwrap();
        assert!(captured.contains("hello"));
    }

    #[test]
    fn test_run_phase_template_substitution() {
        let config = simple_config(vec![cmd("branch-check", "echo {{ branch }}")]);
        let mut ctx = make_ctx();
        run_phase(HookPhase::PrePush, &config, &mut ctx, false, false).unwrap();
        let captured = ctx.command_outputs.get("branch-check").unwrap();
        assert!(captured.contains("main"));
    }

    #[test]
    fn test_run_phase_output_chaining() {
        let config = HooksConfig {
            pre_push: vec![],
            after_push: vec![
                cmd("step1", "echo chain_value"),
                cmd("step2", "echo {{ command_output.step1 }}"),
            ],
        };
        let mut ctx = make_ctx();
        run_phase(HookPhase::AfterPush, &config, &mut ctx, false, false).unwrap();
        let step2_out = ctx.command_outputs.get("step2").unwrap();
        assert!(
            step2_out.contains("chain_value"),
            "step2 should have received step1's output"
        );
    }

    #[test]
    fn test_run_phase_on_error_runs() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("on_error_ran");
        let config = simple_config(vec![HookCommand {
            on_error: Some(format!("touch {}", marker.display())),
            ..cmd("fail", "false")
        }]);
        let mut ctx = make_ctx();
        let _ = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(marker.exists(), "on_error command should have run");
    }

    #[test]
    fn test_run_phase_dry_run_skips_execution() {
        let config = simple_config(vec![cmd("would-fail", "false")]);
        let mut ctx = make_ctx();
        // Should succeed because dry_run never executes
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_phase_empty_commands() {
        let config = HooksConfig {
            pre_push: vec![],
            after_push: vec![],
        };
        let mut ctx = make_ctx();
        assert!(run_phase(HookPhase::PrePush, &config, &mut ctx, false, false).is_ok());
        assert!(run_phase(HookPhase::AfterPush, &config, &mut ctx, false, false).is_ok());
    }

    #[test]
    fn test_run_phase_pre_push_stops_on_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("should_not_exist");
        let config = simple_config(vec![
            cmd("fail-first", "false"),
            cmd("should-not-run", &format!("touch {}", marker.display())),
        ]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(result.is_err());
        assert!(!marker.exists(), "second command should not have run");
    }

    // -----------------------------------------------------------------------
    // Confirm / interactive tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_confirm_field_deserializes() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push": [{
                "name": "deploy",
                "run": "deploy.sh",
                "confirm": "Deploy to production?"
            }]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert_eq!(
            config.pre_push[0].confirm.as_deref(),
            Some("Deploy to production?")
        );
        assert!(!config.pre_push[0].interactive);
    }

    #[test]
    fn test_interactive_field_deserializes() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push": [{
                "name": "login",
                "run": "auth-tool",
                "interactive": true
            }]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert!(config.pre_push[0].interactive);
        assert!(config.pre_push[0].confirm.is_none());
    }

    #[test]
    fn test_interactive_defaults_to_false() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"pre_push": [{"name": "t", "run": "true"}]}"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert!(!config.pre_push[0].interactive);
    }

    #[test]
    fn test_confirm_force_auto_accepts() {
        // With force=true, confirm is auto-accepted and the command runs
        let config = simple_config(vec![HookCommand {
            confirm: Some("Are you sure?".into()),
            ..cmd("guarded", "true")
        }]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, true);
        assert!(result.is_ok());
        assert!(ctx.command_outputs.contains_key("guarded"));
    }

    #[test]
    fn test_confirm_no_tty_auto_accepts() {
        // In test runner (no TTY), confirm is auto-accepted
        let config = simple_config(vec![HookCommand {
            confirm: Some("Continue?".into()),
            ..cmd("gated", "echo ran")
        }]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(result.is_ok());
        let output = ctx.command_outputs.get("gated").unwrap();
        assert!(output.contains("ran"));
    }

    #[test]
    fn test_interactive_command_falls_back_in_no_tty() {
        // In test runner (no TTY), interactive falls back to piped mode
        let config = simple_config(vec![HookCommand {
            interactive: true,
            ..cmd("interactive-echo", "echo interactive_output")
        }]);
        let mut ctx = make_ctx();
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, false, false);
        assert!(result.is_ok());
        // In piped fallback mode, output IS captured
        let output = ctx.command_outputs.get("interactive-echo").unwrap();
        assert!(output.contains("interactive_output"));
    }

    #[test]
    fn test_dry_run_shows_confirm_and_interactive() {
        let config = simple_config(vec![HookCommand {
            confirm: Some("Deploy?".into()),
            interactive: true,
            ..cmd("deploy", "deploy.sh")
        }]);
        let mut ctx = make_ctx();
        // dry_run should succeed without executing or prompting
        let result = run_phase(HookPhase::PrePush, &config, &mut ctx, true, false);
        assert!(result.is_ok());
        // No output captured in dry-run
        assert!(!ctx.command_outputs.contains_key("deploy"));
    }

    #[test]
    fn test_confirm_serialization_skips_defaults() {
        let hook = cmd("test", "true");
        let json = serde_json::to_string(&hook).unwrap();
        assert!(!json.contains("confirm"), "confirm: None should be omitted");
        assert!(
            !json.contains("interactive"),
            "interactive: false should be omitted"
        );
    }

    #[test]
    fn test_confirm_serialization_includes_values() {
        let hook = HookCommand {
            confirm: Some("Sure?".into()),
            interactive: true,
            ..cmd("test", "true")
        };
        let json = serde_json::to_string(&hook).unwrap();
        assert!(json.contains("\"confirm\":\"Sure?\""));
        assert!(json.contains("\"interactive\":true"));
    }

    // -----------------------------------------------------------------------
    // Init / show tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_config_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();

        let path = dir.path().join(".auto-push.json");
        assert!(path.exists());

        let content = fs::read_to_string(&path).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert!(!config.pre_push.is_empty());
    }

    #[test]
    fn test_init_config_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".auto-push.json"), "{}").unwrap();

        let result = init_config(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_init_config_detects_rust() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.pre_push.len(), 3);
        assert!(config.pre_push.iter().any(|c| c.run.contains("cargo test")));
        assert!(
            config
                .pre_push
                .iter()
                .any(|c| c.run.contains("cargo clippy"))
        );
        assert!(config.pre_push.iter().any(|c| c.run.contains("cargo fmt")));
    }

    #[test]
    fn test_init_config_detects_node() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.pre_push.len(), 2);
        assert!(config.pre_push.iter().any(|c| c.run.contains("npm test")));
        assert!(
            config
                .pre_push
                .iter()
                .any(|c| c.run.contains("npm run lint"))
        );
    }

    #[test]
    fn test_init_config_detects_python() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert!(config.pre_push.iter().any(|c| c.run.contains("pytest")));
    }

    #[test]
    fn test_init_config_detects_go() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module test").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert!(config.pre_push.iter().any(|c| c.run.contains("go test")));
    }

    #[test]
    fn test_init_config_generic_fallback() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.pre_push.len(), 1);
        assert_eq!(config.pre_push[0].name, "example");
    }

    #[test]
    fn test_init_config_always_has_after_push_example() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".auto-push.json")).unwrap();
        let config: HooksConfig = serde_json::from_str(&content).unwrap();
        assert!(
            !config.after_push.is_empty(),
            "after_push should always have at least one example"
        );
    }

    #[test]
    fn test_show_config_no_file() {
        let dir = tempfile::tempdir().unwrap();
        // Should succeed and print "No config found"
        show_config(dir.path()).unwrap();
    }

    #[test]
    fn test_show_config_with_commands() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pre_push":   [{"name": "tests", "run": "cargo test"}],
            "after_push": [{"name": "notify", "run": "echo done"}]
        }"#;
        fs::write(dir.path().join(".auto-push.json"), json).unwrap();
        show_config(dir.path()).unwrap();
    }
}
