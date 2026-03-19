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
    pub confirm: bool,
    pub dry_run: bool,
    pub message: Option<String>,
    pub force: bool,
    pub rebase: bool,
}

pub struct Context {
    pub preflight: PreflightResult,
    pub cli: CliFlags,
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
            confirm: false,
            dry_run: false,
            message: None,
            force: false,
            rebase: false,
        }
    }

    #[test]
    fn test_context_construction() {
        let ctx = Context {
            preflight: dummy_preflight(),
            cli: dummy_cli(),
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
            confirm: false,
            dry_run: false,
            message: None,
            force: false,
            rebase: false,
        };
        assert!(!cli.no_after_push);
        assert!(!cli.no_hooks);
    }
}
