use crate::config::{self, HookCommand};
use crate::template;
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
    commands: &[HookCommand],
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

        let resolved_run = render_hook_template(&cmd.run, template_ctx, &cmd.name, &cmd.run, phase);
        println!("[{label}] [{step}/{total}] {desc}...");

        if dry_run {
            if let Some(ref confirm_tmpl) = cmd.confirm {
                let resolved =
                    render_hook_template(confirm_tmpl, template_ctx, &cmd.name, &cmd.run, phase);
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
                render_hook_template(confirm_tmpl, template_ctx, &cmd.name, &cmd.run, phase);

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
                    render_hook_template(on_error_tmpl, template_ctx, &cmd.name, &cmd.run, phase);
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
        eprintln!("[hooks] Note: interactive mode unavailable (no TTY); capturing output instead");
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
}
