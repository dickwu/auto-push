mod claude;
mod context;
mod diff;
mod git;
mod hooks;
mod pre_push;
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

    /// Skip pre-push checks even if .pre-push.json exists
    #[arg(long)]
    no_pre_push: bool,

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

    /// Generate a .pre-push.json config file in the repo root and exit
    #[arg(long)]
    init_pre_push: bool,

    /// Show the current pre-push commands and exit
    #[arg(long)]
    show_pre_push: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("auto-push v{}", env!("CARGO_PKG_VERSION"));

    // Standalone: generate .pre-push.json and exit
    if cli.init_pre_push {
        git::ensure_git_repo()?;
        let root = PathBuf::from(git::repo_root()?);
        return pre_push::init_config(&root);
    }

    // Standalone: show current pre-push commands and exit
    if cli.show_pre_push {
        git::ensure_git_repo()?;
        let root = PathBuf::from(git::repo_root()?);
        return pre_push::show_config(&root);
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
            no_after_push: false,
            no_hooks: false,
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

    // Phase 6: Pre-push checks (after pull + unstash so we validate the full state)
    if !ctx.cli.no_pre_push
        && let Some(config) = pre_push::load_config(&ctx.preflight.repo_root)?
    {
        pre_push::run_pre_push(&config, ctx.cli.dry_run)?;
    }

    // Phase 7: Stage & Commit
    stage_commit::run(&ctx, &pull_outcome)?;

    // Phase 8: Push (submodules first, then parent)
    push::run(&ctx)?;

    Ok(())
}
