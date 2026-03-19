use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const CONFIG_FILE: &str = ".pre-push.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct PrePushConfig {
    pub commands: Vec<PrePushCommand>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrePushCommand {
    pub name: String,
    pub run: String,
}

pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(CONFIG_FILE)
}

pub fn load_config(repo_root: &Path) -> Result<Option<PrePushConfig>> {
    let path = config_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let config: PrePushConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(Some(config))
}

pub fn show_config(repo_root: &Path) -> Result<()> {
    let config = load_config(repo_root)?;
    match config {
        Some(cfg) if !cfg.commands.is_empty() => {
            println!(
                "[pre-push] {} command(s) in {}:",
                cfg.commands.len(),
                config_path(repo_root).display()
            );
            for (i, cmd) in cfg.commands.iter().enumerate() {
                println!("  {}) {} — {}", i + 1, cmd.name, cmd.run);
            }
        }
        _ => {
            println!("[pre-push] No config found. Run with --init-pre-push to create one.");
        }
    }
    Ok(())
}

pub fn init_config(repo_root: &Path) -> Result<()> {
    let path = config_path(repo_root);
    if path.exists() {
        bail!("{CONFIG_FILE} already exists at {}", path.display());
    }

    let commands = default_commands(repo_root);
    let config = PrePushConfig { commands };

    let content =
        serde_json::to_string_pretty(&config).context("failed to serialize default config")?;

    std::fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    println!("Created {CONFIG_FILE} at {}", path.display());
    println!("Edit it to customize your pre-push checks.");
    Ok(())
}

fn default_commands(repo_root: &Path) -> Vec<PrePushCommand> {
    if repo_root.join("Cargo.toml").exists() {
        vec![
            PrePushCommand {
                name: "tests".into(),
                run: "cargo test".into(),
            },
            PrePushCommand {
                name: "lint".into(),
                run: "cargo clippy -- -D warnings".into(),
            },
            PrePushCommand {
                name: "format check".into(),
                run: "cargo fmt -- --check".into(),
            },
        ]
    } else if repo_root.join("package.json").exists() {
        vec![
            PrePushCommand {
                name: "tests".into(),
                run: "npm test".into(),
            },
            PrePushCommand {
                name: "lint".into(),
                run: "npm run lint".into(),
            },
        ]
    } else if repo_root.join("pyproject.toml").exists() || repo_root.join("setup.py").exists() {
        vec![
            PrePushCommand {
                name: "tests".into(),
                run: "python -m pytest".into(),
            },
            PrePushCommand {
                name: "lint".into(),
                run: "python -m ruff check .".into(),
            },
        ]
    } else if repo_root.join("go.mod").exists() {
        vec![
            PrePushCommand {
                name: "tests".into(),
                run: "go test ./...".into(),
            },
            PrePushCommand {
                name: "vet".into(),
                run: "go vet ./...".into(),
            },
        ]
    } else {
        vec![PrePushCommand {
            name: "example".into(),
            run: "echo 'Replace with your pre-push checks'".into(),
        }]
    }
}

pub fn run_pre_push(config: &PrePushConfig, dry_run: bool) -> Result<()> {
    if config.commands.is_empty() {
        return Ok(());
    }

    let total = config.commands.len();
    println!("[pre-push] Running {total} check(s)...");

    for (i, cmd) in config.commands.iter().enumerate() {
        let step = i + 1;
        println!("[pre-push] [{step}/{total}] {}...", cmd.name);

        if dry_run {
            println!("[pre-push] [dry-run] Would run: {}", cmd.run);
            continue;
        }

        let status = Command::new("sh")
            .args(["-c", &cmd.run])
            .status()
            .with_context(|| format!("failed to run: {}", cmd.run))?;

        if !status.success() {
            bail!(
                "pre-push check '{}' failed (exit {}).\n\
                 Command: {}\n\
                 Push aborted. Fix the issue and try again.",
                cmd.name,
                status.code().unwrap_or(-1),
                cmd.run
            );
        }

        println!("[pre-push] [{step}/{total}] {} passed", cmd.name);
    }

    println!("[pre-push] All checks passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_config_path() {
        let root = Path::new("/tmp/repo");
        assert_eq!(config_path(root), PathBuf::from("/tmp/repo/.pre-push.json"));
    }

    #[test]
    fn test_load_config_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_config(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_config_valid() {
        let dir = tempfile::tempdir().unwrap();
        let config_json = r#"{
            "commands": [
                {"name": "tests", "run": "cargo test"},
                {"name": "lint", "run": "cargo clippy"}
            ]
        }"#;
        fs::write(dir.path().join(".pre-push.json"), config_json).unwrap();

        let config = load_config(dir.path()).unwrap().unwrap();
        assert_eq!(config.commands.len(), 2);
        assert_eq!(config.commands[0].name, "tests");
        assert_eq!(config.commands[0].run, "cargo test");
        assert_eq!(config.commands[1].name, "lint");
        assert_eq!(config.commands[1].run, "cargo clippy");
    }

    #[test]
    fn test_load_config_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".pre-push.json"), "not json").unwrap();

        let result = load_config(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_init_config_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();

        let path = dir.path().join(".pre-push.json");
        assert!(path.exists());

        let content = fs::read_to_string(&path).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert!(!config.commands.is_empty());
    }

    #[test]
    fn test_init_config_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".pre-push.json"), "{}").unwrap();

        let result = init_config(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_init_config_detects_rust() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".pre-push.json")).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.commands.len(), 3);
        assert!(config.commands.iter().any(|c| c.run.contains("cargo test")));
        assert!(
            config
                .commands
                .iter()
                .any(|c| c.run.contains("cargo clippy"))
        );
        assert!(config.commands.iter().any(|c| c.run.contains("cargo fmt")));
    }

    #[test]
    fn test_init_config_detects_node() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".pre-push.json")).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.commands.len(), 2);
        assert!(config.commands.iter().any(|c| c.run.contains("npm test")));
        assert!(
            config
                .commands
                .iter()
                .any(|c| c.run.contains("npm run lint"))
        );
    }

    #[test]
    fn test_init_config_detects_python() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".pre-push.json")).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert!(config.commands.iter().any(|c| c.run.contains("pytest")));
    }

    #[test]
    fn test_init_config_detects_go() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module test").unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".pre-push.json")).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert!(config.commands.iter().any(|c| c.run.contains("go test")));
    }

    #[test]
    fn test_init_config_generic_fallback() {
        let dir = tempfile::tempdir().unwrap();
        init_config(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".pre-push.json")).unwrap();
        let config: PrePushConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(config.commands.len(), 1);
        assert_eq!(config.commands[0].name, "example");
    }

    #[test]
    fn test_run_pre_push_empty_commands() {
        let config = PrePushConfig { commands: vec![] };
        run_pre_push(&config, false).unwrap();
    }

    #[test]
    fn test_run_pre_push_success() {
        let config = PrePushConfig {
            commands: vec![PrePushCommand {
                name: "trivial check".into(),
                run: "true".into(),
            }],
        };
        run_pre_push(&config, false).unwrap();
    }

    #[test]
    fn test_run_pre_push_failure_aborts() {
        let config = PrePushConfig {
            commands: vec![PrePushCommand {
                name: "failing check".into(),
                run: "false".into(),
            }],
        };
        let result = run_pre_push(&config, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failing check"));
        assert!(err.contains("Push aborted"));
    }

    #[test]
    fn test_run_pre_push_dry_run_skips_execution() {
        let config = PrePushConfig {
            commands: vec![PrePushCommand {
                name: "would fail".into(),
                run: "false".into(),
            }],
        };
        // dry_run should not actually execute the command, so it should succeed
        run_pre_push(&config, true).unwrap();
    }

    #[test]
    fn test_run_pre_push_stops_on_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("should_not_exist");

        let config = PrePushConfig {
            commands: vec![
                PrePushCommand {
                    name: "fail first".into(),
                    run: "false".into(),
                },
                PrePushCommand {
                    name: "should not run".into(),
                    run: format!("touch {}", marker.display()),
                },
            ],
        };

        let result = run_pre_push(&config, false);
        assert!(result.is_err());
        assert!(!marker.exists(), "second command should not have run");
    }

    #[test]
    fn test_show_config_no_file() {
        let dir = tempfile::tempdir().unwrap();
        // Should succeed and print "No config found" message
        show_config(dir.path()).unwrap();
    }

    #[test]
    fn test_show_config_with_commands() {
        let dir = tempfile::tempdir().unwrap();
        let config_json = r#"{
            "commands": [
                {"name": "tests", "run": "cargo test"},
                {"name": "lint", "run": "cargo clippy"}
            ]
        }"#;
        fs::write(dir.path().join(".pre-push.json"), config_json).unwrap();
        show_config(dir.path()).unwrap();
    }

    #[test]
    fn test_show_config_empty_commands() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".pre-push.json"), r#"{"commands": []}"#).unwrap();
        // Empty commands should show "No config found" message
        show_config(dir.path()).unwrap();
    }

    #[test]
    fn test_roundtrip_serialization() {
        let config = PrePushConfig {
            commands: vec![
                PrePushCommand {
                    name: "a".into(),
                    run: "cmd_a".into(),
                },
                PrePushCommand {
                    name: "b".into(),
                    run: "cmd_b".into(),
                },
            ],
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: PrePushConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.commands.len(), 2);
        assert_eq!(parsed.commands[0].name, "a");
        assert_eq!(parsed.commands[1].run, "cmd_b");
    }
}
