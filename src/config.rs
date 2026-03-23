use anyhow::{Context, Result};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = ".auto-push.json";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub generate: GenerateConfig,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    #[serde(default)]
    pub pipeline: Option<Vec<PipelineCommand>>,
    #[serde(default)]
    pub pre_push: Vec<PipelineCommand>,
    #[serde(default)]
    pub after_push: Vec<PipelineCommand>,
    #[serde(default)]
    pub branches: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateConfig {
    #[serde(default)]
    pub provider: Option<ProviderConfig>,
    #[serde(default)]
    pub commit_style: CommitStyle,
    #[serde(default)]
    pub prompts: CustomPrompts,
    #[serde(default = "default_max_diff_bytes")]
    pub max_diff_bytes: usize,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub structured_output: Option<bool>,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderConfig {
    Preset(String),
    Custom(CustomProvider),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProvider {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitStyle {
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_types")]
    pub types: Vec<String>,
    #[serde(default = "default_max_length")]
    pub max_length: usize,
    #[serde(default = "default_include_body")]
    pub include_body: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomPrompts {
    pub simple: Option<String>,
    pub detailed: Option<String>,
    pub plan: Option<String>,
    pub push_fix: Option<String>,
    pub conflict_resolve: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineCommand {
    pub name: String,
    /// Shell mode: template string passed to sh -c
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Argv mode: command name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Argv mode: command arguments
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interactive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_after: Option<Vec<CaptureAfterEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_mode: Option<CaptureMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAfterEntry {
    pub name: String,
    pub run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    Stdout,
    Stderr,
    Both,
}

fn is_false(v: &bool) -> bool {
    !v
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_format() -> String {
    "conventional".into()
}

fn default_types() -> Vec<String> {
    [
        "feat", "fix", "refactor", "docs", "test", "chore", "perf", "ci",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_max_length() -> usize {
    72
}

fn default_include_body() -> bool {
    true
}

fn default_max_diff_bytes() -> usize {
    20_000
}

impl Default for GenerateConfig {
    fn default() -> Self {
        Self {
            provider: None,
            commit_style: CommitStyle::default(),
            prompts: CustomPrompts::default(),
            max_diff_bytes: default_max_diff_bytes(),
            description: None,
            structured_output: None,
            timeout_secs: 0,
        }
    }
}

impl Default for CommitStyle {
    fn default() -> Self {
        Self {
            format: default_format(),
            types: default_types(),
            max_length: default_max_length(),
            include_body: default_include_body(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            generate: GenerateConfig::default(),
            vars: HashMap::new(),
            pipeline: None,
            pre_push: Vec::new(),
            after_push: Vec::new(),
            branches: serde_json::Map::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Style suffix generation
// ---------------------------------------------------------------------------

/// Generate a style suffix from commit_style config to append to system prompts.
pub fn style_suffix(style: &CommitStyle) -> String {
    let types_str = style.types.join(", ");
    let body_str = if style.include_body { "yes" } else { "no" };
    format!(
        "\nCommit message rules:\n\
         - Format: {}\n\
         - Allowed types: {}\n\
         - Max first line: {} characters\n\
         - Include body: {}",
        style.format, types_str, style.max_length, body_str
    )
}

// ---------------------------------------------------------------------------
// Config loading and layering
// ---------------------------------------------------------------------------

pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(CONFIG_FILE)
}

fn global_config_path() -> PathBuf {
    dirs_or_home().join(CONFIG_FILE)
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Load, merge, and return the final AppConfig for the given repo and branch.
/// If no repo-level config exists, auto-init one with smart defaults.
pub fn load(repo_root: &Path, branch: &str) -> Result<AppConfig> {
    let repo_path = config_path(repo_root);

    // Auto-init if repo config doesn't exist
    if !repo_path.exists() {
        auto_init(repo_root)?;
    }

    // Start with built-in defaults as the base
    let mut merged =
        serde_json::to_value(AppConfig::default()).context("failed to serialize default config")?;

    // Layer 1: global config
    let global_path = global_config_path();
    if global_path.exists() {
        let content = std::fs::read_to_string(&global_path)
            .with_context(|| format!("failed to read {}", global_path.display()))?;
        let global_val: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", global_path.display()))?;
        deep_merge(&mut merged, &global_val);
    }

    // Layer 2: repo config
    if repo_path.exists() {
        let content = std::fs::read_to_string(&repo_path)
            .with_context(|| format!("failed to read {}", repo_path.display()))?;
        let repo_val: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", repo_path.display()))?;
        deep_merge(&mut merged, &repo_val);
    }

    // Layer 3: branch overrides
    apply_branch_overrides(&mut merged, branch)?;

    // Remove the branches key before deserializing (it's metadata, not config)
    if let Some(obj) = merged.as_object_mut() {
        obj.remove("branches");
    }

    let mut config: AppConfig =
        serde_json::from_value(merged).context("failed to deserialize merged config")?;

    // Migrate legacy pre_push/after_push to pipeline if no pipeline is set
    if config.pipeline.is_none() && (!config.pre_push.is_empty() || !config.after_push.is_empty()) {
        let migrated = migrate_to_pipeline(&config)?;
        config = AppConfig {
            pipeline: Some(migrated),
            ..config
        };
    }

    // Validate variable registry (catches duplicates, forward refs, collisions)
    if let Some(ref pipeline) = config.pipeline {
        crate::vars::validate_var_registry(pipeline, &config.vars)?;
    }
    // Also validate legacy pre_push/after_push if no pipeline
    if config.pipeline.is_none() && !config.pre_push.is_empty() {
        crate::vars::validate_var_registry(&config.pre_push, &config.vars)?;
    }

    Ok(config)
}

/// Deep merge `overlay` into `base`. Objects are recursively merged.
/// Arrays and scalars in overlay replace base. Null values remove the key.
pub fn deep_merge(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                if overlay_val.is_null() {
                    base_map.remove(key);
                } else if let Some(base_val) = base_map.get_mut(key) {
                    deep_merge(base_val, overlay_val);
                } else {
                    base_map.insert(key.clone(), overlay_val.clone());
                }
            }
        }
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

fn apply_branch_overrides(merged: &mut serde_json::Value, branch: &str) -> Result<()> {
    let branches = match merged.get("branches") {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => return Ok(()),
    };

    for (pattern, override_val) in &branches {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .with_context(|| format!("invalid branch glob pattern: '{pattern}'"))?
            .compile_matcher();

        if glob.is_match(branch) {
            deep_merge(merged, override_val);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-init
// ---------------------------------------------------------------------------

fn auto_init(repo_root: &Path) -> Result<()> {
    let path = config_path(repo_root);
    let provider = detect_provider();
    let project_commands = detect_project_commands(repo_root);

    let mut config = serde_json::Map::new();

    // Build pipeline array
    let mut pipeline = Vec::new();

    // Stash + Pull + Unstash
    pipeline.push(serde_json::json!({
        "name": "stash",
        "run": "git stash push -m 'auto-push auto-stash' || true",
        "description": "Stash uncommitted changes"
    }));
    pipeline.push(serde_json::json!({
        "name": "pull",
        "run": "git pull",
        "description": "Pull latest changes"
    }));
    pipeline.push(serde_json::json!({
        "name": "unstash",
        "run": "git stash pop || true",
        "description": "Restore stashed changes"
    }));

    // Project-specific commands (tests, lint, fmt)
    for cmd in &project_commands {
        pipeline.push(serde_json::to_value(cmd).context("failed to serialize project command")?);
    }

    // Stage
    pipeline.push(serde_json::json!({
        "name": "stage",
        "run": "git add -A",
        "description": "Stage all changes"
    }));

    // Generate (provider-specific, argv mode)
    if let Some(ref prov) = provider {
        let gen_cmd = match prov.as_str() {
            "claude" => serde_json::json!({
                "name": "generate",
                "command": "claude",
                "args": [
                    "-p",
                    "Generate a commit message for this diff:\n\n{{ diff }}",
                    "--system-prompt", "{{ system_prompt }}",
                    "--output-format", "text",
                    "--no-session-persistence",
                    "--tools", ""
                ],
                "capture": "commit_message",
                "description": "Generate commit message with AI"
            }),
            "codex" => serde_json::json!({
                "name": "generate",
                "command": "codex",
                "args": [
                    "exec", "--color", "never",
                    "{{ system_prompt }}\n\nGenerate a commit message for this diff:\n\n{{ diff }}"
                ],
                "capture": "commit_message",
                "description": "Generate commit message with AI"
            }),
            "ollama" => serde_json::json!({
                "name": "generate",
                "command": "ollama",
                "args": [
                    "run", "llama3",
                    "{{ system_prompt }}\n\nGenerate a commit message for this diff:\n\n{{ diff }}"
                ],
                "capture": "commit_message",
                "description": "Generate commit message with AI"
            }),
            _ => serde_json::json!({
                "name": "generate",
                "run": "echo 'No AI provider detected. Install claude, codex, or ollama.'",
                "capture": "commit_message",
                "description": "Generate commit message with AI (PLACEHOLDER)"
            }),
        };
        pipeline.push(gen_cmd);
        println!("[config] Auto-detected provider: {prov}");
    } else {
        eprintln!("[config] Warning: no AI CLI detected. Install claude, codex, or ollama.");
        pipeline.push(serde_json::json!({
            "name": "generate",
            "run": "echo 'Configure an AI provider in .auto-push.json'",
            "capture": "commit_message"
        }));
    }

    // Commit
    pipeline.push(serde_json::json!({
        "name": "commit",
        "run": "git commit -m '{{ commit_message }}'",
        "description": "Create commit",
        "capture_after": [
            {"name": "commit_hash", "run": "git rev-parse --short HEAD"},
            {"name": "commit_summary", "run": "git log -1 --format=%s"}
        ]
    }));

    // Push
    pipeline.push(serde_json::json!({
        "name": "push",
        "run": "git push origin {{ branch }}",
        "description": "Push to remote",
        "on_error": "sleep 2 && git push origin {{ branch }}"
    }));

    config.insert("pipeline".into(), serde_json::Value::Array(pipeline));

    // Write config
    let content =
        serde_json::to_string_pretty(&config).context("failed to serialize auto-init config")?;
    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    update_gitignore(repo_root);
    println!("[config] Created {CONFIG_FILE}. Run `auto-push --show-config` to see full config.");

    Ok(())
}

fn detect_provider() -> Option<String> {
    for name in ["claude", "codex", "ollama"] {
        if command_exists(name) {
            return Some(name.to_string());
        }
    }
    None
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn detect_project_commands(repo_root: &Path) -> Vec<PipelineCommand> {
    if repo_root.join("Cargo.toml").exists() {
        vec![
            PipelineCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: Some("cargo test".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "lint".into(),
                description: Some("Check for common mistakes and style issues".into()),
                run: Some("cargo clippy -- -D warnings".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "format check".into(),
                description: Some("Verify code formatting matches rustfmt rules".into()),
                run: Some("cargo fmt -- --check".into()),
                ..Default::default()
            },
        ]
    } else if repo_root.join("package.json").exists() {
        vec![
            PipelineCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: Some("npm test".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "lint".into(),
                description: Some("Check for common mistakes and style issues".into()),
                run: Some("npm run lint".into()),
                ..Default::default()
            },
        ]
    } else if repo_root.join("go.mod").exists() {
        vec![
            PipelineCommand {
                name: "tests".into(),
                description: Some("Run the project test suite".into()),
                run: Some("go test ./...".into()),
                ..Default::default()
            },
            PipelineCommand {
                name: "vet".into(),
                description: Some("Check for suspicious constructs".into()),
                run: Some("go vet ./...".into()),
                ..Default::default()
            },
        ]
    } else {
        vec![]
    }
}

fn update_gitignore(repo_root: &Path) {
    let gitignore_path = repo_root.join(".gitignore");
    let entry = CONFIG_FILE;

    let already_listed = gitignore_path
        .exists()
        .then(|| std::fs::read_to_string(&gitignore_path).ok())
        .flatten()
        .map(|content| content.lines().any(|line| line.trim() == entry))
        .unwrap_or(false);

    if already_listed {
        return;
    }

    let append = if gitignore_path.exists() {
        format!("\n{entry}\n")
    } else {
        format!("{entry}\n")
    };

    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore_path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(append.as_bytes())
        })
    {
        eprintln!("[config] Warning: could not update .gitignore: {e}");
    }
}

// ---------------------------------------------------------------------------
// Legacy config migration
// ---------------------------------------------------------------------------

/// Migrate legacy `pre_push`/`after_push` configs into a unified `pipeline` array.
///
/// Build order: stash, pull, unstash, stage, <pre_push>, generate, commit, push, <after_push>
pub fn migrate_to_pipeline(config: &AppConfig) -> Result<Vec<PipelineCommand>> {
    let mut pipeline = vec![
        // Stash
        PipelineCommand {
            name: "stash".into(),
            run: Some("git stash push -m 'auto-push auto-stash' || true".into()),
            description: Some("Stash uncommitted changes".into()),
            ..Default::default()
        },
        // Pull
        PipelineCommand {
            name: "pull".into(),
            run: Some("git pull".into()),
            description: Some("Pull latest changes".into()),
            ..Default::default()
        },
        // Unstash
        PipelineCommand {
            name: "unstash".into(),
            run: Some("git stash pop || true".into()),
            description: Some("Restore stashed changes".into()),
            ..Default::default()
        },
        // Stage
        PipelineCommand {
            name: "stage".into(),
            run: Some("git add -A".into()),
            description: Some("Stage all changes".into()),
            ..Default::default()
        },
    ];

    // Pre-push hooks (migrated with capture_mode: Both to preserve old behavior)
    for cmd in &config.pre_push {
        let migrated = PipelineCommand {
            capture_mode: if cmd.capture_mode.is_none() {
                Some(CaptureMode::Both)
            } else {
                cmd.capture_mode.clone()
            },
            ..cmd.clone()
        };
        pipeline.push(migrated);
    }

    // Generate (from provider config)
    let gen_cmd = build_generate_command(&config.generate)?;
    pipeline.push(gen_cmd);

    // Commit
    pipeline.push(PipelineCommand {
        name: "commit".into(),
        run: Some("git commit -m '{{ commit_message }}'".into()),
        description: Some("Create commit".into()),
        capture_after: Some(vec![
            CaptureAfterEntry {
                name: "commit_hash".into(),
                run: "git rev-parse --short HEAD".into(),
            },
            CaptureAfterEntry {
                name: "commit_summary".into(),
                run: "git log -1 --format=%s".into(),
            },
        ]),
        ..Default::default()
    });

    // Push
    pipeline.push(PipelineCommand {
        name: "push".into(),
        run: Some("git push origin {{ branch }}".into()),
        description: Some("Push to remote".into()),
        on_error: Some("sleep 2 && git push origin {{ branch }}".into()),
        ..Default::default()
    });

    // After-push hooks (migrated with capture_mode: Both to preserve old behavior)
    for cmd in &config.after_push {
        let migrated = PipelineCommand {
            capture_mode: if cmd.capture_mode.is_none() {
                Some(CaptureMode::Both)
            } else {
                cmd.capture_mode.clone()
            },
            ..cmd.clone()
        };
        pipeline.push(migrated);
    }

    eprintln!("[config] Migrated pre_push/after_push to pipeline format.");
    eprintln!("[config] Update your .auto-push.json to use \"pipeline\" directly.");

    Ok(pipeline)
}

fn build_generate_command(gen_config: &GenerateConfig) -> Result<PipelineCommand> {
    let provider = gen_config.provider.as_ref();

    let is_claude_default = provider.is_none();
    let is_claude_preset = matches!(provider, Some(ProviderConfig::Preset(s)) if s == "claude");

    let (command, args) = if is_claude_default || is_claude_preset {
        (
            "claude".to_string(),
            vec![
                "-p".into(),
                "{{ diff }}".into(),
                "--system-prompt".into(),
                "{{ system_prompt }}".into(),
                "--output-format".into(),
                "text".into(),
                "--no-session-persistence".into(),
                "--tools".into(),
                "".into(),
            ],
        )
    } else {
        match provider {
            Some(ProviderConfig::Preset(s)) if s == "codex" => (
                "codex".to_string(),
                vec![
                    "exec".into(),
                    "--color".into(),
                    "never".into(),
                    "{{ system_prompt }}\n\nGenerate a commit message for this diff:\n\n{{ diff }}"
                        .into(),
                ],
            ),
            Some(ProviderConfig::Preset(s)) if s == "ollama" => (
                "ollama".to_string(),
                vec![
                    "run".into(),
                    "llama3".into(),
                    "{{ system_prompt }}\n\nGenerate a commit message for this diff:\n\n{{ diff }}"
                        .into(),
                ],
            ),
            Some(ProviderConfig::Custom(c)) => (c.command.clone(), c.args.clone()),
            Some(ProviderConfig::Preset(s)) => {
                anyhow::bail!("Unknown provider preset: '{s}'");
            }
            None => unreachable!(), // handled above
        }
    };

    Ok(PipelineCommand {
        name: "generate".into(),
        command: Some(command),
        args: Some(args),
        capture: Some("commit_message".into()),
        description: Some("Generate commit message with AI".into()),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Show config
// ---------------------------------------------------------------------------

pub fn show_config(repo_root: &Path, branch: &str) -> Result<()> {
    let config = load(repo_root, branch)?;
    let json =
        serde_json::to_string_pretty(&config).context("failed to serialize config for display")?;
    println!("{json}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-description for hook commands
// ---------------------------------------------------------------------------

pub fn auto_description(cmd: &PipelineCommand) -> String {
    if let Some(ref desc) = cmd.description {
        return desc.clone();
    }

    // Try to infer from run command
    if let Some(ref run_str) = cmd.run {
        let run = run_str.trim();
        if run.starts_with("cargo fmt") {
            return "Check Rust code formatting".into();
        }
        if run.starts_with("cargo clippy") {
            return "Run Rust linter".into();
        }
        if run.starts_with("cargo test") {
            return "Run Rust tests".into();
        }
        if run.starts_with("npm test") || run.starts_with("pnpm test") {
            return "Run JavaScript tests".into();
        }
        if run.contains("eslint") {
            return "Run JavaScript linter".into();
        }
        if run.starts_with("go vet") {
            return "Run Go static analysis".into();
        }
        if run.starts_with("pytest") || run.contains("pytest") {
            return "Run Python tests".into();
        }
    }

    // Fallback: humanize the name
    humanize_name(&cmd.name)
}

fn humanize_name(name: &str) -> String {
    name.replace(['_', '-'], " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    format!("{upper}{}", chars.as_str())
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deep_merge_objects() {
        let mut base = serde_json::json!({"a": 1, "b": {"c": 2}});
        let overlay = serde_json::json!({"b": {"d": 3}, "e": 4});
        deep_merge(&mut base, &overlay);
        assert_eq!(base["a"], 1);
        assert_eq!(base["b"]["c"], 2);
        assert_eq!(base["b"]["d"], 3);
        assert_eq!(base["e"], 4);
    }

    #[test]
    fn test_deep_merge_null_removes() {
        let mut base = serde_json::json!({"a": 1, "b": 2});
        let overlay = serde_json::json!({"a": null});
        deep_merge(&mut base, &overlay);
        assert!(base.get("a").is_none());
        assert_eq!(base["b"], 2);
    }

    #[test]
    fn test_deep_merge_array_replaces() {
        let mut base = serde_json::json!({"arr": [1, 2, 3]});
        let overlay = serde_json::json!({"arr": [4, 5]});
        deep_merge(&mut base, &overlay);
        assert_eq!(base["arr"], serde_json::json!([4, 5]));
    }

    #[test]
    fn test_deep_merge_scalar_replaces() {
        let mut base = serde_json::json!({"x": "old"});
        let overlay = serde_json::json!({"x": "new"});
        deep_merge(&mut base, &overlay);
        assert_eq!(base["x"], "new");
    }

    #[test]
    fn test_deep_merge_nested_three_levels() {
        let mut base = serde_json::json!({"a": {"b": {"c": 1}}});
        let overlay = serde_json::json!({"a": {"b": {"d": 2}}});
        deep_merge(&mut base, &overlay);
        assert_eq!(base["a"]["b"]["c"], 1);
        assert_eq!(base["a"]["b"]["d"], 2);
    }

    #[test]
    fn test_style_suffix() {
        let style = CommitStyle::default();
        let suffix = style_suffix(&style);
        assert!(suffix.contains("conventional"));
        assert!(suffix.contains("72"));
        assert!(suffix.contains("feat"));
    }

    #[test]
    fn test_style_suffix_custom() {
        let style = CommitStyle {
            format: "angular".into(),
            types: vec!["add".into(), "remove".into()],
            max_length: 50,
            include_body: false,
        };
        let suffix = style_suffix(&style);
        assert!(suffix.contains("angular"));
        assert!(suffix.contains("add, remove"));
        assert!(suffix.contains("50"));
        assert!(suffix.contains("Include body: no"));
    }

    #[test]
    fn test_auto_description_explicit() {
        let cmd = PipelineCommand {
            name: "test".into(),
            description: Some("My custom description".into()),
            run: Some("cargo test".into()),
            ..Default::default()
        };
        assert_eq!(auto_description(&cmd), "My custom description");
    }

    #[test]
    fn test_auto_description_inferred() {
        let cmd = PipelineCommand {
            name: "fmt_check".into(),
            run: Some("cargo fmt -- --check".into()),
            ..Default::default()
        };
        assert_eq!(auto_description(&cmd), "Check Rust code formatting");
    }

    #[test]
    fn test_auto_description_fallback() {
        let cmd = PipelineCommand {
            name: "custom_check".into(),
            run: Some("my-tool check".into()),
            ..Default::default()
        };
        assert_eq!(auto_description(&cmd), "Custom Check");
    }

    #[test]
    fn test_humanize_name() {
        assert_eq!(humanize_name("fmt_check"), "Fmt Check");
        assert_eq!(humanize_name("my-tool"), "My Tool");
        assert_eq!(humanize_name("test"), "Test");
    }

    #[test]
    fn test_config_backwards_compatible() {
        // Old configs with only pre_push/after_push should parse fine
        let json = r#"{"pre_push": [{"name": "test", "run": "cargo test"}], "after_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.pre_push.len(), 1);
        // provider defaults to None
        assert!(config.generate.provider.is_none());
    }

    #[test]
    fn test_config_with_generate_section() {
        let json = r#"{
            "generate": {
                "provider": "codex",
                "commit_style": {"max_length": 50}
            },
            "pre_push": []
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        assert!(
            matches!(config.generate.provider, Some(ProviderConfig::Preset(ref s)) if s == "codex")
        );
        assert_eq!(config.generate.commit_style.max_length, 50);
        // Other defaults still apply
        assert_eq!(config.generate.commit_style.format, "conventional");
    }

    #[test]
    fn test_config_custom_provider() {
        let json = r#"{
            "generate": {
                "provider": {
                    "command": "my-ai",
                    "args": ["--prompt", "{{ prompt }}"]
                }
            }
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        match &config.generate.provider {
            Some(ProviderConfig::Custom(c)) => {
                assert_eq!(c.command, "my-ai");
                assert_eq!(c.args.len(), 2);
            }
            _ => panic!("expected custom provider"),
        }
    }

    #[test]
    fn test_branch_override_exact_match() {
        let mut base = serde_json::json!({
            "generate": {"commit_style": {"max_length": 72}},
            "branches": {
                "main": {"generate": {"commit_style": {"max_length": 50}}}
            }
        });
        apply_branch_overrides(&mut base, "main").unwrap();
        assert_eq!(base["generate"]["commit_style"]["max_length"], 50);
    }

    #[test]
    fn test_branch_override_glob_match() {
        let mut base = serde_json::json!({
            "generate": {"commit_style": {"max_length": 72}},
            "branches": {
                "feature/*": {"generate": {"commit_style": {"max_length": 100}}}
            }
        });
        apply_branch_overrides(&mut base, "feature/add-auth").unwrap();
        assert_eq!(base["generate"]["commit_style"]["max_length"], 100);
    }

    #[test]
    fn test_branch_override_no_match() {
        let mut base = serde_json::json!({
            "generate": {"commit_style": {"max_length": 72}},
            "branches": {
                "main": {"generate": {"commit_style": {"max_length": 50}}}
            }
        });
        apply_branch_overrides(&mut base, "develop").unwrap();
        assert_eq!(base["generate"]["commit_style"]["max_length"], 72);
    }

    #[test]
    fn test_branch_override_replaces_hooks() {
        let mut base = serde_json::json!({
            "pre_push": [{"name": "test", "run": "cargo test"}],
            "branches": {
                "main": {"pre_push": []}
            }
        });
        apply_branch_overrides(&mut base, "main").unwrap();
        assert_eq!(base["pre_push"], serde_json::json!([]));
    }

    #[test]
    fn test_auto_init_creates_pipeline_config() {
        let dir = tempfile::tempdir().unwrap();
        // Create a Cargo.toml so it detects Rust project
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        auto_init(dir.path()).unwrap();

        let config_file = dir.path().join(CONFIG_FILE);
        assert!(config_file.exists());

        let content = std::fs::read_to_string(&config_file).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(val["pipeline"].is_array());
        assert!(val.get("pre_push").is_none());
        // Verify expected pipeline commands exist
        let pipeline = val["pipeline"].as_array().unwrap();
        let names: Vec<&str> = pipeline.iter().filter_map(|c| c["name"].as_str()).collect();
        assert!(names.contains(&"stash"));
        assert!(names.contains(&"pull"));
        assert!(names.contains(&"unstash"));
        assert!(names.contains(&"stage"));
        assert!(names.contains(&"generate"));
        assert!(names.contains(&"commit"));
        assert!(names.contains(&"push"));
        // Rust project should have tests + lint + format check
        assert!(names.contains(&"tests"));
        assert!(names.contains(&"lint"));
    }

    #[test]
    fn test_auto_init_updates_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        auto_init(dir.path()).unwrap();

        let gitignore = dir.path().join(".gitignore");
        assert!(gitignore.exists());
        let content = std::fs::read_to_string(&gitignore).unwrap();
        assert!(content.contains(CONFIG_FILE));
    }

    #[test]
    fn test_auto_init_gitignore_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".auto-push.json\n").unwrap();

        // Remove config to trigger auto-init
        auto_init(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        // Should not duplicate
        assert_eq!(content.matches(".auto-push.json").count(), 1);
    }

    #[test]
    fn test_load_backwards_compatible() {
        let dir = tempfile::tempdir().unwrap();
        // Write old-style config (hooks only, no generate)
        let json = r#"{"pre_push": [{"name": "test", "run": "echo hi"}], "after_push": []}"#;
        std::fs::write(dir.path().join(CONFIG_FILE), json).unwrap();

        let config = load(dir.path(), "main").unwrap();
        assert_eq!(config.pre_push.len(), 1);
        // Generate uses defaults; provider is None
        assert!(config.generate.provider.is_none());
    }

    // -----------------------------------------------------------------------
    // PipelineCommand tests
    // -----------------------------------------------------------------------

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
        let json =
            r#"{"name": "t", "run": "cargo test", "capture": "out", "capture_mode": "both"}"#;
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

    // -----------------------------------------------------------------------
    // Legacy config migration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_migrate_legacy_config_basic() {
        let json = r#"{
            "generate": {"provider": "claude"},
            "pre_push": [{"name": "test", "run": "cargo test"}],
            "after_push": [{"name": "notify", "run": "echo done"}]
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        assert!(pipeline.iter().any(|c| c.name == "stash"));
        assert!(pipeline.iter().any(|c| c.name == "pull"));
        assert!(pipeline.iter().any(|c| c.name == "test"));
        assert!(pipeline.iter().any(|c| c.name == "generate"));
        assert!(pipeline.iter().any(|c| c.name == "commit"));
        assert!(pipeline.iter().any(|c| c.name == "push"));
        assert!(pipeline.iter().any(|c| c.name == "notify"));
        // Generate uses argv mode
        let gen_cmd = pipeline.iter().find(|c| c.name == "generate").unwrap();
        assert!(gen_cmd.command.is_some());
        assert_eq!(gen_cmd.capture.as_deref(), Some("commit_message"));
        // Migrated hooks have capture_mode: Both
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
        let gen_cmd = pipeline.iter().find(|c| c.name == "generate").unwrap();
        assert_eq!(gen_cmd.command.as_deref(), Some("my-ai"));
    }

    #[test]
    fn test_migrate_codex_provider() {
        let json = r#"{"generate": {"provider": "codex"}, "pre_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let gen_cmd = pipeline.iter().find(|c| c.name == "generate").unwrap();
        assert_eq!(gen_cmd.command.as_deref(), Some("codex"));
        assert!(gen_cmd.args.as_ref().unwrap().contains(&"exec".to_string()));
    }

    #[test]
    fn test_migrate_default_provider() {
        let json = r#"{"pre_push": [{"name": "lint", "run": "cargo clippy"}]}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let gen_cmd = pipeline.iter().find(|c| c.name == "generate").unwrap();
        assert_eq!(gen_cmd.command.as_deref(), Some("claude")); // default
    }

    #[test]
    fn test_migrate_ordering() {
        let json = r#"{
            "pre_push": [{"name": "test", "run": "cargo test"}],
            "after_push": [{"name": "notify", "run": "echo done"}]
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let names: Vec<&str> = pipeline.iter().map(|c| c.name.as_str()).collect();
        // stash, pull, unstash, stage come before pre_push
        let stash_idx = names.iter().position(|n| *n == "stash").unwrap();
        let test_idx = names.iter().position(|n| *n == "test").unwrap();
        let gen_idx = names.iter().position(|n| *n == "generate").unwrap();
        let notify_idx = names.iter().position(|n| *n == "notify").unwrap();
        assert!(stash_idx < test_idx);
        assert!(test_idx < gen_idx);
        assert!(gen_idx < notify_idx);
    }

    #[test]
    fn test_migrate_ollama_provider() {
        let json = r#"{"generate": {"provider": "ollama"}, "pre_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let gen_cmd = pipeline.iter().find(|c| c.name == "generate").unwrap();
        assert_eq!(gen_cmd.command.as_deref(), Some("ollama"));
        assert!(gen_cmd.args.as_ref().unwrap().contains(&"run".to_string()));
        assert!(
            gen_cmd
                .args
                .as_ref()
                .unwrap()
                .contains(&"llama3".to_string())
        );
    }

    #[test]
    fn test_migrate_unknown_provider_fails() {
        let json = r#"{"generate": {"provider": "unknown-tool"}, "pre_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let result = migrate_to_pipeline(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown provider"));
    }

    #[test]
    fn test_migrate_commit_has_capture_after() {
        let json = r#"{"pre_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let commit = pipeline.iter().find(|c| c.name == "commit").unwrap();
        let ca = commit.capture_after.as_ref().unwrap();
        assert_eq!(ca.len(), 2);
        assert!(ca.iter().any(|e| e.name == "commit_hash"));
        assert!(ca.iter().any(|e| e.name == "commit_summary"));
    }

    #[test]
    fn test_migrate_push_has_on_error() {
        let json = r#"{"pre_push": []}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let push = pipeline.iter().find(|c| c.name == "push").unwrap();
        assert!(push.on_error.is_some());
    }

    #[test]
    fn test_migrate_preserves_existing_capture_mode() {
        let json = r#"{
            "pre_push": [{"name": "test", "run": "cargo test", "capture_mode": "stdout"}],
            "after_push": []
        }"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();
        let pipeline = migrate_to_pipeline(&config).unwrap();
        let test_cmd = pipeline.iter().find(|c| c.name == "test").unwrap();
        assert!(matches!(test_cmd.capture_mode, Some(CaptureMode::Stdout)));
    }

    #[test]
    fn test_migrate_load_wires_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{"pre_push": [{"name": "test", "run": "echo hi"}], "after_push": []}"#;
        std::fs::write(dir.path().join(CONFIG_FILE), json).unwrap();

        let config = load(dir.path(), "main").unwrap();
        // Migration should have produced a pipeline
        assert!(config.pipeline.is_some());
        let pipeline = config.pipeline.unwrap();
        assert!(pipeline.iter().any(|c| c.name == "stash"));
        assert!(pipeline.iter().any(|c| c.name == "generate"));
        assert!(pipeline.iter().any(|c| c.name == "test"));
    }

    #[test]
    fn test_no_migrate_when_pipeline_exists() {
        let dir = tempfile::tempdir().unwrap();
        let json = r#"{
            "pipeline": [{"name": "custom", "run": "echo hello"}],
            "pre_push": [{"name": "test", "run": "cargo test"}]
        }"#;
        std::fs::write(dir.path().join(CONFIG_FILE), json).unwrap();

        let config = load(dir.path(), "main").unwrap();
        // Should NOT have migrated; pipeline stays as-is
        let pipeline = config.pipeline.unwrap();
        assert_eq!(pipeline.len(), 1);
        assert_eq!(pipeline[0].name, "custom");
    }
}
