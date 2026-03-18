use anyhow::{Context, Result, bail};
use std::process::Command;

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
    Conflict,
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
    let combined = format!("{stdout}{stderr}");

    if !output.status.success() {
        if combined.contains("CONFLICT") || combined.contains("Merge conflict") {
            return Ok(PullResult::Conflict);
        }
        bail!("git pull failed: {}", stderr.trim());
    }

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

pub fn unstage_all() -> Result<()> {
    run_git(&["reset", "HEAD"])?;
    Ok(())
}

pub fn stage_files(files: &[String]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    args.extend(files.iter().map(|f| f.as_str()));
    run_git(&args)?;
    Ok(())
}

/// Apply a patch to the index only (staged area) without modifying the working tree.
/// Uses --3way for automatic fallback when context lines don't match exactly.
pub fn apply_patch_to_index(patch: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("git")
        .args(["apply", "--cached", "--3way"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn git apply")?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(patch.as_bytes())?;

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git apply --cached failed: {}", stderr.trim());
    }

    Ok(())
}

pub fn commit(message: &str) -> Result<()> {
    run_git(&["commit", "-m", message])?;
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

pub fn push() -> Result<String> {
    let branch = current_branch()?;
    let remote = default_remote()?;
    run_git(&["push", "-u", &remote, &branch])
}

fn is_allowed_command(line: &str) -> bool {
    line.starts_with("git ") || line.starts_with("UNRECOVERABLE")
}

pub fn run_commands(commands: &str) -> Result<()> {
    for line in commands.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !is_allowed_command(line) {
            eprintln!("  Skipped (not a git command): {line}");
            eprintln!("  Run manually if needed.");
            continue;
        }
        println!("  $ {line}");
        let status = Command::new("sh")
            .args(["-c", line])
            .status()
            .with_context(|| format!("failed to run: {line}"))?;
        if !status.success() {
            bail!("command failed: {line}");
        }
    }
    Ok(())
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

pub fn has_conflicts() -> Result<bool> {
    let files = conflict_files()?;
    Ok(!files.is_empty())
}

pub fn abort_merge() -> Result<()> {
    run_git(&["merge", "--abort"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_git_command_allows_git() {
        assert!(is_allowed_command("git push origin main"));
        assert!(is_allowed_command("git rebase --continue"));
        assert!(is_allowed_command("git fetch --all"));
    }

    #[test]
    fn test_validate_git_command_rejects_non_git() {
        assert!(!is_allowed_command("rm -rf /"));
        assert!(!is_allowed_command("curl http://evil.com"));
        assert!(!is_allowed_command("echo pwned"));
    }

    #[test]
    fn test_validate_git_command_allows_unrecoverable() {
        assert!(is_allowed_command("UNRECOVERABLE: no network"));
    }

    #[test]
    fn test_is_allowed_command_edge_cases() {
        assert!(is_allowed_command("git status"));
        assert!(!is_allowed_command(" git status")); // leading space
        assert!(!is_allowed_command(""));
    }
}
