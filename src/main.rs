mod config;
mod context;
mod diff;
mod generate;
mod git;
mod hooks;
mod preflight;
mod pull;
mod push;
mod stage_commit;
mod stash;
mod submodule;
mod template;
mod vars;

use anyhow::{Result, bail};
use clap::Parser;
use config::ProviderConfig;
use context::CliFlags;

#[derive(Parser)]
#[command(
    name = "auto-push",
    version,
    about = "Automate your git workflow: pull, stage, generate AI commit messages, and push — all in one command",
    long_about = "auto-push streamlines your entire git workflow into a single command.\n\n\
        It pulls the latest changes, stages your work, uses an AI CLI to \
        analyze your diff and generate meaningful commit messages, then pushes to the remote.\n\n\
        Supports hunk-level commit splitting, submodule handling, auto-stash, \
        rebase, and intelligent push recovery.\n\n\
        Supports multiple AI providers: Claude (default), Codex, Ollama, or any custom CLI.\n\n\
        Requires: git, and an AI CLI (claude, codex, or ollama)"
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

    /// Skip AI generation (requires -m to provide a commit message)
    #[arg(long)]
    no_generate: bool,

    /// Review and confirm before each action
    #[arg(short = 'c', long)]
    confirm: bool,

    /// Preview what would happen without making any changes
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Use a custom commit message instead of generating one with AI
    #[arg(short = 'm', long)]
    message: Option<String>,

    /// Auto-resolve merge conflicts without prompts
    #[arg(short = 'f', long)]
    force: bool,

    /// Pull with rebase instead of merge
    #[arg(short = 'r', long)]
    rebase: bool,

    /// Override the AI provider for this run (e.g. --provider codex)
    #[arg(long)]
    provider: Option<String>,

    /// Show the merged config (global + local + branch) and exit
    #[arg(long)]
    show_config: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("auto-push v{}", env!("CARGO_PKG_VERSION"));

    // Phase 1: Preflight
    let preflight_result = preflight::check()?;

    // Show config and exit
    if cli.show_config {
        return config::show_config(&preflight_result.repo_root, &preflight_result.branch);
    }

    // Validate --no-generate requires -m
    if cli.no_generate && cli.message.is_none() {
        bail!("No commit message: use -m to provide one or remove --no-generate.");
    }

    // Phase 2: Load config (auto-inits if missing)
    let mut app_config = config::load(&preflight_result.repo_root, &preflight_result.branch)?;

    // Apply --provider CLI override
    if let Some(ref provider_name) = cli.provider {
        app_config.generate.provider = Some(ProviderConfig::Preset(provider_name.clone()));
        // Reset structured_output to preset default
        app_config.generate.structured_output = None;
    }

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
            no_generate: cli.no_generate,
            confirm: cli.confirm,
            dry_run: cli.dry_run,
            message: cli.message,
            force: cli.force,
            rebase: cli.rebase,
            provider_override: cli.provider,
        },
        app_config,
    };

    // Phase 3: Stash (protect dirty tree before pull)
    let stash_result = stash::auto_stash(&ctx)?;

    // Phase 4: Pull
    let pull_outcome = pull::run(&ctx)?;

    // Phase 5: Submodule Sync (after pull so we have latest .gitmodules)
    submodule::sync(&ctx)?;

    // Phase 6: Unstash (restore changes for commit)
    stash::auto_unstash(&stash_result)?;

    // Phase 7: Pre-push hooks
    if !ctx.cli.no_hooks && !ctx.cli.no_pre_push && !ctx.app_config.pre_push.is_empty() {
        let mut template_ctx = hooks::TemplateContext {
            branch: ctx.preflight.branch.clone(),
            remote: ctx.preflight.remote.clone(),
            commit_hash: git::run_git(&["rev-parse", "HEAD"]).unwrap_or_else(|_| {
                eprintln!("[hooks] Warning: could not resolve HEAD commit hash");
                String::new()
            }),
            commit_summary: String::new(),
            command_outputs: std::collections::HashMap::new(),
        };
        hooks::run_phase(
            hooks::HookPhase::PrePush,
            &ctx.app_config.pre_push,
            &mut template_ctx,
            ctx.cli.dry_run,
            ctx.cli.force,
        )?;
    }

    // Phase 8: Stage & Commit
    stage_commit::run(&ctx, &pull_outcome)?;

    // Phase 9: Push (submodules first, then parent)
    push::run(&ctx)?;

    // Phase 10: After-push hooks (only if push actually happened)
    if !ctx.cli.no_push
        && !ctx.cli.no_hooks
        && !ctx.cli.no_after_push
        && !ctx.app_config.after_push.is_empty()
    {
        let mut template_ctx = hooks::TemplateContext {
            branch: ctx.preflight.branch.clone(),
            remote: ctx.preflight.remote.clone(),
            commit_hash: git::run_git(&["rev-parse", "HEAD"]).unwrap_or_else(|_| {
                eprintln!("[hooks] Warning: could not resolve HEAD commit hash");
                String::new()
            }),
            commit_summary: git::run_git(&["log", "-1", "--format=%s"]).unwrap_or_else(|_| {
                eprintln!("[hooks] Warning: could not resolve commit summary");
                String::new()
            }),
            command_outputs: std::collections::HashMap::new(),
        };
        if let Err(e) = hooks::run_phase(
            hooks::HookPhase::AfterPush,
            &ctx.app_config.after_push,
            &mut template_ctx,
            ctx.cli.dry_run,
            ctx.cli.force,
        ) {
            eprintln!("[after_push] Warning: {e}");
        }
    }

    Ok(())
}
