use crate::context::Context;
use crate::generate;
use crate::git;
use anyhow::{Result, bail};
use std::thread;
use std::time::Duration;

/// Main entry point: push the current repo (and submodules if applicable).
pub fn run(ctx: &Context) -> Result<()> {
    if ctx.cli.no_push {
        return Ok(());
    }

    if ctx.preflight.has_submodules && !ctx.cli.no_submodules {
        push_submodules(ctx)?;
    }

    let remote = &ctx.preflight.remote;
    let branch = &ctx.preflight.branch;
    let set_upstream = !ctx.preflight.has_upstream;
    push_repo(ctx, remote, branch, set_upstream)?;

    Ok(())
}

fn push_submodules(ctx: &Context) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();

    for path in &ctx.preflight.submodule_paths {
        let has_unpushed = check_has_unpushed(path)?;
        if !has_unpushed {
            continue;
        }

        if let Err(e) = push_submodule(path) {
            eprintln!("[push] Submodule {path} push failed: {e}");
            failures.push(path.clone());
        } else {
            println!("[push] Submodule {path}: pushed");
        }
    }

    if !failures.is_empty() {
        bail!("failed to push submodule(s): {}", failures.join(", "));
    }

    Ok(())
}

fn check_has_unpushed(path: &str) -> Result<bool> {
    let (stdout, _, _) = git::run_git_check(&["-C", path, "log", "--oneline", "@{u}..HEAD"])?;
    Ok(!stdout.is_empty())
}

fn push_submodule(path: &str) -> Result<()> {
    let (branch, _, success) =
        git::run_git_check(&["-C", path, "rev-parse", "--abbrev-ref", "HEAD"])?;
    if !success || branch == "HEAD" {
        bail!("submodule {path} is in detached HEAD state — cannot push");
    }

    let has_upstream = {
        let (_, _, ok) = git::run_git_check(&["-C", path, "rev-parse", "--abbrev-ref", "@{u}"])?;
        ok
    };

    let (_, stderr, push_ok) = if has_upstream {
        git::run_git_check(&["-C", path, "push", "origin", &branch])?
    } else {
        git::run_git_check(&["-C", path, "push", "-u", "origin", &branch])?
    };

    if !push_ok {
        bail!("push failed: {stderr}");
    }

    Ok(())
}

fn push_repo(ctx: &Context, remote: &str, branch: &str, set_upstream: bool) -> Result<()> {
    let result = git::push_to(remote, branch, set_upstream);

    match result {
        Ok(_) => {
            println!("[push] Pushed to {remote}/{branch}");
            Ok(())
        }
        Err(ref e) => {
            let msg = e.to_string();

            if msg.contains("protected branch")
                || msg.contains("GH006")
                || msg.contains("push to a protected branch")
            {
                bail!("Branch is protected. Create a PR instead.");
            }

            if msg.contains("Permission denied")
                || msg.contains("permission denied")
                || msg.contains("403")
                || msg.contains("Authorization failed")
            {
                bail!("UNRECOVERABLE: permission denied pushing to {remote}/{branch}");
            }

            if msg.contains("Could not resolve host")
                || msg.contains("Network is unreachable")
                || msg.contains("Connection refused")
                || msg.contains("timed out")
            {
                eprintln!("[push] Network error — retrying in 2s...");
                thread::sleep(Duration::from_secs(2));
                if let Err(retry_err) = git::push_to(remote, branch, set_upstream) {
                    eprintln!("[push] Retry failed: {retry_err}");
                    bail!("committed locally, push manually later");
                }
                println!("[push] Pushed to {remote}/{branch} (after retry)");
                return Ok(());
            }

            // Unknown error: ask AI provider to diagnose
            eprintln!("[push] Push failed: {msg}");
            let gen_config = &ctx.app_config.generate;
            match generate::fix_push_error(branch, &git::remote_url(remote), &msg, gen_config) {
                Ok(fix_commands) => {
                    if fix_commands.starts_with("UNRECOVERABLE:") {
                        bail!("{fix_commands}");
                    }
                    println!("[push] AI suggests:\n{fix_commands}\n");
                    git::run_commands(&fix_commands)?;
                    println!("[push] Pushed to {remote}/{branch} (via AI fix)");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("[push] AI push recovery unavailable: {e}");
                    bail!("Push failed: {msg}. Fix manually and push again.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_branch_protection_message_detected() {
        let msg = "error: GH006: Protected branch update failed for refs/heads/main.";
        let is_protected = msg.contains("protected branch")
            || msg.contains("GH006")
            || msg.contains("push to a protected branch");
        assert!(is_protected);
    }

    #[test]
    fn test_permission_denied_detected() {
        let msg = "Permission denied (publickey).";
        let is_perm = msg.contains("Permission denied") || msg.contains("403");
        assert!(is_perm);
    }

    #[test]
    fn test_network_error_detected() {
        let msg = "fatal: Could not resolve host: github.com";
        let is_network = msg.contains("Could not resolve host")
            || msg.contains("Network is unreachable")
            || msg.contains("Connection refused")
            || msg.contains("timed out");
        assert!(is_network);
    }
}
