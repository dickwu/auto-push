use anyhow::{Context, Result, bail};
use std::process::Command;

fn run_cmd(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run: {cmd} {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{cmd} {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub struct GitStatus {
    pub staged: String,
    pub unstaged: String,
    pub untracked: Vec<String>,
}

fn run_git(args: &[&str]) -> Result<String> {
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

pub enum PullResult {
    AlreadyUpToDate,
    FastForward,
    Merged,
}

pub fn ensure_git_repo() -> Result<()> {
    run_git(&["rev-parse", "--git-dir"]).context("not a git repository")?;
    Ok(())
}

pub fn pull() -> Result<PullResult> {
    let output = Command::new("git")
        .args(["pull"])
        .output()
        .context("failed to run: git pull")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!("git pull failed: {}", stderr.trim());
    }

    let combined = format!("{stdout}{stderr}");

    if combined.contains("Already up to date") {
        Ok(PullResult::AlreadyUpToDate)
    } else if combined.contains("Fast-forward") || combined.contains("fast-forward") {
        Ok(PullResult::FastForward)
    } else {
        Ok(PullResult::Merged)
    }
}

pub fn status() -> Result<GitStatus> {
    let staged = run_git(&["diff", "--cached", "--stat"])?;
    let unstaged = run_git(&["diff", "--stat"])?;

    let untracked_output = run_git(&["ls-files", "--others", "--exclude-standard"])?;
    let untracked: Vec<String> = untracked_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    Ok(GitStatus {
        staged,
        unstaged,
        untracked,
    })
}

pub fn has_changes() -> Result<bool> {
    let status = status()?;
    Ok(!status.staged.is_empty() || !status.unstaged.is_empty() || !status.untracked.is_empty())
}

pub fn diff_for_commit() -> Result<String> {
    let staged = run_git(&["diff", "--cached"])?;
    if !staged.is_empty() {
        return Ok(staged);
    }

    let unstaged = run_git(&["diff"])?;
    if !unstaged.is_empty() {
        return Ok(unstaged);
    }

    let untracked = run_git(&["ls-files", "--others", "--exclude-standard"])?;
    if untracked.is_empty() {
        bail!("no changes to commit");
    }

    let mut parts = Vec::new();
    for file in untracked.lines().filter(|l| !l.is_empty()) {
        parts.push(format!("new file: {file}"));
        if let Ok(content) = std::fs::read_to_string(file) {
            let preview: String = content.lines().take(50).collect::<Vec<_>>().join("\n");
            parts.push(preview);
        }
    }
    Ok(parts.join("\n"))
}

pub fn stage_all() -> Result<()> {
    run_git(&["add", "-A"])?;
    Ok(())
}

pub fn commit(message: &str) -> Result<()> {
    run_git(&["commit", "-m", message])?;
    Ok(())
}

pub fn push() -> Result<String> {
    let branch = current_branch()?;
    run_cmd("gh", &["repo", "sync", "--source", &branch])
        .or_else(|_| run_git(&["push", "-u", "origin", &branch]))
        .or_else(|_| run_git(&["push", "--set-upstream", "origin", &branch]))
}

pub fn current_branch() -> Result<String> {
    run_git(&["rev-parse", "--abbrev-ref", "HEAD"])
}
