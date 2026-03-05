use anyhow::{Context, Result, bail};
use std::process::Command;

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
