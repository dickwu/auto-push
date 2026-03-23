use crate::context::Context;
use crate::generate;
use crate::git;
use anyhow::{Result, bail};

pub enum PullOutcome {
    Skipped,
    AlreadyUpToDate,
    FastForward,
    Merged,
}

impl PullOutcome {
    pub fn needs_merge(&self) -> bool {
        matches!(self, PullOutcome::Merged)
    }
}

pub fn run(ctx: &Context) -> Result<PullOutcome> {
    if ctx.cli.no_pull {
        return Ok(PullOutcome::Skipped);
    }

    if !ctx.preflight.has_upstream {
        println!("[pull] No upstream tracking branch -- skipping pull");
        return Ok(PullOutcome::Skipped);
    }

    let pull_result = if ctx.cli.rebase {
        git::pull_rebase()
    } else {
        git::pull()
    };

    match pull_result {
        Ok(git::PullResult::AlreadyUpToDate) => {
            println!("[pull] Already up to date");
            Ok(PullOutcome::AlreadyUpToDate)
        }
        Ok(git::PullResult::FastForward) => {
            println!("[pull] Fast-forwarded to latest");
            Ok(PullOutcome::FastForward)
        }
        Ok(git::PullResult::Merged) => {
            println!("[pull] Merged remote changes");
            Ok(PullOutcome::Merged)
        }
        Ok(git::PullResult::Conflict) => {
            if ctx.cli.rebase {
                resolve_rebase_conflicts(ctx)?;
            } else {
                resolve_merge_conflicts(ctx)?;
            }
            Ok(PullOutcome::Merged)
        }
        Err(e) => {
            eprintln!("[pull] Warning: git pull failed: {e}");
            eprintln!("[pull] Continuing with local changes...");
            Ok(PullOutcome::Skipped)
        }
    }
}

fn resolve_merge_conflicts(ctx: &Context) -> Result<()> {
    let conflict_files = git::conflict_files()?;
    if conflict_files.is_empty() {
        bail!("git reported conflicts but no conflicted files found");
    }

    println!("[pull] Merge conflicts in {} file(s)", conflict_files.len());

    let gen_config = &ctx.app_config.generate;
    generate::resolve_conflicts(&conflict_files, ctx.cli.force, gen_config)?;

    if git::has_conflicts()? {
        eprintln!("[pull] Conflicts remain after resolution. Aborting merge.");
        git::abort_merge()?;
        bail!("unresolved merge conflicts -- please resolve manually");
    }

    println!("[pull] All conflicts resolved");
    Ok(())
}

fn resolve_rebase_conflicts(ctx: &Context) -> Result<()> {
    const MAX_REBASE_ITERATIONS: usize = 10;
    let gen_config = &ctx.app_config.generate;

    for iteration in 0..MAX_REBASE_ITERATIONS {
        let conflict_files = git::conflict_files()?;
        if conflict_files.is_empty() {
            break;
        }

        println!(
            "[pull] Rebase conflict (step {}) in {} file(s)",
            iteration + 1,
            conflict_files.len()
        );

        generate::resolve_conflicts(&conflict_files, ctx.cli.force, gen_config)?;

        if git::has_conflicts()? {
            eprintln!("[pull] Could not resolve rebase conflicts. Aborting rebase.");
            git::rebase_abort()?;
            bail!("unresolved rebase conflicts -- resolve manually or use merge-based pull");
        }

        let continued = git::rebase_continue()?;
        if continued {
            println!("[pull] Rebase conflicts resolved");
            return Ok(());
        }
    }

    eprintln!(
        "[pull] Rebase exceeded {MAX_REBASE_ITERATIONS} conflict resolution steps. Aborting."
    );
    git::rebase_abort()?;
    bail!("rebase too conflicted -- resolve manually or use merge-based pull");
}
