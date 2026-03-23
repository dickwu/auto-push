use crate::config::AppConfig;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct PreflightResult {
    pub repo_root: PathBuf,
    pub branch: String,
    pub remote: String,
    #[allow(dead_code)]
    pub is_shallow: bool,
    pub has_submodules: bool,
    pub submodule_paths: Vec<String>,
    #[allow(dead_code)]
    pub has_lfs: bool,
    pub has_upstream: bool,
}

#[derive(Clone)]
pub struct CliFlags {
    pub stage_all: bool,
    pub no_push: bool,
    pub no_pull: bool,
    pub no_stash: bool,
    pub no_submodules: bool,
    pub no_pre_push: bool,
    pub no_after_push: bool,
    pub no_hooks: bool,
    pub no_generate: bool,
    pub confirm: bool,
    pub dry_run: bool,
    pub message: Option<String>,
    pub force: bool,
    pub rebase: bool,
    #[allow(dead_code)]
    pub provider_override: Option<String>,
    pub skip: Vec<String>,
    #[allow(dead_code)] // wired in Task 9
    pub var_overrides: Vec<(String, String)>,
}

pub struct Context {
    pub preflight: PreflightResult,
    pub cli: CliFlags,
    pub app_config: AppConfig,
}

/// Map deprecated boolean flags to --skip entries and print warnings.
#[allow(dead_code)] // called from main.rs in Task 9
pub fn apply_deprecation_flags(cli: &mut CliFlags) {
    let mappings = [
        (cli.no_pull, "pull", "--no-pull"),
        (cli.no_push, "push", "--no-push"),
        (cli.no_generate, "generate", "--no-generate"),
        (cli.no_stash, "stash", "--no-stash"),
        (cli.no_submodules, "submodules", "--no-submodules"),
    ];
    for (flag, skip_name, flag_name) in &mappings {
        if *flag && !cli.skip.contains(&skip_name.to_string()) {
            eprintln!("Warning: {flag_name} is deprecated. Use --skip {skip_name} instead.");
            cli.skip.push(skip_name.to_string());
        }
    }
    if cli.rebase {
        eprintln!(
            "Warning: --rebase is deprecated. Use 'git pull --rebase' in your pipeline instead."
        );
    }
}

/// Apply --var overrides to a vars map. Rejects built-in overrides.
#[allow(dead_code)] // called from main.rs in Task 9
pub fn apply_var_overrides(
    vars: &mut HashMap<String, String>,
    overrides: &[(String, String)],
) -> anyhow::Result<()> {
    let builtins = crate::vars::builtin_var_names();
    for (key, value) in overrides {
        if builtins.contains(key) {
            anyhow::bail!("Cannot override built-in variable '{key}' with --var.");
        }
        vars.insert(key.clone(), value.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_preflight() -> PreflightResult {
        PreflightResult {
            repo_root: PathBuf::from("/tmp/repo"),
            branch: "main".to_string(),
            remote: "origin".to_string(),
            is_shallow: false,
            has_submodules: false,
            submodule_paths: vec![],
            has_lfs: false,
            has_upstream: true,
        }
    }

    fn dummy_cli() -> CliFlags {
        CliFlags {
            stage_all: true,
            no_push: false,
            no_pull: false,
            no_stash: false,
            no_submodules: false,
            no_pre_push: false,
            no_after_push: false,
            no_hooks: false,
            no_generate: false,
            confirm: false,
            dry_run: false,
            message: None,
            force: false,
            rebase: false,
            provider_override: None,
            skip: vec![],
            var_overrides: vec![],
        }
    }

    #[test]
    fn test_context_construction() {
        let ctx = Context {
            preflight: dummy_preflight(),
            cli: dummy_cli(),
            app_config: AppConfig::default(),
        };
        assert_eq!(ctx.preflight.branch, "main");
        assert!(ctx.cli.stage_all);
        assert!(!ctx.cli.rebase);
    }

    #[test]
    fn test_cli_flags_has_hook_fields() {
        let cli = CliFlags {
            stage_all: true,
            no_push: false,
            no_pull: false,
            no_stash: false,
            no_submodules: false,
            no_pre_push: false,
            no_after_push: false,
            no_hooks: false,
            no_generate: false,
            confirm: false,
            dry_run: false,
            message: None,
            force: false,
            rebase: false,
            provider_override: None,
            skip: vec![],
            var_overrides: vec![],
        };
        assert!(!cli.no_after_push);
        assert!(!cli.no_hooks);
    }

    #[test]
    fn test_apply_deprecation_flags() {
        let mut cli = dummy_cli();
        cli.no_pull = true;
        cli.no_push = true;
        apply_deprecation_flags(&mut cli);
        assert!(cli.skip.contains(&"pull".to_string()));
        assert!(cli.skip.contains(&"push".to_string()));
    }

    #[test]
    fn test_apply_deprecation_flags_no_duplicates() {
        let mut cli = dummy_cli();
        cli.no_pull = true;
        cli.skip.push("pull".to_string());
        apply_deprecation_flags(&mut cli);
        assert_eq!(cli.skip.iter().filter(|s| *s == "pull").count(), 1);
    }

    #[test]
    fn test_apply_var_overrides() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("team".to_string(), "original".to_string());
        apply_var_overrides(&mut vars, &[("team".to_string(), "override".to_string())]).unwrap();
        assert_eq!(vars.get("team").unwrap(), "override");
    }

    #[test]
    fn test_apply_var_overrides_builtin_rejected() {
        let mut vars = std::collections::HashMap::new();
        let result = apply_var_overrides(&mut vars, &[("branch".to_string(), "x".to_string())]);
        assert!(result.is_err());
    }
}
