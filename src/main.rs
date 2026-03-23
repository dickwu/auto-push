mod config;
mod context;
mod diff;
mod generate;
mod git;
mod pipeline;
mod preflight;
mod template;
mod vars;

use anyhow::{Result, bail};
use clap::Parser;
use config::ProviderConfig;

fn parse_var_override(s: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid --var format: '{s}'. Expected key=value."));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

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

    /// Skip specific pipeline commands by name (repeatable)
    #[arg(long, action = clap::ArgAction::Append)]
    skip: Vec<String>,

    /// Override or add template variables (repeatable, format: key=value)
    #[arg(long = "var", value_parser = parse_var_override, action = clap::ArgAction::Append)]
    var_overrides: Vec<(String, String)>,
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

    // Build CLI flags
    let mut cli_flags = context::CliFlags {
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
        skip: cli.skip,
        var_overrides: cli.var_overrides,
    };

    // Apply deprecation flags (--no-pull -> --skip pull, etc.)
    context::apply_deprecation_flags(&mut cli_flags);

    // Phase 3: Build template vars (static built-ins + user vars + CLI overrides)
    let mut template_vars = vars::build_static_vars(
        &preflight_result.branch,
        &preflight_result.remote,
        &git::remote_url(&preflight_result.remote),
        preflight_result.repo_root.to_str().unwrap_or("."),
        &app_config.generate,
    );

    // Add user vars from config
    for (k, v) in &app_config.vars {
        template_vars.insert(k.clone(), v.clone());
    }

    // Apply --var CLI overrides
    context::apply_var_overrides(&mut template_vars, &cli_flags.var_overrides)?;

    // Pre-register -m message as commit_message
    if let Some(ref msg) = cli_flags.message {
        template_vars.insert("commit_message".to_string(), msg.clone());
    }

    // Phase 4: Resolve pipeline
    let pipeline_commands = app_config.pipeline.unwrap_or_default();

    // Phase 5: Create lazy var resolver
    let mut lazy_resolver = vars::LazyVarResolver::new(app_config.generate.max_diff_bytes);

    // Phase 6: Execute pipeline
    pipeline::run_pipeline(
        &pipeline_commands,
        &mut template_vars,
        &mut lazy_resolver,
        &cli_flags.skip,
        cli_flags.dry_run,
        cli_flags.force,
        cli_flags.confirm,
    )?;

    Ok(())
}
