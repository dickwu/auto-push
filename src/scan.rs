// Module will be consumed by smart_init.rs (upcoming task).
#![allow(dead_code)]

use ignore::WalkBuilder;
use std::fs;
use std::path::Path;
use url::Url;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_CONFIG_FILE_BYTES: usize = 2048;
const MAX_FILE_TREE_BYTES: usize = 8192;
const MAX_WORKSPACES: usize = 20;

/// Manifest files that identify a workspace root.
const MANIFEST_FILES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "Gemfile",
    "mix.exs",
    "pubspec.yaml",
    "CMakeLists.txt",
    "Makefile",
];

/// CI config paths (relative to repo root).
const CI_PATTERNS: &[&str] = &[
    ".gitlab-ci.yml",
    ".circleci/config.yml",
    ".travis.yml",
    "Jenkinsfile",
    "bitbucket-pipelines.yml",
];

/// Build / infra files.
const BUILD_FILES: &[&str] = &["Dockerfile", "docker-compose.yml", "docker-compose.yaml"];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProjectFingerprint {
    pub workspaces: Vec<Workspace>,
    pub git_remotes: Vec<GitRemote>,
    pub ci_files: Vec<ConfigFile>,
    pub build_files: Vec<ConfigFile>,
    pub has_monorepo_markers: bool,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub path: String,
    pub config_files: Vec<ConfigFile>,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub path: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Credential redaction
// ---------------------------------------------------------------------------

/// Redacts user-info (user:token) from HTTPS URLs.
/// SSH URLs and non-URL strings pass through unchanged.
pub fn redact_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(parsed) if !parsed.username().is_empty() || parsed.password().is_some() => {
            let mut redacted = parsed.clone();
            let _ = redacted.set_username("***");
            let _ = redacted.set_password(None);
            redacted.to_string()
        }
        _ => raw.to_string(),
    }
}

/// Returns true for filenames that likely contain secrets.
pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Exact matches
    if matches!(lower.as_str(), ".env" | ".npmrc" | ".pypirc") {
        return true;
    }
    // .env.* variants
    if lower.starts_with(".env.") {
        return true;
    }
    // Extension-based
    if lower.ends_with(".pem") || lower.ends_with(".key") {
        return true;
    }
    // Prefix-based (auth.*, credentials.*)
    if lower.starts_with("auth.") || lower.starts_with("credentials.") {
        return true;
    }
    false
}

/// Truncates content to at most `max_bytes` (on a char boundary),
/// appending a marker if truncation occurred.
pub fn truncate_content(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    // Find the largest char-boundary <= max_bytes
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = content[..end].to_string();
    result.push_str("\n... (truncated)");
    result
}

// ---------------------------------------------------------------------------
// Git remote parsing
// ---------------------------------------------------------------------------

/// Parses `git remote -v` output into deduplicated, redacted remotes.
pub fn parse_git_remotes(raw: &str) -> Vec<GitRemote> {
    let mut seen = std::collections::HashSet::new();
    let mut remotes = Vec::new();

    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let name = parts[0].to_string();
        let url = redact_url(parts[1]);
        let key = format!("{name}\t{url}");
        if seen.insert(key) {
            remotes.push(GitRemote { name, url });
        }
    }
    remotes
}

// ---------------------------------------------------------------------------
// File tree builder
// ---------------------------------------------------------------------------

/// Builds a gitignore-aware file tree string, capped at `MAX_FILE_TREE_BYTES`.
pub fn build_file_tree(root: &Path, max_depth: usize) -> String {
    let mut tree = String::new();

    let walker = WalkBuilder::new(root)
        .max_depth(Some(max_depth))
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    for entry in walker.flatten() {
        let depth = entry.depth();
        if depth == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        // Skip .git directory itself
        if name == ".git" {
            continue;
        }
        let indent = "  ".repeat(depth - 1);
        let suffix = if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            "/"
        } else {
            ""
        };
        let line = format!("{indent}{name}{suffix}\n");

        if tree.len() + line.len() > MAX_FILE_TREE_BYTES {
            tree.push_str("... (truncated)\n");
            break;
        }
        tree.push_str(&line);
    }
    tree
}

// ---------------------------------------------------------------------------
// Workspace detection helpers
// ---------------------------------------------------------------------------

/// Auto-labels well-known directory names.
fn auto_label(rel_path: &str) -> Option<String> {
    let dir_name = Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    match dir_name.as_str() {
        "src-tauri" => Some("tauri-backend".to_string()),
        "android" => Some("android".to_string()),
        "ios" => Some("ios".to_string()),
        _ => {
            // packages/* pattern -> monorepo-package
            if rel_path.starts_with("packages/") || rel_path.starts_with("packages\\") {
                Some("monorepo-package".to_string())
            } else {
                None
            }
        }
    }
}

/// Reads a config file with truncation and secret-file filtering.
fn read_config_file(path: &Path, rel_path: &str) -> Option<ConfigFile> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if is_secret_file(&file_name) {
        return None;
    }

    let content = fs::read_to_string(path).ok()?;
    Some(ConfigFile {
        path: rel_path.to_string(),
        content: truncate_content(&content, MAX_CONFIG_FILE_BYTES),
    })
}

/// Checks whether a directory is safely inside `repo_root` (no symlink escape).
fn is_inside_repo(dir: &Path, repo_root: &Path) -> bool {
    let Ok(canonical_dir) = fs::canonicalize(dir) else {
        return false;
    };
    let Ok(canonical_root) = fs::canonicalize(repo_root) else {
        return false;
    };
    canonical_dir.starts_with(&canonical_root)
}

/// Detects monorepo markers in the repo root.
fn detect_monorepo_markers(root: &Path) -> bool {
    // package.json with "workspaces"
    if let Ok(content) = fs::read_to_string(root.join("package.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
        && val.get("workspaces").is_some()
    {
        return true;
    }
    // Cargo.toml with [workspace]
    if let Ok(content) = fs::read_to_string(root.join("Cargo.toml"))
        && content.contains("[workspace]")
    {
        return true;
    }
    // Standalone monorepo config files
    let markers = ["lerna.json", "nx.json", "pnpm-workspace.yaml"];
    for m in markers {
        if root.join(m).exists() {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Main scanner
// ---------------------------------------------------------------------------

/// Scans a project directory and returns a `ProjectFingerprint`.
pub fn scan_project(root: &Path) -> ProjectFingerprint {
    // ---- git remotes ----
    let git_remotes = crate::git::run_git(&["remote", "-v"])
        .map(|raw| parse_git_remotes(&raw))
        .unwrap_or_default();

    // ---- root workspace config files ----
    let root_configs: Vec<ConfigFile> = MANIFEST_FILES
        .iter()
        .filter_map(|name| {
            let p = root.join(name);
            if p.is_file() {
                read_config_file(&p, name)
            } else {
                None
            }
        })
        .collect();

    let root_workspace = Workspace {
        path: ".".to_string(),
        config_files: root_configs,
        label: None,
    };

    // ---- sub-workspace discovery ----
    let mut sub_workspaces: Vec<Workspace> = Vec::new();

    let walker = WalkBuilder::new(root)
        .max_depth(Some(4))
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    for entry in walker.flatten() {
        if sub_workspaces.len() >= MAX_WORKSPACES {
            break;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !MANIFEST_FILES.contains(&file_name.as_str()) {
            continue;
        }
        let entry_path = entry.path();
        let Some(parent) = entry_path.parent() else {
            continue;
        };
        // Skip root directory (already handled)
        if parent == root {
            continue;
        }
        // Symlink containment
        if !is_inside_repo(parent, root) {
            continue;
        }
        let rel_dir = parent
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Avoid duplicate workspaces
        if sub_workspaces.iter().any(|w| w.path == rel_dir) {
            // Append config to existing workspace
            if let Some(ws) = sub_workspaces.iter_mut().find(|w| w.path == rel_dir) {
                let rel_file = format!("{rel_dir}/{file_name}");
                if let Some(cf) = read_config_file(entry_path, &rel_file) {
                    ws.config_files.push(cf);
                }
            }
            continue;
        }

        let rel_file = format!("{rel_dir}/{file_name}");
        let configs: Vec<ConfigFile> = read_config_file(entry_path, &rel_file)
            .into_iter()
            .collect();

        sub_workspaces.push(Workspace {
            path: rel_dir.clone(),
            config_files: configs,
            label: auto_label(&rel_dir),
        });
    }

    let mut workspaces = vec![root_workspace];
    workspaces.extend(sub_workspaces);

    // ---- CI files ----
    let mut ci_files: Vec<ConfigFile> = Vec::new();

    // GitHub Actions workflows
    let workflows_dir = root.join(".github/workflows");
    if workflows_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&workflows_dir)
    {
        let mut sorted: Vec<_> = entries.flatten().collect();
        sorted.sort_by_key(|e| e.file_name());
        for entry in sorted {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                let rel = format!(".github/workflows/{name}");
                if let Some(cf) = read_config_file(&entry.path(), &rel) {
                    ci_files.push(cf);
                }
            }
        }
    }

    // Other CI configs
    for pattern in CI_PATTERNS {
        let p = root.join(pattern);
        if p.is_file()
            && let Some(cf) = read_config_file(&p, pattern)
        {
            ci_files.push(cf);
        }
    }

    // ---- Build files ----
    let build_files: Vec<ConfigFile> = BUILD_FILES
        .iter()
        .filter_map(|name| {
            let p = root.join(name);
            if p.is_file() {
                read_config_file(&p, name)
            } else {
                None
            }
        })
        .collect();

    // ---- Monorepo markers ----
    let has_monorepo_markers = detect_monorepo_markers(root);

    ProjectFingerprint {
        workspaces,
        git_remotes,
        ci_files,
        build_files,
        has_monorepo_markers,
    }
}

// ---------------------------------------------------------------------------
// Prompt context formatter
// ---------------------------------------------------------------------------

impl ProjectFingerprint {
    /// Formats the fingerprint as structured text suitable for an AI prompt.
    pub fn to_prompt_context(&self, file_tree: &str) -> String {
        let mut out = String::new();

        // Git remotes
        if !self.git_remotes.is_empty() {
            out.push_str("## Git Remotes\n");
            for r in &self.git_remotes {
                out.push_str(&format!("- {} {}\n", r.name, r.url));
            }
            out.push('\n');
        }

        // Monorepo
        if self.has_monorepo_markers {
            out.push_str("## Monorepo: yes\n\n");
        }

        // Workspaces
        out.push_str("## Workspaces\n");
        for ws in &self.workspaces {
            let label_part = ws
                .label
                .as_ref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default();
            out.push_str(&format!("### {}{label_part}\n", ws.path));
            for cf in &ws.config_files {
                out.push_str(&format!("#### {}\n```\n{}\n```\n", cf.path, cf.content));
            }
        }

        // CI files
        if !self.ci_files.is_empty() {
            out.push_str("\n## CI/CD\n");
            for cf in &self.ci_files {
                out.push_str(&format!("#### {}\n```\n{}\n```\n", cf.path, cf.content));
            }
        }

        // Build files
        if !self.build_files.is_empty() {
            out.push_str("\n## Build/Infra\n");
            for cf in &self.build_files {
                out.push_str(&format!("#### {}\n```\n{}\n```\n", cf.path, cf.content));
            }
        }

        // File tree
        if !file_tree.is_empty() {
            out.push_str("\n## File Tree\n```\n");
            out.push_str(file_tree);
            out.push_str("```\n");
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // -- redact_url --

    #[test]
    fn test_redact_url_with_credentials() {
        let input = "https://user:ghp_secret123@github.com/org/repo.git";
        let result = redact_url(input);
        assert!(
            result.contains("***@"),
            "expected redacted user-info: {result}"
        );
        assert!(!result.contains("ghp_secret123"), "token must not appear");
        assert!(result.contains("github.com"), "host must be preserved");
    }

    #[test]
    fn test_redact_url_no_credentials() {
        let input = "https://github.com/org/repo.git";
        assert_eq!(redact_url(input), input);
    }

    #[test]
    fn test_redact_url_ssh() {
        let input = "git@github.com:org/repo.git";
        // SSH URLs are not parseable as http URLs, pass through unchanged
        assert_eq!(redact_url(input), input);
    }

    // -- is_secret_file --

    #[test]
    fn test_is_secret_file_positive() {
        assert!(is_secret_file(".env"));
        assert!(is_secret_file(".env.local"));
        assert!(is_secret_file(".env.production"));
        assert!(is_secret_file(".npmrc"));
        assert!(is_secret_file(".pypirc"));
        assert!(is_secret_file("server.pem"));
        assert!(is_secret_file("private.key"));
        assert!(is_secret_file("auth.json"));
        assert!(is_secret_file("credentials.yaml"));
    }

    #[test]
    fn test_is_secret_file_negative() {
        assert!(!is_secret_file("package.json"));
        assert!(!is_secret_file("Cargo.toml"));
        assert!(!is_secret_file("README.md"));
        assert!(!is_secret_file("main.rs"));
        assert!(!is_secret_file("environment.ts"));
        assert!(!is_secret_file("keyboard.rs"));
    }

    // -- truncate_content --

    #[test]
    fn test_truncate_content_short() {
        let content = "hello world";
        assert_eq!(truncate_content(content, 100), "hello world");
    }

    #[test]
    fn test_truncate_content_long() {
        let content = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_content(content, 10);
        assert!(result.starts_with("abcdefghij"));
        assert!(result.ends_with("... (truncated)"));
        assert!(!result.contains("klmno"));
    }

    // -- parse_git_remotes --

    #[test]
    fn test_parse_git_remotes() {
        let raw = "\
origin\thttps://user:token123@github.com/org/repo.git (fetch)
origin\thttps://user:token123@github.com/org/repo.git (push)
upstream\tgit@github.com:upstream/repo.git (fetch)
upstream\tgit@github.com:upstream/repo.git (push)";

        let remotes = parse_git_remotes(raw);
        // Should deduplicate: 2 unique name+url pairs
        assert_eq!(remotes.len(), 2, "expected 2 deduplicated remotes");
        // First remote should be redacted
        assert!(
            !remotes[0].url.contains("token123"),
            "credentials must be redacted"
        );
        assert!(remotes[0].url.contains("***@"), "should contain ***@");
        // SSH remote passes through
        assert_eq!(remotes[1].url, "git@github.com:upstream/repo.git");
    }

    // -- build_file_tree --

    #[test]
    fn test_build_file_tree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let tree = build_file_tree(root, 3);
        assert!(tree.contains("Cargo.toml"), "should list Cargo.toml");
        assert!(tree.contains("src/"), "should list src/");
        assert!(tree.contains("main.rs"), "should list main.rs");
    }

    // -- scan_project --

    #[test]
    fn test_scan_rust_project() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        // Init a git repo so run_git works from this dir
        init_test_repo(root);

        let fp = scan_project(root);
        assert_eq!(fp.workspaces.len(), 1);
        assert_eq!(fp.workspaces[0].path, ".");
        assert!(!fp.workspaces[0].config_files.is_empty());
        assert_eq!(fp.workspaces[0].config_files[0].path, "Cargo.toml");
    }

    #[test]
    fn test_scan_tauri_project() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Root workspace
        fs::write(root.join("package.json"), r#"{"name":"app"}"#).unwrap();

        // Tauri sub-workspace
        fs::create_dir_all(root.join("src-tauri")).unwrap();
        fs::write(
            root.join("src-tauri/Cargo.toml"),
            "[package]\nname = \"tauri-app\"",
        )
        .unwrap();

        init_test_repo(root);

        let fp = scan_project(root);
        assert!(
            fp.workspaces.len() >= 2,
            "expected root + tauri workspace, got {}",
            fp.workspaces.len()
        );
        let tauri_ws = fp.workspaces.iter().find(|w| w.path == "src-tauri");
        assert!(tauri_ws.is_some(), "should find src-tauri workspace");
        assert_eq!(tauri_ws.unwrap().label.as_deref(), Some("tauri-backend"));
    }

    #[test]
    fn test_scan_monorepo() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(
            root.join("package.json"),
            r#"{"name":"mono","workspaces":["packages/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/core")).unwrap();
        fs::write(
            root.join("packages/core/package.json"),
            r#"{"name":"@mono/core"}"#,
        )
        .unwrap();

        init_test_repo(root);

        let fp = scan_project(root);
        assert!(fp.has_monorepo_markers, "should detect monorepo");
        let core_ws = fp.workspaces.iter().find(|w| w.path == "packages/core");
        assert!(core_ws.is_some(), "should find packages/core");
        assert_eq!(core_ws.unwrap().label.as_deref(), Some("monorepo-package"));
    }

    #[test]
    fn test_scan_empty_project() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        init_test_repo(root);

        let fp = scan_project(root);
        // Should always have the root workspace
        assert_eq!(fp.workspaces.len(), 1);
        assert_eq!(fp.workspaces[0].path, ".");
        assert!(fp.workspaces[0].config_files.is_empty());
    }

    #[test]
    fn test_scan_max_workspaces() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("package.json"), r#"{"name":"root"}"#).unwrap();

        // Create MAX_WORKSPACES + 5 sub-workspaces
        for i in 0..(MAX_WORKSPACES + 5) {
            let dir = root.join(format!("pkg-{i:03}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"pkg-{i:03}"}}"#),
            )
            .unwrap();
        }

        init_test_repo(root);

        let fp = scan_project(root);
        // root (1) + capped sub-workspaces (MAX_WORKSPACES)
        assert!(
            fp.workspaces.len() <= MAX_WORKSPACES + 1,
            "workspaces should be capped at {}, got {}",
            MAX_WORKSPACES + 1,
            fp.workspaces.len()
        );
    }

    #[test]
    fn test_scan_skips_secret_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("package.json"), r#"{"name":"app"}"#).unwrap();
        fs::write(root.join(".env"), "SECRET=abc").unwrap();
        fs::write(root.join(".env.local"), "SECRET=local").unwrap();
        fs::write(root.join("credentials.json"), "{}").unwrap();

        init_test_repo(root);

        let fp = scan_project(root);
        let all_paths: Vec<&str> = fp
            .workspaces
            .iter()
            .flat_map(|w| w.config_files.iter().map(|c| c.path.as_str()))
            .collect();

        assert!(!all_paths.contains(&".env"), "should skip .env");
        assert!(!all_paths.contains(&".env.local"), "should skip .env.local");
        assert!(
            !all_paths.contains(&"credentials.json"),
            "should skip credentials.json"
        );
    }

    #[test]
    fn test_scan_truncation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let big_content = "x".repeat(MAX_CONFIG_FILE_BYTES + 500);
        fs::write(root.join("package.json"), &big_content).unwrap();

        init_test_repo(root);

        let fp = scan_project(root);
        let pkg = fp.workspaces[0]
            .config_files
            .iter()
            .find(|c| c.path == "package.json");
        assert!(pkg.is_some(), "should collect package.json");
        let content = &pkg.unwrap().content;
        assert!(content.ends_with("... (truncated)"));
        assert!(content.len() < big_content.len());
    }

    // -- to_prompt_context --

    #[test]
    fn test_fingerprint_to_prompt() {
        let fp = ProjectFingerprint {
            workspaces: vec![Workspace {
                path: ".".to_string(),
                config_files: vec![ConfigFile {
                    path: "Cargo.toml".to_string(),
                    content: "[package]\nname = \"test\"".to_string(),
                }],
                label: None,
            }],
            git_remotes: vec![GitRemote {
                name: "origin".to_string(),
                url: "https://github.com/org/repo.git".to_string(),
            }],
            ci_files: vec![ConfigFile {
                path: ".github/workflows/ci.yml".to_string(),
                content: "on: push".to_string(),
            }],
            build_files: vec![],
            has_monorepo_markers: false,
        };

        let tree = "Cargo.toml\nsrc/\n  main.rs\n";
        let ctx = fp.to_prompt_context(tree);

        assert!(
            ctx.contains("## Git Remotes"),
            "should have remotes section"
        );
        assert!(ctx.contains("origin"), "should list origin remote");
        assert!(
            ctx.contains("## Workspaces"),
            "should have workspaces section"
        );
        assert!(ctx.contains("Cargo.toml"), "should reference Cargo.toml");
        assert!(ctx.contains("## CI/CD"), "should have CI section");
        assert!(
            ctx.contains("## File Tree"),
            "should have file tree section"
        );
        assert!(!ctx.contains("Monorepo"), "should not have monorepo marker");
    }

    // -- test helper --

    fn init_test_repo(root: &Path) {
        use std::process::Command;
        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(root)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .unwrap();
    }
}
