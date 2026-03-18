use crate::claude;
use crate::context::Context;
use crate::git;
use anyhow::{Result, bail};

/// Main entry point: sync submodules — update, commit dirty changes, stage pointer updates.
pub fn sync(ctx: &Context) -> Result<()> {
    if !ctx.preflight.has_submodules || ctx.cli.no_submodules {
        return Ok(());
    }

    println!("[submodule] Initializing and updating submodules...");
    git::submodule_update_init()?;

    for path in &ctx.preflight.submodule_paths.clone() {
        if git::submodule_is_dirty(path)? {
            println!("[submodule] {path}: dirty — processing...");
            if let Err(e) = process_dirty_submodule(ctx, path) {
                eprintln!("[submodule] {path}: error during processing: {e}");
            }
        }
    }

    // Stage any updated submodule pointer(s) in the parent repo
    let (stat, _, _) = git::run_git_check(&["submodule", "status", "--recursive"])?;
    let dirty_pointers: Vec<&str> = stat
        .lines()
        .filter(|l| l.starts_with('+') || l.starts_with('-'))
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();

    if !dirty_pointers.is_empty() {
        git::stage_files(
            &dirty_pointers
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )?;
        println!(
            "[submodule] Staged pointer update(s) for: {}",
            dirty_pointers.join(", ")
        );
    }

    Ok(())
}

fn process_dirty_submodule(ctx: &Context, path: &str) -> Result<()> {
    ensure_on_branch(path)?;

    // Stash working tree changes so we can pull cleanly
    let stashed = if git::submodule_is_dirty(path)? {
        let (_, _, ok) = git::run_git_check(&[
            "-C",
            path,
            "stash",
            "push",
            "--include-untracked",
            "-m",
            "auto-push: submodule pre-pull stash",
        ])?;
        ok
    } else {
        false
    };

    // Pull if there is an upstream
    let (_, _, has_upstream) =
        git::run_git_check(&["-C", path, "rev-parse", "--abbrev-ref", "@{u}"])?;
    if has_upstream {
        let (_, pull_err, pull_ok) = git::run_git_check(&["-C", path, "pull"])?;
        if !pull_ok {
            eprintln!("[submodule] {path}: pull failed: {pull_err}");
        }
    }

    // Restore stash
    if stashed {
        let (_, stash_err, stash_ok) = git::run_git_check(&["-C", path, "stash", "pop"])?;
        if !stash_ok {
            eprintln!("[submodule] {path}: stash pop failed: {stash_err}");
        }
    }

    // Stage all changes
    let (_, _, stage_ok) = git::run_git_check(&["-C", path, "add", "-A"])?;
    if !stage_ok {
        bail!("[submodule] {path}: failed to stage changes");
    }

    // Check if anything is staged
    let (_, _, nothing_staged) = git::run_git_check(&["-C", path, "diff", "--cached", "--quiet"])?;
    if nothing_staged {
        println!("[submodule] {path}: nothing to commit after staging");
        return Ok(());
    }

    // Generate commit message via Claude, fall back to a sensible default
    let diff = {
        let (d, _, _) = git::run_git_check(&["-C", path, "diff", "--cached"])?;
        d
    };

    let commit_message = if diff.is_empty() || ctx.cli.dry_run {
        "chore: update submodule changes".to_string()
    } else {
        match claude::generate_commit_message(&diff, false) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!(
                    "[submodule] {path}: Claude commit message generation failed ({e}), using default"
                );
                "chore: update submodule changes".to_string()
            }
        }
    };

    if ctx.cli.dry_run {
        println!("[submodule] [dry-run] Would commit {path} with: {commit_message}");
        return Ok(());
    }

    let (_, commit_err, commit_ok) =
        git::run_git_check(&["-C", path, "commit", "-m", &commit_message])?;
    if !commit_ok {
        bail!("[submodule] {path}: commit failed: {commit_err}");
    }

    println!("[submodule] {path}: committed: {commit_message}");
    Ok(())
}

fn ensure_on_branch(path: &str) -> Result<()> {
    let (_, _, on_branch) = git::run_git_check(&["-C", path, "symbolic-ref", "-q", "HEAD"])?;
    if on_branch {
        return Ok(());
    }

    // Detached HEAD — try to find the tracking branch
    match git::submodule_tracking_branch(path)? {
        Some(branch) => {
            let (_, checkout_err, checkout_ok) =
                git::run_git_check(&["-C", path, "checkout", &branch])?;
            if !checkout_ok {
                eprintln!(
                    "[submodule] {path}: could not checkout branch '{branch}': {checkout_err}"
                );
                eprintln!("[submodule] {path}: skipping (detached HEAD)");
            }
        }
        None => {
            eprintln!(
                "[submodule] {path}: detached HEAD and no tracking branch configured — skipping"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_submodule_dirty_pointer_filter() {
        // Lines starting with '+' or '-' indicate changed submodule pointers
        let status = " abc123 path/to/sub (v1.0)\n+def456 path/to/other (v2.0)\n-111111 path/gone";
        let dirty: Vec<&str> = status
            .lines()
            .filter(|l| l.starts_with('+') || l.starts_with('-'))
            .filter_map(|l| l.split_whitespace().nth(1))
            .collect();
        assert_eq!(dirty, vec!["path/to/other", "path/gone"]);
    }
}
