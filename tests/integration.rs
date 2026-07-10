use std::path::Path;
use std::process::Command;

fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {}: {e}", args.join(" ")));
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn init_repo(dir: &Path) {
    git_in(dir, &["init"]);
    git_in(dir, &["config", "user.email", "test@test.com"]);
    git_in(dir, &["config", "user.name", "Test"]);
}

fn auto_push_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_auto-push"))
}

// ---------------------------------------------------------------------------
// Default pipeline end-to-end harness
// ---------------------------------------------------------------------------

/// A workspace with a bare remote, a clone on branch `main` with upstream
/// set, and an isolated HOME so a real `~/.auto-push.json` can't leak in.
struct PipelineWorkspace {
    root: tempfile::TempDir,
}

impl PipelineWorkspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote.git");
        let repo = root.path().join("repo");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        git_in(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "test@test.com"]);
        git_in(&repo, &["config", "user.name", "Test"]);
        git_in(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(repo.join("base.txt"), "base").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-m", "init"]);
        git_in(&repo, &["push", "-u", "origin", "main"]);

        Self { root }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.root.path().join("repo")
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    /// Write a config mirroring the auto-init default pipeline, with the
    /// generate step's `run` replaced (the AI call is the only
    /// non-deterministic step).
    fn write_default_pipeline_config(&self, generate_run: &str) {
        let config = serde_json::json!({
            "pipeline": [
                {"name": "stash",   "run": "git stash push -m 'auto-push auto-stash' || true"},
                {"name": "pull",    "run": "git pull"},
                {"name": "unstash", "run": "git stash pop || true"},
                {"name": "stage",   "run": "git add -A"},
                {"name": "generate","run": generate_run, "capture": "commit_message"},
                {"name": "commit",  "run": "git commit -m '{{ commit_message }}'",
                 "capture_after": [
                    {"name": "commit_hash", "run": "git rev-parse --short HEAD"},
                    {"name": "commit_summary", "run": "git log -1 --format=%s"}
                 ]},
                {"name": "push",    "run": "git push origin {{ branch }}",
                 "on_error": "sleep 2 && git push origin {{ branch }}"}
            ]
        });
        std::fs::write(
            self.repo().join(".auto-push.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    fn run_auto_push(&self, args: &[&str]) -> std::process::Output {
        auto_push_bin()
            .args(args)
            .current_dir(self.repo())
            .env("HOME", self.home())
            .output()
            .unwrap()
    }

    fn last_remote_message(&self) -> String {
        let out = Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .env("GIT_DIR", self.root.path().join("remote.git"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn remote_commit_count(&self) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "main"])
            .env("GIT_DIR", self.root.path().join("remote.git"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn test_default_pipeline_hostile_message_end_to_end() {
    // A generated commit message full of shell metacharacters and a
    // multi-line body must commit and push byte-for-byte intact.
    let ws = PipelineWorkspace::new();
    let message = "fix: don't break on \"quotes\" & $(subst) `ticks`; <redirects>|pipes!\n\nBody has 'single quotes', $VAR, \\backslash and a second line";
    std::fs::write(ws.repo().join("msg.txt"), message).unwrap();
    ws.write_default_pipeline_config("cat msg.txt");

    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();
    let output = ws.run_auto_push(&[]);

    assert!(
        output.status.success(),
        "pipeline failed: {}",
        combined_output(&output)
    );
    assert_eq!(ws.last_remote_message().trim_end(), message);
}

#[test]
fn test_default_pipeline_strips_ansi_from_generated_message() {
    // AI CLIs can emit color codes even when asked not to; they must not
    // reach the commit message.
    let ws = PipelineWorkspace::new();
    ws.write_default_pipeline_config("printf 'fix: \\033[31mcolored\\033[0m message'");

    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();
    let output = ws.run_auto_push(&[]);

    assert!(
        output.status.success(),
        "pipeline failed: {}",
        combined_output(&output)
    );
    assert_eq!(ws.last_remote_message().trim_end(), "fix: colored message");
}

#[test]
fn test_default_pipeline_dash_m_message_with_specials() {
    // The -m path pre-registers commit_message and skips generate; special
    // characters must survive the same commit template.
    let ws = PipelineWorkspace::new();
    ws.write_default_pipeline_config("echo unused");

    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();
    let message = "chore: Peilin's \"release\" costs $5 && `works`";
    let output = ws.run_auto_push(&["-m", message]);

    assert!(
        output.status.success(),
        "pipeline failed: {}",
        combined_output(&output)
    );
    assert_eq!(ws.last_remote_message().trim_end(), message);
}

#[test]
fn test_default_pipeline_dry_run_makes_no_changes() {
    let ws = PipelineWorkspace::new();
    ws.write_default_pipeline_config("cat msg.txt");
    std::fs::write(ws.repo().join("msg.txt"), "feat: never committed").unwrap();
    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();

    let before = ws.remote_commit_count();
    let output = ws.run_auto_push(&["--dry-run"]);

    assert!(
        output.status.success(),
        "dry-run failed: {}",
        combined_output(&output)
    );
    assert_eq!(ws.remote_commit_count(), before, "dry-run pushed a commit");
    let local_log = git_in(&ws.repo(), &["log", "--format=%s"]);
    assert!(
        !local_log.contains("never committed"),
        "dry-run created a commit: {local_log}"
    );
}

#[test]
fn test_pipeline_no_shell_injection_through_binary() {
    // End-to-end guard: a captured value carrying a shell payload must never
    // execute, in any quote context, when run through the real binary.
    let ws = PipelineWorkspace::new();
    let marker = ws.root.path().join("PWNED");
    let payload = format!("subject\ntouch {}\ntrailer", marker.display());
    std::fs::write(ws.repo().join("msg.txt"), &payload).unwrap();

    // A custom pipeline that interpolates the captured value in single-quoted,
    // double-quoted, and `#`-comment contexts before committing.
    let config = serde_json::json!({
        "pipeline": [
            {"name": "stage", "run": "git add -A"},
            {"name": "generate", "run": "cat msg.txt", "capture": "commit_message"},
            {"name": "single", "run": "printf %s '{{ commit_message }}' > /dev/null"},
            {"name": "double", "run": "printf %s \"{{ commit_message }}\" > /dev/null"},
            {"name": "comment", "run": "true # {{ commit_message }}"},
            {"name": "commit", "run": "git commit -m '{{ commit_message }}'"},
            {"name": "push", "run": "git push origin {{ branch }}"}
        ]
    });
    std::fs::write(
        ws.repo().join(".auto-push.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();

    let output = ws.run_auto_push(&[]);
    assert!(
        output.status.success(),
        "pipeline failed: {}",
        combined_output(&output)
    );
    assert!(
        !marker.exists(),
        "shell injection executed: {}",
        combined_output(&output)
    );
    assert_eq!(ws.last_remote_message().trim_end(), payload);
}

#[test]
fn test_default_pipeline_auto_init_runs_end_to_end() {
    // With no config and no AI CLI on PATH, auto-init must write the real
    // default pipeline (placeholder generate) and run it to completion.
    let ws = PipelineWorkspace::new();
    std::fs::write(ws.repo().join("change.txt"), "change").unwrap();

    let output = auto_push_bin()
        .current_dir(ws.repo())
        .env("HOME", ws.home())
        // git + sh + coreutils, but no claude/codex/ollama
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "auto-init pipeline failed: {}",
        combined_output(&output)
    );
    assert!(
        ws.repo().join(".auto-push.json").exists(),
        "auto-init did not write config"
    );
    assert_eq!(ws.remote_commit_count(), 2, "pipeline did not push");
}

#[test]
fn test_preflight_detects_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    let output = auto_push_bin().current_dir(dir.path()).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(!output.status.success());
    assert!(
        combined.contains("not a git repository") || combined.contains("git"),
        "Expected git repo error, got: {combined}"
    );
}

#[test]
fn test_preflight_detects_no_remote() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("file.txt"), "hello").unwrap();
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-m", "init"]);

    let output = auto_push_bin().current_dir(dir.path()).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        !output.status.success(),
        "Expected failure for no remote, got success: {combined}"
    );
    assert!(
        combined.contains("no git remote") || combined.contains("remote"),
        "Expected no-remote error, got: {combined}"
    );
}

#[test]
fn test_help_shows_new_flags() {
    let output = auto_push_bin().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--rebase"), "Missing --rebase flag in help");
    assert!(
        stdout.contains("--no-pull"),
        "Missing --no-pull flag in help"
    );
    assert!(
        stdout.contains("--no-submodules"),
        "Missing --no-submodules flag in help"
    );
    assert!(
        stdout.contains("--no-stash"),
        "Missing --no-stash flag in help"
    );
    assert!(
        stdout.contains("--smart-init"),
        "Missing --smart-init flag in help"
    );
    assert!(stdout.contains("--yes"), "Missing --yes flag in help");
}

// ---------------------------------------------------------------------------
// Smart init JSON contract tests
// ---------------------------------------------------------------------------

#[test]
fn test_smart_init_json_contract() {
    // Validates the JSON shape that AI providers must return.
    // auto-push is a binary crate so we can't import internal types —
    // use serde_json::Value to verify the contract.
    let json = r#"{
        "analysis": "Rust CLI project",
        "steps": [
            {"name": "stash", "kind": "stash", "run": "git stash push -m 'auto-push' || true", "description": "Stash"},
            {"name": "pull", "kind": "pull", "run": "git pull", "description": "Pull"},
            {"name": "unstash", "kind": "unstash", "run": "git stash pop || true", "description": "Unstash"},
            {"name": "test", "kind": "custom", "run": "cargo test", "description": "Tests", "confidence": "high"},
            {"name": "stage", "kind": "stage", "run": "git add -A", "description": "Stage"},
            {"name": "generate", "kind": "generate", "run": "echo placeholder", "description": "Generate"},
            {"name": "commit", "kind": "commit", "run": "git commit -m '{{ commit_message }}'", "description": "Commit"},
            {"name": "push", "kind": "push", "run": "git push origin main", "description": "Push"}
        ],
        "detected": {"language": "rust", "package_manager": "cargo"}
    }"#;

    let resp: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(resp["steps"].as_array().unwrap().len(), 8);
    assert_eq!(resp["detected"]["language"].as_str(), Some("rust"));
    assert_eq!(resp["steps"][0]["kind"].as_str(), Some("stash"));
    assert_eq!(resp["steps"][3]["confidence"].as_str(), Some("high"));
    assert_eq!(resp["analysis"].as_str(), Some("Rust CLI project"));
}

#[test]
fn test_smart_init_requires_remote() {
    // --smart-init still needs a git remote (preflight checks run first)
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("file.txt"), "hello").unwrap();
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-m", "init"]);

    let output = auto_push_bin()
        .args(["--smart-init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("remote"),
        "Expected remote error, got: {combined}"
    );
}

#[test]
fn test_version_shows_current() {
    let output = auto_push_bin().arg("--version").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = env!("CARGO_PKG_VERSION");
    assert!(
        stdout.contains(expected),
        "Expected version {expected}, got: {stdout}"
    );
}
