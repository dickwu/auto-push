use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn run_git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run: git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn ensure_git_repo() -> Result<()> {
    run_git(&["rev-parse", "--git-dir"]).context("not a git repository")?;
    Ok(())
}

/// Returns the first configured remote name, preferring "origin" if present.
pub fn default_remote() -> Result<String> {
    let remotes = run_git(&["remote"])?;
    if remotes.is_empty() {
        bail!(
            "no git remote configured.\n\
             Add one with: git remote add origin <url>"
        );
    }
    let preferred = remotes.lines().find(|r| *r == "origin");
    Ok(preferred
        .unwrap_or_else(|| remotes.lines().next().unwrap())
        .to_string())
}

pub fn remote_url(name: &str) -> String {
    run_git(&["remote", "get-url", name]).unwrap_or_else(|_| name.to_string())
}

pub fn current_branch() -> Result<String> {
    run_git(&["rev-parse", "--abbrev-ref", "HEAD"])
}

pub fn conflict_files() -> Result<Vec<String>> {
    let output = run_git(&["diff", "--name-only", "--diff-filter=U"])?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Run a git command, returning Ok(output) even on non-zero exit.
pub fn run_git_check(args: &[&str]) -> Result<(String, String, bool)> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run: git {}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok((stdout, stderr, output.status.success()))
}

pub fn is_detached_head() -> Result<bool> {
    let (_, _, success) = run_git_check(&["symbolic-ref", "-q", "HEAD"])?;
    Ok(!success)
}

pub fn has_remote() -> Result<bool> {
    let remotes = run_git(&["remote"])?;
    Ok(!remotes.is_empty())
}

pub fn has_upstream() -> Result<bool> {
    let (_, _, success) = run_git_check(&["rev-parse", "--abbrev-ref", "@{u}"])?;
    Ok(success)
}

pub fn is_shallow() -> Result<bool> {
    let output = run_git(&["rev-parse", "--is-shallow-repository"])?;
    Ok(output == "true")
}

pub fn repo_root() -> Result<String> {
    run_git(&["rev-parse", "--show-toplevel"])
}

pub fn has_gitmodules() -> Result<bool> {
    let root = repo_root()?;
    let gitmodules = std::path::Path::new(&root).join(".gitmodules");
    Ok(gitmodules.exists())
}

pub fn has_lfs() -> Result<bool> {
    let root = repo_root()?;
    let gitattributes = std::path::Path::new(&root).join(".gitattributes");
    if !gitattributes.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&gitattributes)
        .with_context(|| format!("failed to read {}", gitattributes.display()))?;
    Ok(content.contains("filter=lfs"))
}

pub fn submodule_paths() -> Result<Vec<String>> {
    let (stdout, _, success) = run_git_check(&["submodule", "status", "--recursive"])?;
    if !success || stdout.is_empty() {
        return Ok(vec![]);
    }
    let paths = stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start_matches([' ', '+', '-', 'U']);
            trimmed.split_whitespace().nth(1).map(String::from)
        })
        .collect();
    Ok(paths)
}
