mod claude;
mod git;

use anyhow::{bail, Result};
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

    // Stage everything first so we can get a full picture of changes
    if cli.stage_all {
        if cli.dry_run {
            println!("\n[dry-run] Would stage all changes");
        } else {
            git::stage_all()?;
            println!("\nStaged all changes.");
        }
    }

    if let Some(msg) = cli.message {
        // Manual message: single commit, everything staged
        println!("\nCommit message:\n---\n{msg}\n---");

        if cli.confirm && !prompt_confirm("Proceed with commit?")? {
            println!("Aborted.");
            return Ok(());
        }

        if cli.dry_run {
            println!("[dry-run] Would commit with the message above");
        } else {
            git::commit(&msg)?;
            println!("Committed.");
        }
    } else {
        // Let Claude analyze and split into logical commits
        println!("\nAnalyzing changes with Claude...");
        let files = git::changed_file_list()?;
        let diff = git::diff_for_commit()?;

        let commit_groups = if needs_merge {
            // For merges, use single commit with detailed message
            let message = claude::generate_commit_message(&diff, true)?;
            vec![claude::CommitGroup { message, files }]
        } else {
            claude::plan_commits(&files, &diff)?
        };

        let total = commit_groups.len();
        println!("\nClaude planned {total} commit(s):\n");
        for (i, group) in commit_groups.iter().enumerate() {
            println!("  {}. {} ({})", i + 1, group.message, group.files.join(", "));
        }
        println!();

        if cli.confirm && !prompt_confirm("Proceed with these commits?")? {
            println!("Aborted.");
            return Ok(());
        }

        if cli.dry_run {
            println!("[dry-run] Would create {total} commit(s) as shown above");
        } else {
            for (i, group) in commit_groups.iter().enumerate() {
                // Unstage everything, then stage only this group's files
                if total > 1 {
                    git::unstage_all()?;
                    git::stage_files(&group.files)?;
                }
                // If single commit, everything is already staged

                git::commit(&group.message)?;
                println!("  [{}/{}] Committed: {}", i + 1, total, group.message);
            }
        }
    }

    if !cli.no_push {
        let branch = git::current_branch()?;
        let remote = git::default_remote().unwrap_or_else(|_| "origin".to_string());
        println!("Pushing to {remote}/{branch}...");
        if let Err(push_err) = git::push() {
            eprintln!("Push failed: {push_err}");
            eprintln!("Asking Claude to diagnose and fix...");
            let remote_url = git::remote_url(&remote);
            let fix_commands = claude::fix_push_error(&branch, &remote_url, &push_err.to_string())?;
            if fix_commands.starts_with("UNRECOVERABLE:") {
                bail!("{fix_commands}");
            }
            println!("Claude suggests:\n{fix_commands}\n");
            git::run_commands(&fix_commands)?;
            println!("Pushed (via Claude fix).");
        } else {
            println!("Pushed.");
        }
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
