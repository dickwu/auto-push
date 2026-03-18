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
