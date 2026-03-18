use anyhow::{Context, Result, bail};
use std::process::Command;

pub struct GitStatus {
    pub staged: String,
    pub unstaged: String,
    pub untracked: Vec<String>,
}

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

pub fn submodule_update_init() -> Result<()> {
    run_git(&["submodule", "update", "--init", "--recursive"])?;
    Ok(())
}

pub fn submodule_is_dirty(path: &str) -> Result<bool> {
    let (stdout, _, _) = run_git_check(&["-C", path, "status", "--porcelain"])?;
    Ok(!stdout.is_empty())
}

pub fn submodule_tracking_branch(path: &str) -> Result<Option<String>> {
    let (stdout, _, success) = run_git_check(&[
        "config",
        "-f",
        ".gitmodules",
        &format!("submodule.{path}.branch"),
    ])?;
    if success && !stdout.is_empty() {
        return Ok(Some(stdout));
    }
    let (stdout, _, success) = run_git_check(&["-C", path, "remote", "show", "origin"])?;
    if success {
        for line in stdout.lines() {
            if line.contains("HEAD branch:")
                && let Some(branch) = line.split(':').nth(1)
            {
                return Ok(Some(branch.trim().to_string()));
            }
        }
    }
    Ok(None)
}

pub fn has_dirty_working_tree() -> Result<bool> {
    let (stdout, _, _) = run_git_check(&["status", "--porcelain"])?;
    Ok(!stdout.is_empty())
}

pub fn stash_push() -> Result<()> {
    run_git(&[
        "stash",
        "push",
        "--include-untracked",
        "-m",
        "auto-push: pre-pull stash",
    ])?;
    Ok(())
}

pub fn stash_pop() -> Result<bool> {
    let (_, stderr, success) = run_git_check(&["stash", "pop"])?;
    if !success && stderr.contains("CONFLICT") {
        return Ok(false);
    }
    if !success {
        anyhow::bail!("git stash pop failed: {stderr}");
    }
    Ok(true)
}

pub fn pull_rebase() -> Result<PullResult> {
    let output = Command::new("git")
        .args(["pull", "--rebase"])
        .output()
        .context("failed to run: git pull --rebase")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    if !output.status.success() {
        if combined.contains("CONFLICT") || combined.contains("could not apply") {
            return Ok(PullResult::Conflict);
        }
        bail!("git pull --rebase failed: {}", stderr.trim());
    }
    if combined.contains("Already up to date") {
        Ok(PullResult::AlreadyUpToDate)
    } else if combined.contains("Fast-forward") || combined.contains("fast-forward") {
        Ok(PullResult::FastForward)
    } else {
        Ok(PullResult::Merged)
    }
}

pub fn rebase_continue() -> Result<bool> {
    let (_, stderr, success) = run_git_check(&["rebase", "--continue"])?;
    if !success && (stderr.contains("CONFLICT") || stderr.contains("could not apply")) {
        return Ok(false);
    }
    Ok(success)
}

pub fn rebase_abort() -> Result<()> {
    run_git(&["rebase", "--abort"])?;
    Ok(())
}

pub fn push_to(remote: &str, branch: &str, set_upstream: bool) -> Result<String> {
    if set_upstream {
        run_git(&["push", "-u", remote, branch])
    } else {
        run_git(&["push", remote, branch])
    }
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
