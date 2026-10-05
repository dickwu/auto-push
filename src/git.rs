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

/// The remote-tracking branch a local branch pushes to and pulls from.
///
/// Its `branch` is the name ON THE REMOTE, which need not match the local
/// name: a release worktree checked out as `rel-1` from `origin/main` tracks
/// `main`. Every push auto-push makes must target this, never the local
/// name, or a bare `git push` refuses (git's `push.default=simple`) and a
/// `git push origin <local>` invents a stray remote branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub remote: String,
    pub branch: String,
}

impl Upstream {
    /// What `git push <remote> …` needs to land HEAD on the tracked branch.
    pub fn push_refspec(&self) -> String {
        format!("HEAD:{}", self.branch)
    }
}

/// Resolve the current branch's upstream, or `None` when it has none.
pub fn upstream(branch: &str) -> Result<Option<Upstream>> {
    let (stdout, _, success) = run_git_check(&[
        "for-each-ref",
        "--format=%(upstream:remotename) %(upstream:remoteref)",
        &format!("refs/heads/{branch}"),
    ])?;
    if !success {
        return Ok(None);
    }
    Ok(parse_upstream_ref(&stdout))
}

/// Parse one `for-each-ref` line of the form `<remote> refs/heads/<branch>`.
/// An unset upstream prints a blank line (or one lone space); a branch name
/// may itself contain `/`, which is why the remote and the ref are read as
/// two space-separated fields rather than split on `/`.
fn parse_upstream_ref(line: &str) -> Option<Upstream> {
    let mut fields = line.trim().splitn(2, ' ');
    let remote = fields.next()?.trim();
    let remote_ref = fields.next()?.trim();
    if remote.is_empty() || remote_ref.is_empty() {
        return None;
    }
    let branch = remote_ref.strip_prefix("refs/heads/").unwrap_or(remote_ref);
    Some(Upstream {
        remote: remote.to_string(),
        branch: branch.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_upstream_ref_strips_the_heads_prefix() {
        let up = parse_upstream_ref("origin refs/heads/main\n").unwrap();
        assert_eq!(up.remote, "origin");
        assert_eq!(up.branch, "main");
        assert_eq!(up.push_refspec(), "HEAD:main");
    }

    #[test]
    fn test_parse_upstream_ref_keeps_slashes_in_branch_names() {
        let up = parse_upstream_ref("origin refs/heads/release/2026-10").unwrap();
        assert_eq!(up.branch, "release/2026-10");
    }

    #[test]
    fn test_parse_upstream_ref_none_when_unset() {
        assert_eq!(parse_upstream_ref(" "), None);
        assert_eq!(parse_upstream_ref(""), None);
        assert_eq!(parse_upstream_ref("origin "), None);
    }
}
