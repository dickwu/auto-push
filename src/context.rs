use crate::config::AppConfig;
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
}

pub struct Context {
    pub preflight: PreflightResult,
    pub cli: CliFlags,
    pub app_config: AppConfig,
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
        };
        assert!(!cli.no_after_push);
        assert!(!cli.no_hooks);
    }
}
