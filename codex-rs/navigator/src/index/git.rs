use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tracing::warn;

pub fn recent_paths(root: &Path) -> HashSet<String> {
    let mut paths = HashSet::new();
    if let Err(err) = collect_status(root, &mut paths) {
        warn!("git status failed: {err:?}");
    }
    paths
}

pub fn churn_scores(root: &Path) -> HashMap<String, u32> {
    let mut scores = HashMap::new();
    if let Err(err) = collect_churn(root, &mut scores) {
        warn!("git churn scan failed: {err:?}");
    }
    scores
}

pub fn recency_days(root: &Path) -> HashMap<String, u32> {
    let mut last_commit = HashMap::new();
    if let Err(err) = collect_last_commit(root, &mut last_commit) {
        warn!("git recency scan failed: {err:?}");
    }
    normalize_recency(last_commit)
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

fn collect_churn(root: &Path, scores: &mut HashMap<String, u32>) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("log")
        .arg("--since=30.days")
        .arg("--name-only")
        .arg("--pretty=format:")
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.replace('\\', "/");
        let counter = scores.entry(normalized).or_insert(0);
        *counter = counter.saturating_add(1);
    }
    Ok(())
}

fn collect_last_commit(root: &Path, entries: &mut HashMap<String, i64>) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("log")
        .arg("--pretty=format:%ct")
        .arg("--name-only")
        .arg("--since=120.days")
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(());
    }
    let mut current_ts: Option<i64> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
            current_ts = trimmed.parse::<i64>().ok();
            continue;
        }
        if let Some(ts) = current_ts {
            let normalized = trimmed.replace('\\', "/");
            entries.entry(normalized).or_insert(ts);
        }
    }
    Ok(())
}

fn normalize_recency(entries: HashMap<String, i64>) -> HashMap<String, u32> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0);
    entries
        .into_iter()
        .map(|(path, ts)| {
            let age_secs = now_secs.saturating_sub(ts.max(0));
            let days = (age_secs / 86_400).clamp(0, 365) as u32;
            (path, days)
        })
        .collect()
}
