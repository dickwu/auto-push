mod claude;
mod diff;
mod git;

use anyhow::{Result, bail};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "auto-push",
    version,
    about = "Automate your git workflow: pull, stage, generate AI commit messages, and push — all in one command",
    long_about = "auto-push streamlines your entire git workflow into a single command.\n\n\
        It pulls the latest changes, stages your work, uses the local Claude CLI to \
        analyze your diff and generate meaningful commit messages, then pushes to the remote.\n\n\
        Supports hunk-level commit splitting — Claude can intelligently group related \
        changes into separate, well-described commits.\n\n\
        Requires: git, gh (GitHub CLI), claude (Claude Code CLI, authenticated)"
)]
struct Cli {
    /// Stage all changes before committing (enabled by default)
    #[arg(short = 'a', long, default_value_t = true)]
    stage_all: bool,

    /// Skip pushing to remote after committing
    #[arg(long)]
    no_push: bool,

    /// Review the generated commit message(s) and confirm before committing
    #[arg(short = 'c', long)]
    confirm: bool,

    /// Preview what would happen without making any changes
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Use a custom commit message instead of generating one with Claude
    #[arg(short = 'm', long)]
    message: Option<String>,

    /// Use Claude with --dangerously-skip-permissions to auto-resolve merge conflicts
    #[arg(short = 'f', long)]
    force: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("auto-push v{}", env!("CARGO_PKG_VERSION"));

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
        Ok(git::PullResult::Conflict) => {
            println!("Merge conflicts detected!");
            let conflict_files = git::conflict_files()?;
            if conflict_files.is_empty() {
                bail!("git reported conflicts but no conflicted files found");
            }
            println!(
                "Conflicted files ({}):\n{}",
                conflict_files.len(),
                conflict_files
                    .iter()
                    .map(|f| format!("  - {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            if !cli.force {
                println!("\nLaunching Claude to resolve conflicts (interactive)...");
                println!("Tip: use --force (-f) to auto-resolve without prompts");
            } else {
                println!("\nLaunching Claude to auto-resolve conflicts (--force mode)...");
            }

            claude::resolve_conflicts(&conflict_files, cli.force)?;

            if git::has_conflicts()? {
                eprintln!("Conflicts remain after Claude resolution. Aborting merge.");
                git::abort_merge()?;
                bail!("unresolved merge conflicts — please resolve manually");
            }

            println!("All conflicts resolved by Claude.");
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
        let raw_diff = git::diff_for_commit()?;
        let hunks = diff::parse_diff(&raw_diff);

        if hunks.is_empty() {
            bail!("no diff hunks found to commit");
        }

        let commit_groups = if needs_merge {
            // For merges, use single commit with detailed message
            let message = claude::generate_commit_message(&raw_diff, true)?;
            let all_ids: Vec<usize> = hunks.iter().map(|h| h.id).collect();
            vec![claude::CommitGroup {
                message,
                hunks: all_ids,
            }]
        } else {
            let formatted = diff::format_hunks_for_prompt(&hunks);
            let valid_ids: Vec<usize> = hunks.iter().map(|h| h.id).collect();
            claude::plan_commits(&formatted, &valid_ids)?
        };

        // Deduplicate: each hunk ID only appears in its first commit group
        let commit_groups = dedup_commit_groups(commit_groups);

        let total = commit_groups.len();
        println!("\nClaude planned {total} commit(s):\n");
        for (i, group) in commit_groups.iter().enumerate() {
            let files = resolve_files(&hunks, &group.hunks);
            println!("  {}. {} ({})", i + 1, group.message, files.join(", "));
        }
        println!();

        if cli.confirm && !prompt_confirm("Proceed with these commits?")? {
            println!("Aborted.");
            return Ok(());
        }

        if cli.dry_run {
            println!("[dry-run] Would create {total} commit(s) as shown above");
        } else {
            commit_groups_with_hunks(&commit_groups, &hunks, total)?;
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

/// Execute commit groups using hunk-level staging via `git apply --cached`.
/// Falls back to file-level staging if patch application fails.
fn commit_groups_with_hunks(
    groups: &[claude::CommitGroup],
    all_hunks: &[diff::DiffHunk],
    total: usize,
) -> Result<()> {
    for (i, group) in groups.iter().enumerate() {
        let group_hunks: Vec<&diff::DiffHunk> = group
            .hunks
            .iter()
            .filter_map(|id| all_hunks.iter().find(|h| h.id == *id))
            .collect();

        if total > 1 {
            git::unstage_all()?;

            // Separate patchable hunks from file-level entries (binary, rename-only)
            let patchable: Vec<&diff::DiffHunk> = group_hunks
                .iter()
                .filter(|h| h.is_patchable())
                .copied()
                .collect();
            let file_level: Vec<&diff::DiffHunk> = group_hunks
                .iter()
                .filter(|h| !h.is_patchable())
                .copied()
                .collect();

            // Stage patchable hunks via git apply
            if !patchable.is_empty() {
                let patch = diff::hunks_to_patch(&patchable);
                if let Err(e) = git::apply_patch_to_index(&patch) {
                    eprintln!(
                        "  Warning: hunk-level staging failed ({e}), falling back to file-level"
                    );
                    let files = diff::files_from_hunks(patchable.into_iter());
                    git::stage_files(&files)?;
                }
            }

            // Stage file-level entries via git add
            if !file_level.is_empty() {
                let files = diff::files_from_hunks(file_level.into_iter());
                git::stage_files(&files)?;
            }
        }
        // If single commit, everything is already staged

        git::commit(&group.message)?;
        println!("  [{}/{}] Committed: {}", i + 1, total, group.message);
    }
    Ok(())
}

/// Ensure each hunk ID appears in only one commit group (the first one that claims it).
/// Drops any groups that end up with no hunks after dedup.
fn dedup_commit_groups(groups: Vec<claude::CommitGroup>) -> Vec<claude::CommitGroup> {
    let mut seen = std::collections::HashSet::new();
    groups
        .into_iter()
        .filter_map(|group| {
            let unique_hunks: Vec<usize> = group
                .hunks
                .into_iter()
                .filter(|id| seen.insert(*id))
                .collect();
            if unique_hunks.is_empty() {
                None
            } else {
                Some(claude::CommitGroup {
                    message: group.message,
                    hunks: unique_hunks,
                })
            }
        })
        .collect()
}

/// Resolve hunk IDs to their unique file paths for display.
fn resolve_files(all_hunks: &[diff::DiffHunk], hunk_ids: &[usize]) -> Vec<String> {
    let matched = hunk_ids
        .iter()
        .filter_map(|id| all_hunks.iter().find(|h| h.id == *id));
    diff::files_from_hunks(matched)
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
