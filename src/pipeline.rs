use crate::config::{self, CaptureMode, PipelineCommand};
use crate::template;
use crate::vars;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::io::IsTerminal;

// ---------------------------------------------------------------------------
// Public types (execution context, not config)
// ---------------------------------------------------------------------------

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
    pub commit_summary: String,
    pub command_outputs: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Template rendering for hook commands (shell-escaped)
// ---------------------------------------------------------------------------

fn build_hook_vars(
    ctx: &TemplateContext,
    command_name: &str,
    command_run: &str,
    phase: HookPhase,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("branch".into(), ctx.branch.clone());
    vars.insert("remote".into(), ctx.remote.clone());
    vars.insert("commit_hash".into(), ctx.commit_hash.clone());
    vars.insert("commit_summary".into(), ctx.commit_summary.clone());
    vars.insert("command_name".into(), command_name.to_string());
    vars.insert("command_run".into(), command_run.to_string());
    vars.insert("command_type".into(), phase.label().to_string());

    // Add command outputs with prefix for compatibility
    for (name, output) in &ctx.command_outputs {
        vars.insert(format!("command_output.{name}"), output.clone());
    }

    vars
}

fn render_hook_template(
    tmpl: &str,
    ctx: &TemplateContext,
    command_name: &str,
    command_run: &str,
    phase: HookPhase,
) -> String {
    let vars = build_hook_vars(ctx, command_name, command_run, phase);
    // Hook commands run in a shell, so values must be shell-escaped
    template::render_shell(tmpl, &vars)
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

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
    commands: &[PipelineCommand],
    template_ctx: &mut TemplateContext,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    let label = phase.label();
    let total = commands.len();
    println!("[{label}] Running {total} command(s)...");

    for (i, cmd) in commands.iter().enumerate() {
        let step = i + 1;
        let desc = config::auto_description(cmd);
        let run_str = cmd.run.as_deref().unwrap_or("");

        let resolved_run = render_hook_template(run_str, template_ctx, &cmd.name, run_str, phase);
        println!("[{label}] [{step}/{total}] {desc}...");

        if dry_run {
            if let Some(ref confirm_tmpl) = cmd.confirm {
                let resolved =
                    render_hook_template(confirm_tmpl, template_ctx, &cmd.name, run_str, phase);
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
            let resolved =
                render_hook_template(confirm_tmpl, template_ctx, &cmd.name, run_str, phase);

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
                    render_hook_template(on_error_tmpl, template_ctx, &cmd.name, run_str, phase);
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

    if interactive && std::io::stdin().is_terminal() {
        let status = Command::new("sh")
            .args(["-c", cmd])
            .status()
            .with_context(|| format!("failed to run: {cmd}"))?;
        return Ok((String::new(), status.success()));
    }

    if interactive {
        eprintln!(
            "[pipeline] Note: interactive mode unavailable (no TTY); capturing output instead"
        );
    }

    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run: {cmd}"))?;

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
        .with_context(|| format!("failed to wait for: {cmd}"))?;

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
// Split-capture execution (stdout only)
// ---------------------------------------------------------------------------

/// Execute a shell command with separate stdout and stderr pipes.
/// stdout is captured and returned. stderr is streamed to terminal only.
fn execute_command_split(cmd: &str, interactive: bool) -> Result<(String, bool)> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::thread;

    if interactive && std::io::stdin().is_terminal() {
        let status = Command::new("sh")
            .args(["-c", cmd])
            .status()
            .with_context(|| format!("failed to run: {cmd}"))?;
        return Ok((String::new(), status.success()));
    }

    if interactive {
        eprintln!(
            "[pipeline] Note: interactive mode unavailable (no TTY); capturing output instead"
        );
    }

    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run: {cmd}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("stderr pipe unavailable"))?;

    // stdout: stream to terminal AND capture
    let t_stdout = thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .inspect(|line| println!("{line}"))
            .collect::<Vec<_>>()
            .join("\n")
    });

    // stderr: stream to terminal only (not captured)
    let t_stderr = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .for_each(|line| eprintln!("{line}"));
    });

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for: {cmd}"))?;

    let stdout_output = t_stdout.join().unwrap_or_default();
    let _ = t_stderr.join();

    Ok((stdout_output, status.success()))
}

/// Execute a command with args directly (no shell), separate stdout/stderr.
/// stdout is captured and returned. stderr is streamed to terminal only.
fn execute_argv(command: &str, args: &[String], interactive: bool) -> Result<(String, bool)> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::thread;

    if interactive && std::io::stdin().is_terminal() {
        let status = Command::new(command)
            .args(args)
            .status()
            .with_context(|| format!("failed to run: {command}"))?;
        return Ok((String::new(), status.success()));
    }

    if interactive {
        eprintln!(
            "[pipeline] Note: interactive mode unavailable (no TTY); capturing output instead"
        );
    }

    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run: {command}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("stderr pipe unavailable"))?;

    let t_stdout = thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .inspect(|line| println!("{line}"))
            .collect::<Vec<_>>()
            .join("\n")
    });

    let t_stderr = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .for_each(|line| eprintln!("{line}"));
    });

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for: {command}"))?;

    let stdout_output = t_stdout.join().unwrap_or_default();
    let _ = t_stderr.join();

    Ok((stdout_output, status.success()))
}

// ---------------------------------------------------------------------------
// Unified pipeline engine
// ---------------------------------------------------------------------------

/// Run a pipeline of commands with full capture, skip, dry-run, and confirm support.
///
/// This is the unified replacement for `run_phase()`. It uses `HashMap<String, String>`
/// for template variables (instead of the old `TemplateContext`).
///
/// - `lazy_resolver`: resolver for dynamic built-in vars (diff, staged_files, etc.)
/// - `skip_names`: command names to skip (from `--skip`)
/// - `dry_run`: print resolved commands without executing
/// - `force`: auto-accept all confirm prompts
/// - `confirm_all`: prompt before each command (from `--confirm`)
pub fn run_pipeline(
    commands: &[PipelineCommand],
    template_vars: &mut HashMap<String, String>,
    lazy_resolver: &mut vars::LazyVarResolver,
    skip_names: &[String],
    dry_run: bool,
    force: bool,
    confirm_all: bool,
) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    let total = commands.len();
    println!("[pipeline] Running {total} command(s)...");

    for (i, cmd) in commands.iter().enumerate() {
        let step = i + 1;
        let desc = config::auto_description(cmd);

        // 1. Skip check: --skip flag
        if skip_names.iter().any(|s| s == &cmd.name) {
            println!("[pipeline] [{step}/{total}] {desc} skipped");
            continue;
        }

        // 2. -m auto-skip: if template_vars already has the capture target, skip
        if let Some(ref capture_name) = cmd.capture
            && template_vars.contains_key(capture_name)
        {
            println!("[pipeline] [{step}/{total}] {desc} skipped ('{capture_name}' already set)");
            continue;
        }

        // Populate dynamic vars on demand before template resolution
        for dynamic_name in vars::LazyVarResolver::dynamic_names() {
            if let Some(val) = lazy_resolver.get(dynamic_name) {
                template_vars.insert(dynamic_name.to_string(), val);
            }
        }

        // 3. Template resolution
        let resolved_display = resolve_command_display(cmd, template_vars);

        // 4. Dry-run: print and continue
        if dry_run {
            if let Some(ref confirm_text) = cmd.confirm {
                let resolved_confirm = template::render_shell(confirm_text, template_vars);
                println!("[pipeline] [dry-run] Would confirm: {resolved_confirm}");
            }
            println!("[pipeline] [dry-run] Would run: {resolved_display}");
            if cmd.interactive {
                println!("[pipeline] [dry-run] (interactive mode)");
            }
            continue;
        }

        // 5. Confirm prompt
        if !handle_confirm(
            cmd,
            template_vars,
            force,
            confirm_all,
            &resolved_display,
            step,
            total,
        )? {
            println!("[pipeline] [{step}/{total}] {desc} skipped (not confirmed)");
            continue;
        }

        println!("[pipeline] [{step}/{total}] {desc}...");

        // 6. Execute
        let (output, success) = execute_pipeline_command(cmd, template_vars)?;

        // After execution: invalidate lazy cache if the command mutates git state
        if let Some(ref run_str) = cmd.run
            && vars::is_git_mutating(run_str)
        {
            lazy_resolver.invalidate();
        }

        if success {
            // 7. Capture: store trimmed stdout into template_vars
            if let Some(ref capture_name) = cmd.capture {
                template_vars.insert(capture_name.clone(), output.trim().to_string());
            }

            // 8. Capture_after: execute each entry, store trimmed stdout
            if let Some(ref entries) = cmd.capture_after {
                for entry in entries {
                    let resolved_cap_cmd = template::render_shell(&entry.run, template_vars);
                    let (cap_output, _) = execute_command_split(&resolved_cap_cmd, false)?;
                    template_vars.insert(entry.name.clone(), cap_output.trim().to_string());
                }
            }

            println!("[pipeline] [{step}/{total}] {} passed", cmd.name);
        } else {
            // 9. On failure: run on_error handler, then bail
            if let Some(ref on_error_tmpl) = cmd.on_error {
                let resolved_on_error = template::render_shell(on_error_tmpl, template_vars);
                println!("[pipeline] [on_error] Running: {resolved_on_error}");
                let _ = execute_command_split(&resolved_on_error, false);
            }

            bail!(
                "pipeline command '{}' failed.\nCommand: {}\nPipeline aborted.",
                cmd.name,
                resolved_display
            );
        }
    }

    if !dry_run {
        println!("[pipeline] All commands passed");
    }

    Ok(())
}

/// Resolve a pipeline command to its display string for logging.
fn resolve_command_display(cmd: &PipelineCommand, vars: &HashMap<String, String>) -> String {
    if let Some(ref run_str) = cmd.run {
        template::render_shell(run_str, vars)
    } else if let Some(ref command) = cmd.command {
        let resolved_args: Vec<String> = cmd
            .args
            .as_ref()
            .map(|args| args.iter().map(|a| template::render_raw(a, vars)).collect())
            .unwrap_or_default();
        if resolved_args.is_empty() {
            command.clone()
        } else {
            format!("{command} {}", resolved_args.join(" "))
        }
    } else {
        "(no command)".to_string()
    }
}

/// Handle confirm prompt logic. Returns true if execution should proceed.
fn handle_confirm(
    cmd: &PipelineCommand,
    vars: &HashMap<String, String>,
    force: bool,
    confirm_all: bool,
    resolved_display: &str,
    step: usize,
    total: usize,
) -> Result<bool> {
    // Determine confirm text: explicit field takes priority, then --confirm flag
    let confirm_text = if let Some(ref text) = cmd.confirm {
        Some(template::render_shell(text, vars))
    } else if confirm_all {
        Some(format!("Run '{resolved_display}'?"))
    } else {
        None
    };

    let Some(prompt_text) = confirm_text else {
        return Ok(true);
    };

    if force {
        println!("[pipeline] [{step}/{total}] Confirm auto-accepted (--force): {prompt_text}");
        return Ok(true);
    }

    if !std::io::stdin().is_terminal() {
        println!("[pipeline] [{step}/{total}] Confirm auto-accepted (no TTY): {prompt_text}");
        return Ok(true);
    }

    prompt_confirm(&prompt_text)
}

/// Execute a single pipeline command using the appropriate method.
fn execute_pipeline_command(
    cmd: &PipelineCommand,
    vars: &HashMap<String, String>,
) -> Result<(String, bool)> {
    // Argv mode: command + args (no shell)
    if let Some(ref command) = cmd.command {
        let resolved_args: Vec<String> = cmd
            .args
            .as_ref()
            .map(|args| args.iter().map(|a| template::render_raw(a, vars)).collect())
            .unwrap_or_default();
        return execute_argv(command, &resolved_args, cmd.interactive);
    }

    // Shell mode: run string
    let run_str = cmd.run.as_deref().unwrap_or("");
    let resolved_run = template::render_shell(run_str, vars);

    // Choose capture function based on capture_mode
    let use_both = matches!(cmd.capture_mode, Some(CaptureMode::Both));

    if use_both {
        execute_command(&resolved_run, cmd.interactive)
    } else {
        execute_command_split(&resolved_run, cmd.interactive)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            branch: "main".into(),
            remote: "origin".into(),
            commit_hash: "abc1234".into(),
            commit_summary: "feat: add new feature".into(),
            command_outputs: HashMap::new(),
        }
    }

    #[test]
    fn test_render_hook_template_basic() {
        let ctx = make_ctx();
        let result = render_hook_template(
            "echo {{ branch }}",
            &ctx,
            "test",
            "echo",
            HookPhase::PrePush,
        );
        assert!(result.contains("main"));
    }

    #[test]
    fn test_render_hook_template_commit_summary() {
        let ctx = make_ctx();
        let result = render_hook_template(
            "echo {{ commit_summary }}",
            &ctx,
            "test",
            "echo",
            HookPhase::AfterPush,
        );
        // Should be shell-escaped
        assert!(result.contains("feat"));
    }

    #[test]
    fn test_render_hook_template_command_output_chaining() {
        let mut ctx = make_ctx();
        ctx.command_outputs
            .insert("prev".into(), "some output".into());
        let result = render_hook_template(
            "echo {{ command_output.prev }}",
            &ctx,
            "test",
            "echo",
            HookPhase::AfterPush,
        );
        assert!(result.contains("some output"));
    }

    #[test]
    fn test_hook_phase_label() {
        assert_eq!(HookPhase::PrePush.label(), "pre_push");
        assert_eq!(HookPhase::AfterPush.label(), "after_push");
    }

    // -----------------------------------------------------------------------
    // execute_command_split tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_command_split_stdout_only() {
        let (output, success) = execute_command_split("echo hello && echo err >&2", false).unwrap();
        assert!(success);
        assert_eq!(output.trim(), "hello");
        assert!(!output.contains("err"));
    }

    #[test]
    fn test_execute_command_split_failure() {
        let (_, success) = execute_command_split("exit 1", false).unwrap();
        assert!(!success);
    }

    // -----------------------------------------------------------------------
    // execute_argv tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_argv_basic() {
        let (output, success) = execute_argv("echo", &["hello world".to_string()], false).unwrap();
        assert!(success);
        assert!(output.contains("hello world"));
    }

    #[test]
    fn test_execute_argv_failure() {
        let (_, success) = execute_argv("false", &[], false).unwrap();
        assert!(!success);
    }

    // -----------------------------------------------------------------------
    // run_pipeline tests
    // -----------------------------------------------------------------------

    fn make_lazy() -> vars::LazyVarResolver {
        vars::LazyVarResolver::new(20_000)
    }

    #[test]
    fn test_run_pipeline_basic() {
        let commands = vec![PipelineCommand {
            name: "greet".into(),
            run: Some("echo hello".into()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        vars.insert("branch".into(), "main".into());
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_pipeline_capture_chains() {
        let commands = vec![
            PipelineCommand {
                name: "gen".into(),
                run: Some("echo world".into()),
                capture: Some("msg".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "use".into(),
                run: Some("echo {{ msg }}".into()),
                ..Default::default()
            },
        ];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
        assert_eq!(vars.get("msg").unwrap(), "world");
    }

    #[test]
    fn test_run_pipeline_skip() {
        let commands = vec![
            PipelineCommand {
                name: "skip_me".into(),
                run: Some("exit 1".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "keep".into(),
                run: Some("echo ok".into()),
                ..Default::default()
            },
        ];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(
            &commands,
            &mut vars,
            &mut lazy,
            &["skip_me".to_string()],
            false,
            false,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_pipeline_m_flag_skips_generate() {
        let commands = vec![
            PipelineCommand {
                name: "gen".into(),
                run: Some("exit 1".into()),
                capture: Some("commit_message".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "commit".into(),
                run: Some("echo ok".into()),
                ..Default::default()
            },
        ];
        let mut vars = HashMap::new();
        vars.insert("commit_message".to_string(), "manual msg".to_string());
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_pipeline_dry_run() {
        let commands = vec![PipelineCommand {
            name: "fail".into(),
            run: Some("exit 1".into()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], true, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_pipeline_capture_after() {
        let commands = vec![PipelineCommand {
            name: "multi".into(),
            run: Some("echo ok".into()),
            capture_after: Some(vec![
                crate::config::CaptureAfterEntry {
                    name: "var_a".into(),
                    run: "echo alpha".into(),
                },
                crate::config::CaptureAfterEntry {
                    name: "var_b".into(),
                    run: "echo beta".into(),
                },
            ]),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
        assert_eq!(vars.get("var_a").unwrap(), "alpha");
        assert_eq!(vars.get("var_b").unwrap(), "beta");
    }

    #[test]
    fn test_run_pipeline_on_error() {
        let commands = vec![PipelineCommand {
            name: "fail".into(),
            run: Some("exit 1".into()),
            on_error: Some("echo recovered".into()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        // Should still fail (on_error runs but pipeline still bails)
        assert!(result.is_err());
    }

    #[test]
    fn test_run_pipeline_argv_mode() {
        let commands = vec![PipelineCommand {
            name: "argv_echo".into(),
            command: Some("echo".into()),
            args: Some(vec!["hello".into(), "argv".into()]),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_pipeline_argv_with_capture() {
        let commands = vec![PipelineCommand {
            name: "cap_argv".into(),
            command: Some("echo".into()),
            args: Some(vec!["captured_value".into()]),
            capture: Some("my_var".into()),
            ..Default::default()
        }];
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&commands, &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
        assert_eq!(vars.get("my_var").unwrap(), "captured_value");
    }

    #[test]
    fn test_run_pipeline_empty() {
        let mut vars = HashMap::new();
        let mut lazy = make_lazy();
        let result = run_pipeline(&[], &mut vars, &mut lazy, &[], false, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_command_display_shell() {
        let cmd = PipelineCommand {
            name: "test".into(),
            run: Some("echo {{ branch }}".into()),
            ..Default::default()
        };
        let mut vars = HashMap::new();
        vars.insert("branch".into(), "main".into());
        let display = resolve_command_display(&cmd, &vars);
        assert!(display.contains("main"));
    }

    #[test]
    fn test_resolve_command_display_argv() {
        let cmd = PipelineCommand {
            name: "test".into(),
            command: Some("git".into()),
            args: Some(vec!["push".into(), "{{ remote }}".into()]),
            ..Default::default()
        };
        let mut vars = HashMap::new();
        vars.insert("remote".into(), "origin".into());
        let display = resolve_command_display(&cmd, &vars);
        assert_eq!(display, "git push origin");
    }

    #[test]
    fn test_resolve_command_display_no_command() {
        let cmd = PipelineCommand {
            name: "empty".into(),
            ..Default::default()
        };
        let display = resolve_command_display(&cmd, &HashMap::new());
        assert_eq!(display, "(no command)");
    }
}
