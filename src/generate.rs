use crate::config::{self, GenerateConfig};
use anyhow::{Context, Result, bail};
use std::process::Command;

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
// Conflict resolution (Claude-only interactive feature)
// ---------------------------------------------------------------------------

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
