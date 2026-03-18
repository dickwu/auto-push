use crate::context::Context;
use crate::git;
use anyhow::Result;

pub enum StashResult {
    NotNeeded,
    Stashed,
}

pub fn auto_stash(ctx: &Context) -> Result<StashResult> {
    if ctx.cli.no_stash {
        if git::has_dirty_working_tree()? && !ctx.cli.no_pull && ctx.preflight.has_upstream {
            anyhow::bail!(
                "uncommitted changes -- commit or stash manually before running auto-push\n\
                 Tip: remove --no-stash to let auto-push handle this automatically"
            );
        }
        return Ok(StashResult::NotNeeded);
    }

    if !ctx.cli.no_pull && ctx.preflight.has_upstream && git::has_dirty_working_tree()? {
        let count = dirty_file_count()?;
        git::stash_push()?;
        println!("[stash] Saved {count} uncommitted change(s)");
        Ok(StashResult::Stashed)
    } else {
        Ok(StashResult::NotNeeded)
    }
}

pub fn auto_unstash(stash: &StashResult) -> Result<()> {
    if matches!(stash, StashResult::NotNeeded) {
        return Ok(());
    }

    let clean_pop = git::stash_pop()?;
    if clean_pop {
        println!("[stash] Restored stashed changes for commit");
    } else {
        eprintln!(
            "[stash] Warning: stash could not be cleanly restored.\n\
             Your changes are in `git stash list`. Run `git stash pop` manually."
        );
    }
    Ok(())
}

fn dirty_file_count() -> Result<usize> {
    let (stdout, _, _) = git::run_git_check(&["status", "--porcelain"])?;
    Ok(stdout.lines().filter(|l| !l.is_empty()).count())
}
