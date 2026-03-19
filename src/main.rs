mod claude;
mod context;
mod diff;
mod git;
mod hooks;
mod preflight;
mod pull;
mod push;
mod stage_commit;
mod stash;
mod submodule;

use anyhow::Result;
use clap::Parser;
use context::CliFlags;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "auto-push",
    version,
    about = "Automate your git workflow: pull, stage, generate AI commit messages, and push — all in one command",
    long_about = "auto-push streamlines your entire git workflow into a single command.\n\n\
        It pulls the latest changes, stages your work, uses the local Claude CLI to \
        analyze your diff and generate meaningful commit messages, then pushes to the remote.\n\n\
        Supports hunk-level commit splitting, submodule handling, auto-stash, \
        rebase, and intelligent push recovery.\n\n\
        Requires: git, claude (Claude Code CLI, authenticated)"
)]
struct Cli {
    /// Stage all changes before committing (enabled by default)
    #[arg(short = 'a', long, default_value_t = true)]
    stage_all: bool,

    /// Skip pushing to remote after committing
    #[arg(long)]
    no_push: bool,

    /// Skip pulling from remote before committing
    #[arg(long)]
    no_pull: bool,

    /// Skip submodule handling
    #[arg(long)]
    no_submodules: bool,

    /// Don't auto-stash dirty working tree (bail if dirty)
    #[arg(long)]
    no_stash: bool,

    /// Skip pre-push hooks even if .auto-push.json exists
    #[arg(long)]
    no_pre_push: bool,

    /// Skip after-push hooks even if .auto-push.json exists
    #[arg(long)]
    no_after_push: bool,

    /// Skip all hooks (pre-push and after-push)
    #[arg(long)]
    no_hooks: bool,

    /// Review and confirm before each action
    #[arg(short = 'c', long)]
    confirm: bool,

    /// Preview what would happen without making any changes
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Use a custom commit message instead of generating one with Claude
    #[arg(short = 'm', long)]
    message: Option<String>,

    /// Auto-resolve merge conflicts without prompts
    #[arg(short = 'f', long)]
    force: bool,

    /// Pull with rebase instead of merge
    #[arg(short = 'r', long)]
    rebase: bool,

    /// Generate a .auto-push.json config file in the repo root and exit
    #[arg(long)]
    init_hooks: bool,

    /// Show the current hook commands and exit
    #[arg(long)]
    show_hooks: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("auto-push v{}", env!("CARGO_PKG_VERSION"));

    if cli.init_hooks {
        git::ensure_git_repo()?;
        let root = PathBuf::from(git::repo_root()?);
        return hooks::init_config(&root);
    }

    if cli.show_hooks {
        git::ensure_git_repo()?;
        let root = PathBuf::from(git::repo_root()?);
        return hooks::show_config(&root);
    }

    // Phase 1: Preflight
    let preflight_result = preflight::check()?;

    let ctx = context::Context {
        preflight: preflight_result,
        cli: CliFlags {
            stage_all: cli.stage_all,
            no_push: cli.no_push,
            no_pull: cli.no_pull,
            no_stash: cli.no_stash,
            no_submodules: cli.no_submodules,
            no_pre_push: cli.no_pre_push,
            no_after_push: cli.no_after_push,
            no_hooks: cli.no_hooks,
            confirm: cli.confirm,
            dry_run: cli.dry_run,
            message: cli.message,
            force: cli.force,
            rebase: cli.rebase,
        },
    };

    // Phase 2: Stash (protect dirty tree before pull)
    let stash_result = stash::auto_stash(&ctx)?;

    // Phase 3: Pull
    let pull_outcome = pull::run(&ctx)?;

    // Phase 4: Submodule Sync (after pull so we have latest .gitmodules)
    submodule::sync(&ctx)?;

    // Phase 5: Unstash (restore changes for commit)
    stash::auto_unstash(&stash_result)?;

    // Phase 6: Pre-push hooks
    let hooks_config = if !ctx.cli.no_hooks {
        hooks::load_config(&ctx.preflight.repo_root)?
    } else {
        None
    };

    if !ctx.cli.no_hooks
        && !ctx.cli.no_pre_push
        && let Some(ref config) = hooks_config
    {
        let mut template_ctx = hooks::TemplateContext {
            branch: ctx.preflight.branch.clone(),
            remote: ctx.preflight.remote.clone(),
            commit_hash: git::run_git(&["rev-parse", "HEAD"]).unwrap_or_else(|_| {
                eprintln!("[hooks] Warning: could not resolve HEAD commit hash");
                String::new()
            }),
            command_outputs: std::collections::HashMap::new(),
        };
        hooks::run_phase(
            hooks::HookPhase::PrePush,
            config,
            &mut template_ctx,
            ctx.cli.dry_run,
        )?;
    }

    // Phase 7: Stage & Commit
    stage_commit::run(&ctx, &pull_outcome)?;

    // Phase 8: Push (submodules first, then parent)
    push::run(&ctx)?;

    // Phase 9: After-push hooks (only if push actually happened)
    if !ctx.cli.no_push
        && !ctx.cli.no_hooks
        && !ctx.cli.no_after_push
        && let Some(ref config) = hooks_config
    {
        let mut template_ctx = hooks::TemplateContext {
            branch: ctx.preflight.branch.clone(),
            remote: ctx.preflight.remote.clone(),
            commit_hash: git::run_git(&["rev-parse", "HEAD"]).unwrap_or_else(|_| {
                eprintln!("[hooks] Warning: could not resolve HEAD commit hash");
                String::new()
            }),
            command_outputs: std::collections::HashMap::new(),
        };
        if let Err(e) = hooks::run_phase(
            hooks::HookPhase::AfterPush,
            config,
            &mut template_ctx,
            ctx.cli.dry_run,
        ) {
            eprintln!("[after_push] Warning: {e}");
        }
    }

    Ok(())
}
