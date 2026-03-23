use crate::config::{self, GenerateConfig};
use crate::template;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct CommitGroup {
    pub message: String,
    pub hunks: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Built-in system prompts (used when no custom prompts are configured)
// ---------------------------------------------------------------------------

const PLAN_COMMITS_SYSTEM_PROMPT_BASE: &str = r#"You are a git commit planner. Given numbered diff hunks, group them into logical commits.

Rules:
- Group related changes together (e.g. a feature + its tests, a refactor across related files)
- Unrelated changes MUST be in separate commits (e.g. a bug fix and a new feature)
- Each hunk ID must appear in EXACTLY ONE commit group
- If ALL changes are related, use a single commit group
- Output ONLY valid JSON, no markdown fences, no explanation

Output format (JSON array):
[{"message":"<commit message>","hunks":[1,2,3]},{"message":"<commit message>","hunks":[4,5]}]"#;

const PUSH_FIX_SYSTEM_PROMPT: &str = r#"You are a git expert assistant. The user's `git push` failed.
Output ONLY shell commands to fix and complete the push. Each line must be a single executable shell command.
STRICT rules:
- NO explanations, NO prose, NO markdown, NO code fences, NO backticks
- Every line of your response will be passed directly to `sh -c` — write accordingly
- Use only standard git commands (no gh, no hub)
- Do not force-push unless the error clearly requires it
- Maximum 5 lines
- If the error is unrecoverable (no network, bad credentials, repo does not exist), output exactly one line: UNRECOVERABLE: <reason>"#;

const SIMPLE_SYSTEM_PROMPT_BASE: &str = r#"You are a git commit message generator. Given a git diff, generate a concise, conventional commit message.

Rules:
- Output ONLY the commit message, nothing else"#;

const DETAILED_SYSTEM_PROMPT_BASE: &str = r#"You are a git commit message generator. Given a git diff that includes a merge, generate a conventional commit message.

Rules:
- Keep the first line concise
- Add a blank line then a body explaining what was merged and any conflicts resolved
- Body should be 2-5 lines max
- Output ONLY the commit message, nothing else"#;

const CONFLICT_RESOLVE_PROMPT: &str = r#"You are a git merge conflict resolver. The following files have merge conflicts (marked with <<<<<<<, =======, >>>>>>>).

Your task:
1. Read each conflicted file
2. Understand both sides of each conflict
3. Resolve each conflict by keeping the correct combination of changes
4. Write the resolved content back to each file (remove ALL conflict markers)
5. After resolving, run: git add <file> for each resolved file

Rules:
- Preserve ALL intentional changes from both sides when possible
- If changes are incompatible, prefer the incoming (remote) changes but keep local additions that don't conflict
- Remove ALL conflict markers (<<<<<<< ======= >>>>>>>) from every file
- Do NOT leave any conflict markers in any file
- After editing each file, stage it with git add"#;

// ---------------------------------------------------------------------------
// Provider invocation
// ---------------------------------------------------------------------------

fn call_provider(prompt: &str, system_prompt: &str, gen_config: &GenerateConfig) -> Result<String> {
    let resolved = config::resolve_provider(gen_config)?;

    // Build template variables
    let mut vars = HashMap::new();

    // For providers without a system_prompt slot, prepend it to the prompt
    let has_system_slot = resolved
        .args
        .iter()
        .any(|a| a.contains("{{ system_prompt }}"));

    let effective_prompt = if has_system_slot {
        vars.insert("system_prompt".to_string(), system_prompt.to_string());
        prompt.to_string()
    } else {
        format!("{system_prompt}\n\n{prompt}")
    };

    vars.insert("prompt".to_string(), effective_prompt);

    if let Some(ref model) = resolved.model {
        vars.insert("model".to_string(), model.clone());
    }

    // Render args using raw template (no shell escaping)
    let rendered_args: Vec<String> = resolved
        .args
        .iter()
        .map(|arg| template::render_raw(arg, &vars))
        .collect();

    let child = Command::new(&resolved.command)
        .args(&rendered_args)
        .env_remove("CLAUDECODE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "Provider '{}' not found in PATH. Install it or change provider in .auto-push.json.",
                resolved.command
            )
        })?;

    let output = wait_with_timeout(child, gen_config.timeout_secs, &resolved.command)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Provider '{}' failed (exit {}): {}",
            resolved.command,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        );
    }

    let message = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if message.is_empty() {
        bail!("Provider '{}' returned empty output", resolved.command);
    }

    Ok(message)
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout_secs: u64,
    command_name: &str,
) -> Result<std::process::Output> {
    if timeout_secs == 0 {
        return child
            .wait_with_output()
            .context("failed to wait for provider");
    }

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    bail!("Provider '{command_name}' timed out after {timeout_secs} seconds.");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e).context("failed to check provider status"),
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt resolution with style suffix
// ---------------------------------------------------------------------------

fn resolve_system_prompt(
    base: &str,
    custom: Option<&str>,
    gen_config: &GenerateConfig,
    inject_style: bool,
) -> String {
    let prompt = custom.unwrap_or(base).to_string();
    if inject_style {
        format!(
            "{}{}",
            prompt,
            config::style_suffix(&gen_config.commit_style)
        )
    } else {
        prompt
    }
}

/// Build a system prompt from the generate config for use as a template variable.
///
/// When `detailed` is false, returns the simple commit message prompt (with style suffix).
/// When `detailed` is true, returns the detailed/merge commit prompt (with style suffix).
pub fn build_system_prompt(gen_config: &GenerateConfig, detailed: bool) -> String {
    if detailed {
        resolve_system_prompt(
            DETAILED_SYSTEM_PROMPT_BASE,
            gen_config.prompts.detailed.as_deref(),
            gen_config,
            true,
        )
    } else {
        resolve_system_prompt(
            SIMPLE_SYSTEM_PROMPT_BASE,
            gen_config.prompts.simple.as_deref(),
            gen_config,
            true,
        )
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

fn truncate_diff(diff: &str, max_len: usize) -> String {
    if diff.len() > max_len {
        format!(
            "{}\n\n... (diff truncated, {} bytes total)",
            &diff[..max_len],
            diff.len()
        )
    } else {
        diff.to_string()
    }
}

/// Strip markdown code fences and any non-command prose lines.
fn extract_commands(raw: &str) -> String {
    let mut in_fence = false;
    let mut commands: Vec<&str> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if !in_fence {
            let looks_like_prose = (trimmed.ends_with('.') || trimmed.ends_with(':'))
                && !trimmed.starts_with("git ")
                && !trimmed.starts_with("UNRECOVERABLE");
            if looks_like_prose {
                continue;
            }
        }
        commands.push(trimmed);
    }

    commands.join("\n")
}

pub fn generate_commit_message(
    diff: &str,
    needs_merge: bool,
    gen_config: &GenerateConfig,
) -> Result<String> {
    let truncated = truncate_diff(diff, gen_config.max_diff_bytes);

    let (prompt, system) = if needs_merge {
        (
            format!("Generate a commit message for this merge diff:\n\n```diff\n{truncated}\n```"),
            resolve_system_prompt(
                DETAILED_SYSTEM_PROMPT_BASE,
                gen_config.prompts.detailed.as_deref(),
                gen_config,
                true,
            ),
        )
    } else {
        (
            format!("Generate a commit message for this diff:\n\n```diff\n{truncated}\n```"),
            resolve_system_prompt(
                SIMPLE_SYSTEM_PROMPT_BASE,
                gen_config.prompts.simple.as_deref(),
                gen_config,
                true,
            ),
        )
    };

    call_provider(&prompt, &system, gen_config)
}

pub fn plan_commits(
    formatted_hunks: &str,
    valid_ids: &[usize],
    gen_config: &GenerateConfig,
) -> Result<Vec<CommitGroup>> {
    let resolved = config::resolve_provider(gen_config)?;

    if !resolved.structured_output {
        eprintln!(
            "[generate] Hunk splitting disabled: provider does not support structured output."
        );
        // Fall back to single commit via simple prompt
        let message = generate_commit_message(formatted_hunks, false, gen_config)?;
        return Ok(vec![CommitGroup {
            message,
            hunks: valid_ids.to_vec(),
        }]);
    }

    let truncated = truncate_diff(formatted_hunks, gen_config.max_diff_bytes);

    // Build plan prompt with style suffix injected
    let system = resolve_system_prompt(
        PLAN_COMMITS_SYSTEM_PROMPT_BASE,
        gen_config.prompts.plan.as_deref(),
        gen_config,
        true,
    );

    let prompt = format!(
        "Diff hunks:\n{truncated}\n\nGroup these hunks into logical commits. Output JSON only."
    );

    let raw = call_provider(&prompt, &system, gen_config)?;

    // Strip markdown fences if wrapped
    let json_str = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim());
    let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

    let groups: Vec<CommitGroup> = serde_json::from_str(json_str)
        .with_context(|| format!("failed to parse provider's commit plan as JSON:\n{raw}"))?;

    if groups.is_empty() {
        bail!("Provider returned an empty commit plan");
    }

    for group in &groups {
        if group.hunks.is_empty() {
            bail!(
                "Provider returned a commit group with no hunks: {}",
                group.message
            );
        }
        for hunk_id in &group.hunks {
            if !valid_ids.contains(hunk_id) {
                bail!("Provider referenced unknown hunk ID in commit plan: {hunk_id}");
            }
        }
    }

    Ok(groups)
}

pub fn fix_push_error(
    branch: &str,
    remote_url: &str,
    error: &str,
    gen_config: &GenerateConfig,
) -> Result<String> {
    let resolved = config::resolve_provider(gen_config)?;

    if !resolved.structured_output {
        bail!(
            "Push error recovery requires structured output. \
             Current provider does not support it. Fix the push manually."
        );
    }

    let system = resolve_system_prompt(
        PUSH_FIX_SYSTEM_PROMPT,
        gen_config.prompts.push_fix.as_deref(),
        gen_config,
        false, // no style suffix for push fix
    );

    let prompt = format!(
        "git push failed for branch `{branch}` on remote `{remote_url}`.\n\nError:\n{error}\n\nOutput the shell commands to fix and complete the push."
    );
    let raw = call_provider(&prompt, &system, gen_config)?;
    Ok(extract_commands(&raw))
}

/// Resolve merge conflicts using Claude (Claude-only feature).
/// Returns an error if the current provider is not Claude.
pub fn resolve_conflicts(
    conflict_files: &[String],
    force: bool,
    gen_config: &GenerateConfig,
) -> Result<()> {
    if !config::is_claude_provider(gen_config) {
        bail!(
            "Automatic conflict resolution requires the Claude provider. \
             Resolve conflicts manually or switch provider to Claude."
        );
    }

    let file_list = conflict_files.join(", ");

    let system = gen_config
        .prompts
        .conflict_resolve
        .as_deref()
        .unwrap_or(CONFLICT_RESOLVE_PROMPT);

    let prompt = format!(
        "These files have merge conflicts that need resolving: {file_list}\n\n\
         Please read each file, resolve all merge conflicts, write the fixed content back, \
         and stage each file with `git add`."
    );

    let mut args = vec!["-p", &prompt, "--system-prompt", system];
    if force {
        args.push("--dangerously-skip-permissions");
    } else {
        args.push("--allowedTools");
        args.push("Edit,Read,Bash");
    }

    let status = Command::new("claude")
        .args(&args)
        .env_remove("CLAUDECODE")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context("failed to run claude CLI for conflict resolution")?;

    if !status.success() {
        bail!("claude conflict resolution failed");
    }

    Ok(())
}
