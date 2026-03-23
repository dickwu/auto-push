use crate::config::{self, GenerateConfig};

// ---------------------------------------------------------------------------
// Built-in system prompts (used when no custom prompts are configured)
// ---------------------------------------------------------------------------

const SIMPLE_SYSTEM_PROMPT_BASE: &str = r#"You are a git commit message generator. Given a git diff, generate a concise, conventional commit message.

Rules:
- Output ONLY the commit message, nothing else"#;

const DETAILED_SYSTEM_PROMPT_BASE: &str = r#"You are a git commit message generator. Given a git diff that includes a merge, generate a conventional commit message.

Rules:
- Keep the first line concise
- Add a blank line then a body explaining what was merged and any conflicts resolved
- Body should be 2-5 lines max
- Output ONLY the commit message, nothing else"#;

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
