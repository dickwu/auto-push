use anyhow::{Context, Result, bail};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = ".auto-push.json";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub generate: GenerateConfig,
    #[serde(default)]
    pub pre_push: Vec<HookCommand>,
    #[serde(default)]
    pub after_push: Vec<HookCommand>,
    #[serde(default)]
    pub branches: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateConfig {
    #[serde(default = "default_provider")]
    pub provider: ProviderConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub run: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interactive: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_provider() -> ProviderConfig {
    ProviderConfig::Preset("claude".into())
}

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
            provider: default_provider(),
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
            pre_push: Vec::new(),
            after_push: Vec::new(),
            branches: serde_json::Map::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider presets
// ---------------------------------------------------------------------------

pub struct ResolvedProvider {
    pub command: String,
    pub args: Vec<String>,
    pub model: Option<String>,
    pub structured_output: bool,
}

pub fn resolve_provider(config: &GenerateConfig) -> Result<ResolvedProvider> {
    let explicit_structured = config.structured_output;

    match &config.provider {
        ProviderConfig::Preset(name) => match name.as_str() {
            "claude" => Ok(ResolvedProvider {
                command: "claude".into(),
                args: vec![
                    "-p".into(),
                    "{{ prompt }}".into(),
                    "--system-prompt".into(),
                    "{{ system_prompt }}".into(),
                ],
                model: None,
                structured_output: explicit_structured.unwrap_or(true),
            }),
            "codex" => Ok(ResolvedProvider {
                command: "codex".into(),
                args: vec!["--quiet".into(), "--prompt".into(), "{{ prompt }}".into()],
                model: None,
                structured_output: explicit_structured.unwrap_or(false),
            }),
            "ollama" => Ok(ResolvedProvider {
                command: "ollama".into(),
                args: vec!["run".into(), "{{ model }}".into(), "{{ prompt }}".into()],
                model: None,
                structured_output: explicit_structured.unwrap_or(false),
            }),
            other => bail!(
                "unknown provider preset: '{other}'. Use 'claude', 'codex', 'ollama', or a custom provider object."
            ),
        },
        ProviderConfig::Custom(custom) => Ok(ResolvedProvider {
            command: custom.command.clone(),
            args: custom.args.clone(),
            model: custom.model.clone(),
            structured_output: explicit_structured.unwrap_or(false),
        }),
    }
}

/// Check if the resolved provider is Claude (needed for Claude-only features).
pub fn is_claude_provider(config: &GenerateConfig) -> bool {
    match &config.provider {
        ProviderConfig::Preset(name) => name == "claude",
        ProviderConfig::Custom(c) => c.command == "claude",
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

    let config: AppConfig =
        serde_json::from_value(merged).context("failed to deserialize merged config")?;

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

    // Detect provider
    let provider = detect_provider();

    // Detect project type for hooks
    let pre_push = detect_pre_push_hooks(repo_root);

    let mut config = serde_json::Map::new();

    // Only write provider if detected
    if let Some(prov) = provider {
        let mut generate_obj = serde_json::Map::new();
        generate_obj.insert("provider".into(), serde_json::Value::String(prov.clone()));
        config.insert("generate".into(), serde_json::Value::Object(generate_obj));
        println!("[config] Auto-detected provider: {prov}");
    } else {
        eprintln!(
            "[config] Warning: no AI CLI detected. Install claude, codex, or ollama, \
             or configure a custom provider in .auto-push.json."
        );
    }

    config.insert(
        "pre_push".into(),
        serde_json::to_value(&pre_push).context("failed to serialize pre_push")?,
    );
    config.insert("after_push".into(), serde_json::Value::Array(vec![]));

    let content =
        serde_json::to_string_pretty(&config).context("failed to serialize auto-init config")?;

    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    // Update .gitignore
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

fn detect_pre_push_hooks(repo_root: &Path) -> Vec<HookCommand> {
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
                description: Some("Check for suspicious constructs".into()),
                run: "go vet ./...".into(),
                on_error: None,
                confirm: None,
                interactive: false,
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

pub fn auto_description(cmd: &HookCommand) -> String {
    if let Some(ref desc) = cmd.description {
        return desc.clone();
    }

    // Try to infer from run command
    let run = cmd.run.trim();
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
    fn test_resolve_provider_claude() {
        let config = GenerateConfig::default();
        let resolved = resolve_provider(&config).unwrap();
        assert_eq!(resolved.command, "claude");
        assert!(resolved.structured_output);
    }

    #[test]
    fn test_resolve_provider_codex() {
        let config = GenerateConfig {
            provider: ProviderConfig::Preset("codex".into()),
            ..Default::default()
        };
        let resolved = resolve_provider(&config).unwrap();
        assert_eq!(resolved.command, "codex");
        assert!(!resolved.structured_output);
    }

    #[test]
    fn test_resolve_provider_unknown_preset() {
        let config = GenerateConfig {
            provider: ProviderConfig::Preset("unknown".into()),
            ..Default::default()
        };
        assert!(resolve_provider(&config).is_err());
    }

    #[test]
    fn test_resolve_provider_custom() {
        let config = GenerateConfig {
            provider: ProviderConfig::Custom(CustomProvider {
                command: "my-tool".into(),
                args: vec!["--input".into(), "{{ prompt }}".into()],
                model: None,
                description: None,
            }),
            ..Default::default()
        };
        let resolved = resolve_provider(&config).unwrap();
        assert_eq!(resolved.command, "my-tool");
        assert!(!resolved.structured_output);
    }

    #[test]
    fn test_resolve_provider_explicit_structured_override() {
        let config = GenerateConfig {
            provider: ProviderConfig::Preset("codex".into()),
            structured_output: Some(true),
            ..Default::default()
        };
        let resolved = resolve_provider(&config).unwrap();
        assert!(resolved.structured_output);
    }

    #[test]
    fn test_is_claude_provider() {
        let claude_config = GenerateConfig::default();
        assert!(is_claude_provider(&claude_config));

        let codex_config = GenerateConfig {
            provider: ProviderConfig::Preset("codex".into()),
            ..Default::default()
        };
        assert!(!is_claude_provider(&codex_config));
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
        let cmd = HookCommand {
            name: "test".into(),
            description: Some("My custom description".into()),
            run: "cargo test".into(),
            on_error: None,
            confirm: None,
            interactive: false,
        };
        assert_eq!(auto_description(&cmd), "My custom description");
    }

    #[test]
    fn test_auto_description_inferred() {
        let cmd = HookCommand {
            name: "fmt_check".into(),
            description: None,
            run: "cargo fmt -- --check".into(),
            on_error: None,
            confirm: None,
            interactive: false,
        };
        assert_eq!(auto_description(&cmd), "Check Rust code formatting");
    }

    #[test]
    fn test_auto_description_fallback() {
        let cmd = HookCommand {
            name: "custom_check".into(),
            description: None,
            run: "my-tool check".into(),
            on_error: None,
            confirm: None,
            interactive: false,
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
        assert!(matches!(config.generate.provider, ProviderConfig::Preset(ref s) if s == "claude"));
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
        assert!(matches!(config.generate.provider, ProviderConfig::Preset(ref s) if s == "codex"));
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
            ProviderConfig::Custom(c) => {
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
    fn test_auto_init_creates_config() {
        let dir = tempfile::tempdir().unwrap();
        // Create a Cargo.toml so it detects Rust project
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        auto_init(dir.path()).unwrap();

        let config_file = dir.path().join(CONFIG_FILE);
        assert!(config_file.exists());

        let content = std::fs::read_to_string(&config_file).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(val["pre_push"].is_array());
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
        // Generate uses defaults
        assert!(matches!(config.generate.provider, ProviderConfig::Preset(ref s) if s == "claude"));
    }
}
