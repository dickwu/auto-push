use crate::claude;
use crate::context::Context;
use crate::diff;
use crate::git;
use crate::pull::PullOutcome;
use anyhow::{Result, bail};
use std::collections::HashSet;

/// Main entry point: stage and commit changes.
pub fn run(ctx: &Context, pull_outcome: &PullOutcome) -> Result<()> {
    if !git::has_changes()? {
        println!("[stage] No changes to commit");
        return Ok(());
    }

    if ctx.cli.stage_all {
        if ctx.cli.dry_run {
            println!("[stage] [dry-run] Would stage all changes");
        } else {
            git::stage_all()?;
            let stat = git::run_git(&["diff", "--cached", "--stat"])?;
            let n = count_staged_files(&stat);
            println!("[stage] Staged {n} file(s)");
        }
    }

    if let Some(ref msg) = ctx.cli.message.clone() {
        commit_manual(ctx, msg)?;
    } else {
        commit_with_claude(ctx, pull_outcome)?;
    }

    Ok(())
}

fn commit_manual(ctx: &Context, msg: &str) -> Result<()> {
    println!("\n[commit] Commit message:\n---\n{msg}\n---");

    if ctx.cli.confirm && !prompt_confirm("Proceed with commit?")? {
        println!("[commit] Aborted.");
        return Ok(());
    }

    if ctx.cli.dry_run {
        println!("[commit] [dry-run] Would commit with the message above");
    } else {
        git::commit(msg)?;
        println!("[commit] Committed.");
    }

    Ok(())
}

fn commit_with_claude(ctx: &Context, pull_outcome: &PullOutcome) -> Result<()> {
    println!("\n[commit] Analyzing changes with Claude...");
    let raw_diff = git::diff_for_commit()?;
    let hunks = diff::parse_diff(&raw_diff);

    if hunks.is_empty() {
        bail!("no diff hunks found to commit");
    }

    let commit_groups = if pull_outcome.needs_merge() {
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

    let commit_groups = dedup_commit_groups(commit_groups);

    // Ensure all hunks are covered — Claude may omit some IDs from its plan
    let commit_groups = ensure_all_hunks_covered(commit_groups, &hunks);

    let total = commit_groups.len();
    println!("\n[commit] {total} commit(s) planned by Claude:\n");

    for (i, group) in commit_groups.iter().enumerate() {
        let files = resolve_files(&hunks, &group.hunks);
        println!("  {}. {} ({})", i + 1, group.message, files.join(", "));
    }
    println!();

    if ctx.cli.confirm && !prompt_confirm("Proceed with these commits?")? {
        println!("[commit] Aborted.");
        return Ok(());
    }

    if ctx.cli.dry_run {
        println!("[commit] [dry-run] Would create {total} commit(s) as shown above");
    } else {
        commit_groups_with_hunks(&commit_groups, &hunks, total)?;
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

            if !file_level.is_empty() {
                let files = diff::files_from_hunks(file_level.into_iter());
                git::stage_files(&files)?;
            }
        }

        git::commit(&group.message)?;
        println!("  [{}/{}] Committed: {}", i + 1, total, group.message);
    }
    Ok(())
}

/// Ensure each hunk ID appears in only one commit group (the first that claims it).
/// Drops any groups that end up with no hunks after dedup.
fn dedup_commit_groups(groups: Vec<claude::CommitGroup>) -> Vec<claude::CommitGroup> {
    let mut seen = HashSet::new();
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

/// If Claude's commit plan doesn't cover all hunk IDs, add a catch-all group
/// for the remaining hunks so no changes are silently dropped.
fn ensure_all_hunks_covered(
    mut groups: Vec<claude::CommitGroup>,
    all_hunks: &[diff::DiffHunk],
) -> Vec<claude::CommitGroup> {
    let covered: HashSet<usize> = groups
        .iter()
        .flat_map(|g| g.hunks.iter().copied())
        .collect();
    let missing: Vec<usize> = all_hunks
        .iter()
        .map(|h| h.id)
        .filter(|id| !covered.contains(id))
        .collect();

    if !missing.is_empty() {
        let files = resolve_files(all_hunks, &missing);
        eprintln!(
            "[commit] Warning: Claude's plan missed {} hunk(s) in {}. Adding catch-all commit.",
            missing.len(),
            files.join(", ")
        );
        groups.push(claude::CommitGroup {
            message: "chore: stage remaining changes".to_string(),
            hunks: missing,
        });
    }

    groups
}

/// Resolve hunk IDs to their unique file paths for display.
fn resolve_files(all_hunks: &[diff::DiffHunk], hunk_ids: &[usize]) -> Vec<String> {
    let matched = hunk_ids
        .iter()
        .filter_map(|id| all_hunks.iter().find(|h| h.id == *id));
    diff::files_from_hunks(matched)
}

/// Count the number of staged files from `git diff --cached --stat` output.
pub fn count_staged_files(staged_stat: &str) -> usize {
    staged_stat
        .lines()
        .filter(|l| !l.trim().is_empty() && l.contains('|'))
        .count()
}

/// Prompt the user with a yes/no question. Defaults to No.
pub fn prompt_confirm(question: &str) -> Result<bool> {
    use std::io::{self, Write};

    print!("{question} [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_staged_files_empty() {
        assert_eq!(count_staged_files(""), 0);
    }

    #[test]
    fn test_count_staged_files_typical() {
        let stat = " src/main.rs | 10 ++++------\n src/lib.rs  |  5 ++---\n 2 files changed, 15 insertions(+), 5 deletions(-)";
        // Two non-summary lines
        assert_eq!(count_staged_files(stat), 2);
    }

    #[test]
    fn test_dedup_commit_groups_removes_duplicates() {
        let groups = vec![
            claude::CommitGroup {
                message: "feat: first".to_string(),
                hunks: vec![1, 2, 3],
            },
            claude::CommitGroup {
                message: "feat: second".to_string(),
                hunks: vec![2, 4],
            },
        ];
        let deduped = dedup_commit_groups(groups);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].hunks, vec![1, 2, 3]);
        // Hunk 2 was already claimed, only 4 remains
        assert_eq!(deduped[1].hunks, vec![4]);
    }

    #[test]
    fn test_ensure_all_hunks_covered_adds_missing() {
        let hunks = vec![
            diff::DiffHunk {
                id: 1,
                file_path: "a.rs".to_string(),
                file_header: String::new(),
                hunk_header: "@@ -1 +1 @@".to_string(),
                body: String::new(),
            },
            diff::DiffHunk {
                id: 2,
                file_path: "b.rs".to_string(),
                file_header: String::new(),
                hunk_header: "@@ -1 +1 @@".to_string(),
                body: String::new(),
            },
            diff::DiffHunk {
                id: 3,
                file_path: "c.rs".to_string(),
                file_header: String::new(),
                hunk_header: "@@ -1 +1 @@".to_string(),
                body: String::new(),
            },
        ];
        let groups = vec![claude::CommitGroup {
            message: "feat: only covers hunk 1".to_string(),
            hunks: vec![1],
        }];
        let result = ensure_all_hunks_covered(groups, &hunks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].hunks, vec![1]);
        assert_eq!(result[1].hunks, vec![2, 3]);
        assert_eq!(result[1].message, "chore: stage remaining changes");
    }

    #[test]
    fn test_ensure_all_hunks_covered_noop_when_complete() {
        let hunks = vec![diff::DiffHunk {
            id: 1,
            file_path: "a.rs".to_string(),
            file_header: String::new(),
            hunk_header: "@@ -1 +1 @@".to_string(),
            body: String::new(),
        }];
        let groups = vec![claude::CommitGroup {
            message: "feat: covers everything".to_string(),
            hunks: vec![1],
        }];
        let result = ensure_all_hunks_covered(groups, &hunks);
        assert_eq!(result.len(), 1); // no catch-all added
    }

    #[test]
    fn test_dedup_commit_groups_drops_empty() {
        let groups = vec![
            claude::CommitGroup {
                message: "feat: first".to_string(),
                hunks: vec![1],
            },
            claude::CommitGroup {
                message: "feat: second".to_string(),
                hunks: vec![1],
            },
        ];
        let deduped = dedup_commit_groups(groups);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].message, "feat: first");
    }
}
