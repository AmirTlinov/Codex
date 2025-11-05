use crate::OperationStatus;
use crate::OperationSummary;
use crate::TaskStatus;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Instant;
use which::which;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattingOutcome {
    pub tool: String,
    pub scope: Option<String>,
    pub status: TaskStatus,
    pub duration_ms: u128,
    pub files: Vec<PathBuf>,
    pub note: Option<String>,
}

pub fn run_auto_formatters(root: &Path, operations: &[OperationSummary]) -> Vec<FormattingOutcome> {
    let applied: Vec<&OperationSummary> = operations
        .iter()
        .filter(|op| op.status == OperationStatus::Applied)
        .collect();
    if applied.is_empty() {
        return Vec::new();
    }

    let mut outcomes = Vec::new();
    outcomes.extend(run_cargo_fmt(root, &applied));
    outcomes.extend(run_gofmt(root, &applied));
    outcomes.extend(run_prettier(root, &applied));
    outcomes.extend(run_swift_format(root, &applied));
    outcomes.extend(run_php_cs_fixer(root, &applied));
    outcomes
}

fn run_cargo_fmt(root: &Path, operations: &[&OperationSummary]) -> Vec<FormattingOutcome> {
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
        let files: Vec<PathBuf> = manifests
            .values()
            .flat_map(|set| set.iter().cloned())
            .collect();
        return vec![FormattingOutcome {
            tool: "cargo fmt".to_string(),
            scope: None,
            status: TaskStatus::Skipped,
            duration_ms: 0,
            files,
            note: Some("cargo not found on PATH".to_string()),
        }];
    }

    let mut outcomes = Vec::new();
    for (manifest, files) in manifests {
        let mut cmd = Command::new("cargo");
        cmd.arg("fmt").arg("--manifest-path").arg(&manifest);
        if let Some(parent) = manifest.parent() {
            cmd.current_dir(parent);
        } else {
            cmd.current_dir(root);
        }
        let result = run_command(cmd);
        let scope = manifest
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        outcomes.push(FormattingOutcome {
            tool: "cargo fmt".to_string(),
            scope,
            status: result.status,
            duration_ms: result.duration_ms,
            files: files.into_iter().collect(),
            note: result.note,
        });
    }
    outcomes
}

fn run_gofmt(root: &Path, operations: &[&OperationSummary]) -> Vec<FormattingOutcome> {
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

    if which("gofmt").is_err() {
        let files: Vec<PathBuf> = modules
            .values()
            .flat_map(|set| set.iter().cloned())
            .collect();
        return vec![FormattingOutcome {
            tool: "gofmt".to_string(),
            scope: None,
            status: TaskStatus::Skipped,
            duration_ms: 0,
            files,
            note: Some("gofmt not found on PATH".to_string()),
        }];
    }

    let mut outcomes = Vec::new();
    for (module, files) in modules {
        let mut cmd = Command::new("gofmt");
        cmd.arg("-w");
        for file in &files {
            cmd.arg(file);
        }
        cmd.current_dir(&module);
        let result = run_command(cmd);
        outcomes.push(FormattingOutcome {
            tool: "gofmt".to_string(),
            scope: module
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string()),
            status: result.status,
            duration_ms: result.duration_ms,
            files: files.into_iter().collect(),
            note: result.note,
        });
    }
    outcomes
}

fn run_prettier(root: &Path, operations: &[&OperationSummary]) -> Vec<FormattingOutcome> {
    const EXTENSIONS: &[&str] = &["js", "jsx", "ts", "tsx", "json", "md"]; // minimal set
    let mut projects: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for op in operations {
        let ext = match op.path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => ext,
            None => continue,
        };
        if !EXTENSIONS.contains(&ext) {
            continue;
        }
        let project_root = find_upwards(&op.path, "package.json")
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| {
                op.path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.to_path_buf())
            });
        projects
            .entry(project_root)
            .or_default()
            .insert(op.path.clone());
    }

    if projects.is_empty() {
        return Vec::new();
    }

    let mut outcomes = Vec::new();
    for (project, files) in projects {
        let (program, args_prefix) = match detect_prettier_command(&project) {
            Some(value) => value,
            None => {
                outcomes.push(FormattingOutcome {
                    tool: "prettier".to_string(),
                    scope: project
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string()),
                    status: TaskStatus::Skipped,
                    duration_ms: 0,
                    files: files.iter().cloned().collect(),
                    note: Some("prettier not available".to_string()),
                });
                continue;
            }
        };

        let mut cmd = Command::new(program);
        for arg in args_prefix {
            cmd.arg(arg);
        }
        cmd.arg("--write");
        for file in &files {
            cmd.arg(file);
        }
        cmd.current_dir(&project);
        let result = run_command(cmd);
        outcomes.push(FormattingOutcome {
            tool: "prettier".to_string(),
            scope: project
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string()),
            status: result.status,
            duration_ms: result.duration_ms,
            files: files.into_iter().collect(),
            note: result.note,
        });
    }
    outcomes
}

fn run_swift_format(root: &Path, operations: &[&OperationSummary]) -> Vec<FormattingOutcome> {
    let mut modules: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for op in operations {
        if op.path.extension().and_then(|ext| ext.to_str()) != Some("swift") {
            continue;
        }
        let module_root = find_upwards(&op.path, "Package.swift")
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

    if which("swift-format").is_err() {
        let files: Vec<PathBuf> = modules
            .values()
            .flat_map(|set| set.iter().cloned())
            .collect();
        return vec![FormattingOutcome {
            tool: "swift-format".to_string(),
            scope: None,
            status: TaskStatus::Skipped,
            duration_ms: 0,
            files,
            note: Some("swift-format not found on PATH".to_string()),
        }];
    }

    let mut outcomes = Vec::new();
    for (module, files) in modules {
        let mut cmd = Command::new("swift-format");
        cmd.arg("format").arg("--in-place");
        for file in &files {
            cmd.arg(file);
        }
        cmd.current_dir(&module);
        let result = run_command(cmd);
        outcomes.push(FormattingOutcome {
            tool: "swift-format".to_string(),
            scope: module
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string()),
            status: result.status,
            duration_ms: result.duration_ms,
            files: files.into_iter().collect(),
            note: result.note,
        });
    }
    outcomes
}

fn run_php_cs_fixer(root: &Path, operations: &[&OperationSummary]) -> Vec<FormattingOutcome> {
    let mut projects: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for op in operations {
        if op.path.extension().and_then(|ext| ext.to_str()) != Some("php") {
            continue;
        }
        let project_root = op
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf());
        projects
            .entry(project_root)
            .or_default()
            .insert(op.path.clone());
    }

    if projects.is_empty() {
        return Vec::new();
    }

    if which("php-cs-fixer").is_err() {
        let files: Vec<PathBuf> = projects
            .values()
            .flat_map(|set| set.iter().cloned())
            .collect();
        return vec![FormattingOutcome {
            tool: "php-cs-fixer".to_string(),
            scope: None,
            status: TaskStatus::Skipped,
            duration_ms: 0,
            files,
            note: Some("php-cs-fixer not found on PATH".to_string()),
        }];
    }

    let mut outcomes = Vec::new();
    for (project, files) in projects {
        let mut cmd = Command::new("php-cs-fixer");
        cmd.arg("fix");
        cmd.current_dir(&project);
        let result = run_command(cmd);
        outcomes.push(FormattingOutcome {
            tool: "php-cs-fixer".to_string(),
            scope: project
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string()),
            status: result.status,
            duration_ms: result.duration_ms,
            files: files.into_iter().collect(),
            note: result.note,
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

fn detect_prettier_command(project: &Path) -> Option<(String, Vec<String>)> {
    let local = project.join("node_modules").join(".bin").join("prettier");
    if local.exists() {
        return Some((local.to_string_lossy().into_owned(), Vec::new()));
    }
    if which("prettier").is_ok() {
        return Some(("prettier".to_string(), Vec::new()));
    }
    if which("pnpm").is_ok() {
        return Some((
            "pnpm".to_string(),
            vec!["dlx".to_string(), "prettier".to_string()],
        ));
    }
    None
}

struct CommandOutcome {
    status: TaskStatus,
    duration_ms: u128,
    note: Option<String>,
}

fn run_command(mut cmd: Command) -> CommandOutcome {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    match cmd.output() {
        Ok(output) => {
            let duration_ms = start.elapsed().as_millis();
            let stderr = if output.stderr.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stderr).trim().to_string())
            };
            let note = if output.status.success() {
                stderr.filter(|s| !s.is_empty())
            } else {
                Some(
                    stderr
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "formatter command failed".to_string()),
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
            }
        }
        Err(err) => CommandOutcome {
            status: TaskStatus::Failed,
            duration_ms: start.elapsed().as_millis(),
            note: Some(err.to_string()),
        },
    }
}
