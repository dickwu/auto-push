use crate::config::PipelineCommand;
use crate::template;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::collections::HashSet;
use std::process::Command;

/// All built-in variable names that auto-push provides.
/// These cannot be overridden by user vars or captures.
pub fn builtin_var_names() -> HashSet<String> {
    [
        // Static (computed once at preflight)
        "branch",
        "remote",
        "remote_url",
        "repo_root",
        // Dynamic (lazy, recomputed when git state changes)
        "diff",
        "diff_stat",
        "hunks",
        "staged_files",
        "staged_count",
        // From generate config metadata
        "style_suffix",
        "system_prompt",
        "system_prompt_detailed",
        "system_prompt_plan",
        "push_fix_prompt",
        "conflict_resolve_prompt",
        "max_diff_bytes",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Check whether `name` is a valid variable name: [a-zA-Z_][a-zA-Z0-9_]*
fn is_valid_var_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extract variable name references from a template string.
/// Returns the root var names (before any dot-path or regex accessor).
pub fn extract_var_references(template: &str) -> Vec<String> {
    template::scan_template_expressions(template)
        .into_iter()
        .map(|(_, _, expr)| {
            // Extract root var name before . or :/
            let root = if let Some(dot) = expr.find('.') {
                &expr[..dot]
            } else if let Some(colon) = expr.find(":/") {
                &expr[..colon]
            } else {
                expr
            };
            root.trim().to_string()
        })
        .collect()
}

/// Validate the variable registry for a pipeline + user vars.
/// Checks:
/// - Rule 1: No duplicate names across built-ins, user vars, captures
/// - Rule 2: Every {{ var }} reference is resolvable at that pipeline index
/// - Rule 3: Exactly one of `run` or `command` must be set per command
/// - Rule 4: `interactive` + `capture`/`capture_after` is rejected
/// - Rule 5: Var names must match [a-zA-Z_][a-zA-Z0-9_]*
pub fn validate_var_registry(
    pipeline: &[PipelineCommand],
    user_vars: &HashMap<String, String>,
) -> Result<()> {
    let builtins = builtin_var_names();

    // Rule 5: validate user var names
    for name in user_vars.keys() {
        if !is_valid_var_name(name) {
            bail!(
                "Invalid variable name '{name}' in vars section. \
                 Names must match [a-zA-Z_][a-zA-Z0-9_]*."
            );
        }
    }

    // Rule 1a: user vars must not collide with built-ins
    for name in user_vars.keys() {
        if builtins.contains(name) {
            bail!(
                "Variable '{name}' in vars conflicts with built-in variable.\n\
                 \u{2192} Remove '{name}' from vars or use a different name."
            );
        }
    }

    // Collect all capture names and check for duplicates
    // name -> (pipeline index, command name)
    let mut all_captures: HashMap<String, (usize, String)> = HashMap::new();

    for (i, cmd) in pipeline.iter().enumerate() {
        // Rule 3: exactly one of run or command
        let has_run = cmd.run.is_some();
        let has_command = cmd.command.is_some();
        if !has_run && !has_command {
            bail!(
                "Pipeline command '{}' (index {i}) must have either 'run' or 'command' set.",
                cmd.name
            );
        }
        if has_run && has_command {
            bail!(
                "Pipeline command '{}' (index {i}) has both 'run' and 'command' set. \
                 Use exactly one.",
                cmd.name
            );
        }
        if cmd.args.is_some() && !has_command {
            bail!(
                "Pipeline command '{}' (index {i}) has 'args' without 'command'. \
                 'args' is only valid with 'command'.",
                cmd.name
            );
        }

        // Rule 4: interactive + capture rejected
        if cmd.interactive && (cmd.capture.is_some() || cmd.capture_after.is_some()) {
            bail!(
                "Pipeline command '{}' (index {i}) has 'interactive: true' with \
                 'capture' or 'capture_after'. Interactive mode disables output capture.",
                cmd.name
            );
        }

        // Collect captures for duplicate checking
        if let Some(ref cap_name) = cmd.capture {
            validate_capture_name(cap_name, &cmd.name, i, &builtins, user_vars, &all_captures)?;
            all_captures.insert(cap_name.clone(), (i, cmd.name.clone()));
        }

        if let Some(ref entries) = cmd.capture_after {
            for entry in entries {
                validate_capture_name(
                    &entry.name,
                    &cmd.name,
                    i,
                    &builtins,
                    user_vars,
                    &all_captures,
                )?;
                all_captures.insert(entry.name.clone(), (i, cmd.name.clone()));
            }
        }
    }

    // Rule 2: every {{ var }} reference must be resolvable at that pipeline index
    for (i, cmd) in pipeline.iter().enumerate() {
        // Collect available captures from commands BEFORE this one
        let available_captures: HashSet<String> = all_captures
            .iter()
            .filter(|(_, (idx, _))| *idx < i)
            .map(|(name, _)| name.clone())
            .collect();

        // Check all template fields
        let templates_to_check: Vec<&str> = [
            cmd.run.as_deref(),
            cmd.on_error.as_deref(),
            cmd.confirm.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();

        // Also check args
        let args_refs: Vec<String> = cmd
            .args
            .as_ref()
            .map(|args| {
                args.iter()
                    .flat_map(|a| extract_var_references(a))
                    .collect()
            })
            .unwrap_or_default();

        let mut all_refs: Vec<String> = templates_to_check
            .iter()
            .flat_map(|t| extract_var_references(t))
            .collect();
        all_refs.extend(args_refs);

        for var_name in &all_refs {
            if builtins.contains(var_name) {
                continue;
            }
            if user_vars.contains_key(var_name) {
                continue;
            }
            if available_captures.contains(var_name) {
                continue;
            }

            // Check if it is captured by a LATER command (actionable error)
            if let Some((cap_idx, cap_cmd)) = all_captures.get(var_name) {
                bail!(
                    "Variable '{var_name}' in command '{}' (index {i}) is not available.\n\
                     It is captured by command '{cap_cmd}' (index {cap_idx}) which runs later.\n\
                     \u{2192} Move '{cap_cmd}' before '{}' in your pipeline.",
                    cmd.name,
                    cmd.name
                );
            }

            bail!(
                "Variable '{var_name}' in command '{}' (index {i}) is not defined.\n\
                 It is not a built-in, user var, or captured by any pipeline command.",
                cmd.name
            );
        }
    }

    Ok(())
}

/// Validate a single capture name against all collision rules.
fn validate_capture_name(
    cap_name: &str,
    cmd_name: &str,
    index: usize,
    builtins: &HashSet<String>,
    user_vars: &HashMap<String, String>,
    all_captures: &HashMap<String, (usize, String)>,
) -> Result<()> {
    if !is_valid_var_name(cap_name) {
        bail!(
            "Invalid capture name '{cap_name}' in command '{cmd_name}' (index {index}). \
             Names must match [a-zA-Z_][a-zA-Z0-9_]*."
        );
    }
    if builtins.contains(cap_name) {
        bail!(
            "Capture name '{cap_name}' in command '{cmd_name}' (index {index}) \
             conflicts with built-in variable."
        );
    }
    if user_vars.contains_key(cap_name) {
        bail!(
            "Capture name '{cap_name}' in command '{cmd_name}' (index {index}) \
             conflicts with user variable in vars section."
        );
    }
    if let Some((prev_idx, prev_name)) = all_captures.get(cap_name) {
        bail!(
            "Duplicate capture name '{cap_name}'.\n\
             Captured by both '{prev_name}' (index {prev_idx}) and '{cmd_name}' (index {index}).\n\
             \u{2192} Each variable name must be unique across vars and captures."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git-mutating command detection
// ---------------------------------------------------------------------------

/// Check if a command modifies git state (staging, commits, stash, pull, checkout).
pub fn is_git_mutating(cmd: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "git add",
        "git commit",
        "git stash",
        "git pull",
        "git checkout",
        "git reset",
        "git merge",
        "git rebase",
        "git rm",
    ];
    PATTERNS.iter().any(|p| cmd.contains(p))
}

// ---------------------------------------------------------------------------
// Dynamic built-in variables
// ---------------------------------------------------------------------------

/// Returns true if `name` is a dynamic built-in (lazily computed from git state).
fn is_dynamic_builtin(name: &str) -> bool {
    matches!(
        name,
        "diff" | "diff_stat" | "hunks" | "staged_files" | "staged_count"
    )
}

/// Names of all dynamic built-in variables.
const DYNAMIC_BUILTINS: &[&str] = &["diff", "diff_stat", "hunks", "staged_files", "staged_count"];

fn run_git_capture(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() > max_bytes {
        format!(
            "{}\n\n... (diff truncated, {} bytes total)",
            &s[..max_bytes],
            s.len()
        )
    } else {
        s.to_string()
    }
}

/// Lazy resolver for dynamic built-in variables.
/// Computes values on first access, caches them, and invalidates when git state changes.
pub struct LazyVarResolver {
    cache: HashMap<String, String>,
    max_diff_bytes: usize,
    dirty: bool,
}

#[allow(dead_code)] // main.rs integration in Task 9; used by run_pipeline
impl LazyVarResolver {
    pub fn new(max_diff_bytes: usize) -> Self {
        Self {
            cache: HashMap::new(),
            max_diff_bytes,
            dirty: true,
        }
    }

    /// Invalidate the cache (called after git-mutating commands).
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.dirty = true;
    }

    /// Get a dynamic var value. Computes and caches on first access.
    /// Returns `None` if `name` is not a dynamic built-in.
    pub fn get(&mut self, name: &str) -> Option<String> {
        if !is_dynamic_builtin(name) {
            return None;
        }

        if self.dirty || !self.cache.contains_key(name) {
            if self.dirty {
                self.cache.clear();
                self.dirty = false;
            }
            if let Some(val) = self.compute(name) {
                self.cache.insert(name.to_string(), val);
            }
        }

        self.cache.get(name).cloned()
    }

    /// Return the list of all dynamic built-in variable names.
    pub fn dynamic_names() -> &'static [&'static str] {
        DYNAMIC_BUILTINS
    }

    fn compute(&self, name: &str) -> Option<String> {
        match name {
            "diff" => self.compute_diff(),
            "diff_stat" => self.compute_diff_stat(),
            "staged_files" => self.compute_staged_files(),
            "staged_count" => self.compute_staged_count(),
            "hunks" => self.compute_hunks(),
            _ => None,
        }
    }

    fn compute_diff(&self) -> Option<String> {
        run_git_capture(&["diff", "--cached"]).map(|d| truncate(&d, self.max_diff_bytes))
    }

    fn compute_diff_stat(&self) -> Option<String> {
        run_git_capture(&["diff", "--cached", "--stat"])
    }

    fn compute_staged_files(&self) -> Option<String> {
        run_git_capture(&["diff", "--cached", "--name-only"])
    }

    fn compute_staged_count(&self) -> Option<String> {
        self.compute_staged_files().map(|files| {
            files
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
                .to_string()
        })
    }

    fn compute_hunks(&self) -> Option<String> {
        // Use diff module's hunk parsing if available; for now return the raw diff
        self.compute_diff()
    }
}

// ---------------------------------------------------------------------------
// Static built-in variable construction
// ---------------------------------------------------------------------------

/// Build static built-in vars from preflight results and config.
#[allow(dead_code)] // main.rs integration in Task 9
pub fn build_static_vars(
    branch: &str,
    remote: &str,
    remote_url: &str,
    repo_root: &str,
    generate_config: &crate::config::GenerateConfig,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("branch".into(), branch.to_string());
    vars.insert("remote".into(), remote.to_string());
    vars.insert("remote_url".into(), remote_url.to_string());
    vars.insert("repo_root".into(), repo_root.to_string());
    vars.insert(
        "max_diff_bytes".into(),
        generate_config.max_diff_bytes.to_string(),
    );
    vars.insert(
        "style_suffix".into(),
        crate::config::style_suffix(&generate_config.commit_style),
    );

    // System prompts from generate metadata
    vars.insert(
        "system_prompt".into(),
        crate::generate::build_system_prompt(generate_config, false),
    );
    vars.insert(
        "system_prompt_detailed".into(),
        crate::generate::build_system_prompt(generate_config, true),
    );
    // push_fix_prompt and conflict_resolve_prompt from custom prompts
    if let Some(ref p) = generate_config.prompts.push_fix {
        vars.insert("push_fix_prompt".into(), p.clone());
    }
    if let Some(ref p) = generate_config.prompts.conflict_resolve {
        vars.insert("conflict_resolve_prompt".into(), p.clone());
    }

    vars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PipelineCommand;

    fn cmd(name: &str, run: &str) -> PipelineCommand {
        PipelineCommand {
            name: name.into(),
            run: Some(run.into()),
            ..Default::default()
        }
    }

    fn cmd_with_capture(name: &str, run: &str, capture: &str) -> PipelineCommand {
        PipelineCommand {
            name: name.into(),
            run: Some(run.into()),
            capture: Some(capture.into()),
            ..Default::default()
        }
    }

    #[test]
    fn test_builtin_names_defined() {
        let names = builtin_var_names();
        assert!(names.contains("branch"));
        assert!(names.contains("diff"));
        assert!(names.contains("system_prompt"));
        assert!(names.contains("remote_url"));
    }

    #[test]
    fn test_valid_var_names() {
        assert!(is_valid_var_name("foo"));
        assert!(is_valid_var_name("_bar"));
        assert!(is_valid_var_name("commit_message"));
        assert!(!is_valid_var_name(""));
        assert!(!is_valid_var_name("123"));
        assert!(!is_valid_var_name("foo.bar"));
        assert!(!is_valid_var_name("foo:/regex/"));
    }

    #[test]
    fn test_extract_var_references() {
        let refs = extract_var_references("echo {{ branch }} {{ commit_message }}");
        assert_eq!(refs, vec!["branch", "commit_message"]);
    }

    #[test]
    fn test_extract_var_references_dot_path() {
        let refs = extract_var_references("{{ plan.0.message }}");
        assert_eq!(refs, vec!["plan"]);
    }

    #[test]
    fn test_extract_var_references_regex() {
        let refs = extract_var_references("{{ ver:/v(\\d+)/ }}");
        assert_eq!(refs, vec!["ver"]);
    }

    #[test]
    fn test_validate_correct_pipeline_passes() {
        let pipeline = vec![
            cmd_with_capture("gen", "echo msg", "commit_message"),
            cmd("commit", "git commit -m '{{ commit_message }}'"),
        ];
        assert!(validate_var_registry(&pipeline, &HashMap::new()).is_ok());
    }

    #[test]
    fn test_validate_duplicate_capture_errors() {
        let pipeline = vec![
            cmd_with_capture("gen1", "echo a", "msg"),
            cmd_with_capture("gen2", "echo b", "msg"),
        ];
        let err = validate_var_registry(&pipeline, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("Duplicate capture"));
    }

    #[test]
    fn test_validate_var_collides_with_builtin() {
        let mut vars = HashMap::new();
        vars.insert("branch".to_string(), "override".to_string());
        let pipeline = vec![];
        let err = validate_var_registry(&pipeline, &vars).unwrap_err();
        assert!(err.to_string().contains("conflicts with built-in"));
    }

    #[test]
    fn test_validate_forward_reference_error() {
        let pipeline = vec![
            cmd("commit", "git commit -m '{{ msg }}'"),
            cmd_with_capture("gen", "echo hello", "msg"),
        ];
        let err = validate_var_registry(&pipeline, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("runs later"));
    }

    #[test]
    fn test_validate_user_var_resolves() {
        let pipeline = vec![cmd("notify", "echo {{ team }}")];
        let mut vars = HashMap::new();
        vars.insert("team".to_string(), "backend".to_string());
        assert!(validate_var_registry(&pipeline, &vars).is_ok());
    }

    #[test]
    fn test_validate_builtin_var_resolves() {
        let pipeline = vec![cmd("push", "git push {{ remote }} {{ branch }}")];
        assert!(validate_var_registry(&pipeline, &HashMap::new()).is_ok());
    }

    #[test]
    fn test_validate_interactive_with_capture_rejected() {
        let mut c = cmd_with_capture("gen", "echo a", "msg");
        c.interactive = true;
        let pipeline = vec![c];
        let err = validate_var_registry(&pipeline, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("interactive"));
    }

    #[test]
    fn test_validate_no_run_or_command_rejected() {
        let c = PipelineCommand {
            name: "bad".into(),
            ..Default::default()
        };
        let pipeline = vec![c];
        let err = validate_var_registry(&pipeline, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("must have either"));
    }

    #[test]
    fn test_validate_both_run_and_command_rejected() {
        let c = PipelineCommand {
            name: "bad".into(),
            run: Some("echo".into()),
            command: Some("echo".into()),
            ..Default::default()
        };
        let pipeline = vec![c];
        let err = validate_var_registry(&pipeline, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("both"));
    }

    #[test]
    fn test_validate_invalid_var_name_rejected() {
        let mut vars = HashMap::new();
        vars.insert("123bad".to_string(), "val".to_string());
        let err = validate_var_registry(&[], &vars).unwrap_err();
        assert!(err.to_string().contains("Invalid variable name"));
    }

    // -------------------------------------------------------------------
    // is_git_mutating tests
    // -------------------------------------------------------------------

    #[test]
    fn test_is_git_mutating() {
        assert!(is_git_mutating("git add -A"));
        assert!(is_git_mutating("git commit -m 'msg'"));
        assert!(is_git_mutating("git stash pop"));
        assert!(is_git_mutating("git pull"));
        assert!(is_git_mutating("git checkout main"));
        assert!(is_git_mutating("git reset --hard HEAD"));
        assert!(is_git_mutating("git merge feature"));
        assert!(is_git_mutating("git rebase main"));
        assert!(is_git_mutating("git rm file.txt"));
        assert!(!is_git_mutating("cargo test"));
        assert!(!is_git_mutating("echo hello"));
        assert!(!is_git_mutating("git status"));
        assert!(!is_git_mutating("git diff"));
        assert!(!is_git_mutating("git log"));
    }

    // -------------------------------------------------------------------
    // is_dynamic_builtin tests
    // -------------------------------------------------------------------

    #[test]
    fn test_is_dynamic_builtin() {
        assert!(is_dynamic_builtin("diff"));
        assert!(is_dynamic_builtin("diff_stat"));
        assert!(is_dynamic_builtin("hunks"));
        assert!(is_dynamic_builtin("staged_files"));
        assert!(is_dynamic_builtin("staged_count"));
        assert!(!is_dynamic_builtin("branch"));
        assert!(!is_dynamic_builtin("system_prompt"));
        assert!(!is_dynamic_builtin("remote"));
    }

    // -------------------------------------------------------------------
    // LazyVarResolver tests
    // -------------------------------------------------------------------

    #[test]
    fn test_lazy_resolver_invalidation() {
        let mut resolver = LazyVarResolver::new(20_000);
        // Seed the resolver with known state
        resolver.dirty = false;
        resolver.cache.insert("diff".into(), "old".into());
        resolver.invalidate();
        assert!(resolver.dirty);
        assert!(resolver.cache.is_empty());
    }

    #[test]
    fn test_lazy_resolver_non_dynamic_returns_none() {
        let mut resolver = LazyVarResolver::new(20_000);
        assert!(resolver.get("branch").is_none());
        assert!(resolver.get("system_prompt").is_none());
        assert!(resolver.get("nonexistent").is_none());
    }

    #[test]
    fn test_lazy_resolver_dynamic_names() {
        let names = LazyVarResolver::dynamic_names();
        assert!(names.contains(&"diff"));
        assert!(names.contains(&"diff_stat"));
        assert!(names.contains(&"hunks"));
        assert!(names.contains(&"staged_files"));
        assert!(names.contains(&"staged_count"));
    }

    #[test]
    fn test_truncate_no_op() {
        let s = "short";
        assert_eq!(truncate(s, 100), "short");
    }

    #[test]
    fn test_truncate_truncates() {
        let s = "hello world, this is a long string";
        let result = truncate(s, 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("... (diff truncated,"));
        assert!(result.contains("bytes total)"));
    }
}
