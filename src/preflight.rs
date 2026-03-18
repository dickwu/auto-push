use crate::context::PreflightResult;
use crate::git;
use anyhow::{Result, bail};
use std::path::PathBuf;

pub fn check() -> Result<PreflightResult> {
    git::ensure_git_repo()?;

    if git::is_detached_head()? {
        bail!("detached HEAD -- checkout a branch first");
    }

    if !git::has_remote()? {
        bail!(
            "no git remote configured.\n\
             Add one with: git remote add origin <url>"
        );
    }

    let branch = git::current_branch()?;
    let remote = git::default_remote()?;
    let has_upstream = git::has_upstream()?;
    let is_shallow = git::is_shallow()?;
    let has_lfs = git::has_lfs()?;

    if is_shallow {
        eprintln!(
            "[preflight] Warning: shallow clone detected -- some operations may behave differently"
        );
    }

    if has_lfs {
        let (_, _, success) = git::run_git_check(&["lfs", "version"])?;
        if !success {
            eprintln!("[preflight] Warning: LFS files detected but git-lfs not installed");
        }
    }

    // Check for leftover unmerged paths
    let conflicts = git::conflict_files()?;
    if !conflicts.is_empty() {
        bail!(
            "unresolved conflicts from a previous merge.\n\
             Resolve them before running auto-push:\n{}",
            conflicts
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let has_submodules = git::has_gitmodules()?;
    let submodule_paths = if has_submodules {
        git::submodule_paths()?
    } else {
        vec![]
    };

    let repo_root = PathBuf::from(git::repo_root()?);

    let info_parts: Vec<String> = [
        if has_submodules {
            Some(format!("{} submodule(s)", submodule_paths.len()))
        } else {
            None
        },
        if has_lfs {
            Some("LFS detected".to_string())
        } else {
            None
        },
        if is_shallow {
            Some("shallow clone".to_string())
        } else {
            None
        },
        if !has_upstream {
            Some("no upstream".to_string())
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let info = if info_parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", info_parts.join(", "))
    };

    println!("[preflight] {branch} -> {remote}{info}");

    Ok(PreflightResult {
        repo_root,
        branch,
        remote,
        is_shallow,
        has_submodules,
        submodule_paths,
        has_lfs,
        has_upstream,
    })
}
