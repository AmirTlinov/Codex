use crate::OperationStatus;
use crate::OperationSummary;
use crate::TaskStatus;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Instant;
use toml::Value;
use which::which;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostCheckOutcome {
    pub name: String,
    pub command: Vec<String>,
    pub status: TaskStatus,
    pub duration_ms: u128,
    pub cwd: PathBuf,
    pub note: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

pub fn run_post_checks(root: &Path, operations: &[OperationSummary]) -> Vec<PostCheckOutcome> {
    let applied: Vec<&OperationSummary> = operations
        .iter()
        .filter(|op| op.status == OperationStatus::Applied)
        .collect();
    if applied.is_empty() {
        return Vec::new();
    }

    let mut outcomes = Vec::new();
    outcomes.extend(run_cargo_tests(root, &applied));
    outcomes.extend(run_go_tests(root, &applied));
    outcomes
}

fn run_cargo_tests(root: &Path, operations: &[&OperationSummary]) -> Vec<PostCheckOutcome> {
    let mut manifests: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for op in operations {
        if op.path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        if let Some(manifest) = find_upwards(&op.path, "Cargo.toml") {
            manifests
                .entry(manifest)
                .or_default()
                .insert(op.path.clone());
        }
    }

    if manifests.is_empty() {
        return Vec::new();
    }

    if which("cargo").is_err() {
        return vec![PostCheckOutcome {
            name: "cargo test".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
            status: TaskStatus::Skipped,
            duration_ms: 0,
            cwd: root.to_path_buf(),
            note: Some("cargo not found on PATH".to_string()),
            stdout: None,
            stderr: None,
        }];
    }

    let mut crate_targets = Vec::new();
    for manifest in manifests.keys() {
        if let Some(name) = read_crate_name(manifest) {
            crate_targets.push((manifest.clone(), name));
        }
    }

    if crate_targets.is_empty() {
        return Vec::new();
    }

    let mut outcomes = Vec::new();
    if crate_targets.len() <= 2 {
        for (manifest, crate_name) in crate_targets {
            let mut cmd = Command::new("cargo");
            cmd.arg("test").arg("-p").arg(&crate_name).arg("--quiet");
            if let Some(parent) = manifest.parent() {
                cmd.current_dir(parent);
            } else {
                cmd.current_dir(root);
            }
            let result = run_command_capture(cmd);
            outcomes.push(PostCheckOutcome {
                name: format!("cargo test -p {crate_name}"),
                command: vec![
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    crate_name.clone(),
                    "--quiet".to_string(),
                ],
                status: result.status,
                duration_ms: result.duration_ms,
                cwd: manifest
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.to_path_buf()),
                note: result.note,
                stdout: result.stdout,
                stderr: result.stderr,
            });
        }
    } else {
        let manifest = crate_targets[0]
            .0
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf());
        let mut cmd = Command::new("cargo");
        cmd.arg("test").arg("--workspace").arg("--quiet");
        cmd.current_dir(&manifest);
        let result = run_command_capture(cmd);
        outcomes.push(PostCheckOutcome {
            name: "cargo test --workspace".to_string(),
            command: vec![
                "cargo".to_string(),
                "test".to_string(),
                "--workspace".to_string(),
                "--quiet".to_string(),
            ],
            status: result.status,
            duration_ms: result.duration_ms,
            cwd: manifest,
            note: result.note,
            stdout: result.stdout,
            stderr: result.stderr,
        });
    }

    outcomes
}

fn run_go_tests(root: &Path, operations: &[&OperationSummary]) -> Vec<PostCheckOutcome> {
    let mut modules: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for op in operations {
        if op.path.extension().and_then(|ext| ext.to_str()) != Some("go") {
            continue;
        }
        let module_root = find_upwards(&op.path, "go.mod")
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| {
                op.path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.to_path_buf())
            });
        modules
            .entry(module_root)
            .or_default()
            .insert(op.path.clone());
    }

    if modules.is_empty() {
        return Vec::new();
    }

    if which("go").is_err() {
        return vec![PostCheckOutcome {
            name: "go test".to_string(),
            command: vec!["go".to_string(), "test".to_string(), "./...".to_string()],
            status: TaskStatus::Skipped,
            duration_ms: 0,
            cwd: root.to_path_buf(),
            note: Some("go not found on PATH".to_string()),
            stdout: None,
            stderr: None,
        }];
    }

    let mut outcomes = Vec::new();
    for (module_root, _files) in modules {
        let mut cmd = Command::new("go");
        cmd.arg("test").arg("./...");
        cmd.current_dir(&module_root);
        let result = run_command_capture(cmd);
        outcomes.push(PostCheckOutcome {
            name: "go test ./...".to_string(),
            command: vec!["go".to_string(), "test".to_string(), "./...".to_string()],
            status: result.status,
            duration_ms: result.duration_ms,
            cwd: module_root,
            note: result.note,
            stdout: result.stdout,
            stderr: result.stderr,
        });
    }
    outcomes
}

fn find_upwards(start: &Path, marker: &str) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    if current.is_file() {
        current = current.parent().map(PathBuf::from)?;
    }
    loop {
        let candidate = current.join(marker);
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn read_crate_name(manifest: &Path) -> Option<String> {
    let contents = fs::read_to_string(manifest).ok()?;
    let value: Value = toml::from_str(&contents).ok()?;
    value
        .get("package")
        .and_then(|pkg| pkg.get("name"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

struct CommandOutcome {
    status: TaskStatus,
    duration_ms: u128,
    note: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

fn run_command_capture(mut cmd: Command) -> CommandOutcome {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    match cmd.output() {
        Ok(output) => {
            let duration_ms = start.elapsed().as_millis();
            let stdout = if output.stdout.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            };
            let stderr = if output.stderr.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stderr).trim().to_string())
            };
            let note = if output.status.success() {
                stderr.clone()
            } else {
                Some(
                    stderr
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "post-check command failed".to_string()),
                )
            };
            CommandOutcome {
                status: if output.status.success() {
                    TaskStatus::Applied
                } else {
                    TaskStatus::Failed
                },
                duration_ms,
                note,
                stdout,
                stderr,
            }
        }
        Err(err) => CommandOutcome {
            status: TaskStatus::Failed,
            duration_ms: start.elapsed().as_millis(),
            note: Some(err.to_string()),
            stdout: None,
            stderr: None,
        },
    }
}
