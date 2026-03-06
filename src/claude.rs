use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct CommitGroup {
    pub message: String,
    pub files: Vec<String>,
}

const PLAN_COMMITS_SYSTEM_PROMPT: &str = r#"You are a git commit planner. Given a list of changed files and a unified diff, decide how to group them into logical commits.

Rules:
- Group related changes together (e.g. a feature + its tests, a refactor across related files)
- Unrelated changes MUST be in separate commits (e.g. a bug fix and a new feature)
- Use conventional commit format for messages: <type>: <description>
- Types: feat, fix, refactor, docs, test, chore, perf, ci
- Keep each commit message under 72 characters
- If ALL changes are related, use a single commit group
- Output ONLY valid JSON, no markdown fences, no explanation

Output format (JSON array):
[{"message":"<commit message>","files":["path/to/file1","path/to/file2"]},{"message":"<commit message>","files":["path/to/file3"]}]"#;

const PUSH_FIX_SYSTEM_PROMPT: &str = r#"You are a git expert assistant. The user's `git push` failed.
Output ONLY shell commands to fix and complete the push. Each line must be a single executable shell command.
STRICT rules:
- NO explanations, NO prose, NO markdown, NO code fences, NO backticks
- Every line of your response will be passed directly to `sh -c` — write accordingly
- Use only standard git commands (no gh, no hub)
- Do not force-push unless the error clearly requires it
- Maximum 5 lines
- If the error is unrecoverable (no network, bad credentials, repo does not exist), output exactly one line: UNRECOVERABLE: <reason>"#;

const SIMPLE_SYSTEM_PROMPT: &str = r#"You are a git commit message generator. Given a git diff, generate a concise, conventional commit message.

Rules:
- Use conventional commit format: <type>: <description>
- Types: feat, fix, refactor, docs, test, chore, perf, ci
- Keep it to a single line, under 72 characters
- Output ONLY the commit message, nothing else"#;

const DETAILED_SYSTEM_PROMPT: &str = r#"You are a git commit message generator. Given a git diff that includes a merge, generate a conventional commit message.

Rules:
- Use conventional commit format: <type>: <description>
- Types: feat, fix, refactor, docs, test, chore, perf, ci
- Keep the first line under 72 characters
- Add a blank line then a body explaining what was merged and any conflicts resolved
- Body should be 2-5 lines max
- Output ONLY the commit message, nothing else"#;

fn call_claude(prompt: &str, system: &str) -> Result<String> {
    let output = Command::new("claude")
        .args(["-p", prompt, "--system-prompt", system])
        .env_remove("CLAUDECODE")
        .output()
        .context("failed to run claude CLI — is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("claude CLI failed: {}", stderr.trim());
    }

    let message = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if message.is_empty() {
        bail!("claude returned an empty commit message");
    }

    Ok(message)
}

fn truncate_diff(diff: &str) -> String {
    let max_len = 20_000;
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

/// Strip markdown code fences and any non-command prose lines from Claude output.
/// Keeps only lines that look like shell commands or the UNRECOVERABLE sentinel.
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
        // Skip prose lines: sentences ending with punctuation that aren't commands
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

pub fn fix_push_error(branch: &str, remote_url: &str, error: &str) -> Result<String> {
    let prompt = format!(
        "git push failed for branch `{branch}` on remote `{remote_url}`.\n\nError:\n{error}\n\nOutput the shell commands to fix and complete the push."
    );
    let raw = call_claude(&prompt, PUSH_FIX_SYSTEM_PROMPT)?;
    Ok(extract_commands(&raw))
}

pub fn generate_commit_message(diff: &str, needs_merge: bool) -> Result<String> {
    let truncated = truncate_diff(diff);

    let (prompt, system) = if needs_merge {
        (
            format!("Generate a commit message for this merge diff:\n\n```diff\n{truncated}\n```"),
            DETAILED_SYSTEM_PROMPT,
        )
    } else {
        (
            format!("Generate a commit message for this diff:\n\n```diff\n{truncated}\n```"),
            SIMPLE_SYSTEM_PROMPT,
        )
    };

    call_claude(&prompt, system)
}

pub fn plan_commits(files: &[String], diff: &str) -> Result<Vec<CommitGroup>> {
    let truncated = truncate_diff(diff);
    let file_list = files.join("\n");

    let prompt = format!(
        "Changed files:\n{file_list}\n\nDiff:\n```diff\n{truncated}\n```\n\nGroup these into logical commits. Output JSON only."
    );

    let raw = call_claude(&prompt, PLAN_COMMITS_SYSTEM_PROMPT)?;

    // Strip markdown fences if Claude wraps the JSON
    let json_str = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim());
    let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

    let groups: Vec<CommitGroup> = serde_json::from_str(json_str)
        .with_context(|| format!("failed to parse Claude's commit plan as JSON:\n{raw}"))?;

    if groups.is_empty() {
        bail!("Claude returned an empty commit plan");
    }

    // Validate that all files in the plan exist in the changed files list
    for group in &groups {
        if group.files.is_empty() {
            bail!("Claude returned a commit group with no files: {}", group.message);
        }
        for file in &group.files {
            if !files.contains(file) {
                bail!("Claude referenced unknown file in commit plan: {file}");
            }
        }
    }

    Ok(groups)
}
