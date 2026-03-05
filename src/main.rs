mod claude;
mod git;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "auto-push",
    version,
    about = "Auto-stage, generate commit message with Claude, and push"
)]
struct Cli {
    /// Stage all changes before committing
    #[arg(short = 'a', long, default_value_t = true)]
    stage_all: bool,

    /// Skip pushing to remote
    #[arg(long)]
    no_push: bool,

    /// Show the generated message and ask for confirmation before committing
    #[arg(short = 'c', long)]
    confirm: bool,

    /// Dry run: show what would happen without making changes
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Override the generated commit message
    #[arg(short = 'm', long)]
    message: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    git::ensure_git_repo()?;

    // Pull first to sync with remote
    println!("Pulling from remote...");
    let pull_result = git::pull();
    let needs_merge = match &pull_result {
        Ok(git::PullResult::AlreadyUpToDate) => {
            println!("Already up to date.");
            false
        }
        Ok(git::PullResult::FastForward) => {
            println!("Fast-forwarded to latest.");
            false
        }
        Ok(git::PullResult::Merged) => {
            println!("Merged remote changes.");
            true
        }
        Err(e) => {
            eprintln!("Warning: git pull failed: {e}");
            eprintln!("Continuing with local changes...");
            false
        }
    };

    if !git::has_changes()? {
        println!("No changes to commit.");
        return Ok(());
    }

    print_status()?;

    if cli.stage_all {
        if cli.dry_run {
            println!("\n[dry-run] Would stage all changes");
        } else {
            git::stage_all()?;
            println!("\nStaged all changes.");
        }
    }

    let commit_message = match cli.message {
        Some(msg) => msg,
        None => {
            println!("\nGenerating commit message with Claude...");
            let diff = git::diff_for_commit()?;
            claude::generate_commit_message(&diff, needs_merge)?
        }
    };

    println!("\nCommit message:\n---\n{commit_message}\n---");

    if cli.confirm && !prompt_confirm("Proceed with commit?")? {
        println!("Aborted.");
        return Ok(());
    }

    if cli.dry_run {
        println!("[dry-run] Would commit with the message above");
        if !cli.no_push {
            println!("[dry-run] Would push to remote");
        }
        return Ok(());
    }

    git::commit(&commit_message)?;
    println!("Committed.");

    if !cli.no_push {
        let branch = git::current_branch()?;
        println!("Pushing to origin/{branch}...");
        git::push()?;
        println!("Pushed.");
    }

    Ok(())
}

fn print_status() -> Result<()> {
    let status = git::status()?;

    if !status.staged.is_empty() {
        println!("Staged:\n{}", status.staged);
    }
    if !status.unstaged.is_empty() {
        println!("Unstaged:\n{}", status.unstaged);
    }
    if !status.untracked.is_empty() {
        println!("Untracked: {} file(s)", status.untracked.len());
    }

    Ok(())
}

fn prompt_confirm(question: &str) -> Result<bool> {
    use std::io::{self, Write};

    print!("{question} [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}
