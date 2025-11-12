use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use tracing::warn;

pub fn recent_paths(root: &Path) -> HashSet<String> {
    let mut paths = HashSet::new();
    if let Err(err) = collect_status(root, &mut paths) {
        warn!("git status failed: {err:?}");
    }
    paths
}

fn collect_status(root: &Path, paths: &mut HashSet<String>) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let path_part = line[3..].trim();
        let path = if let Some(idx) = path_part.find(" -> ") {
            &path_part[idx + 4..]
        } else {
            path_part
        };
        if path.is_empty() {
            continue;
        }
        paths.insert(path.replace('\\', "/"));
    }
    Ok(())
}
