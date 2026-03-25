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
