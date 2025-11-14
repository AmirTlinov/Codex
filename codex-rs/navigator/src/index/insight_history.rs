use crate::project::ProjectProfile;
use crate::proto::InsightSectionKind;
use crate::proto::InsightTrend;
use crate::proto::InsightTrendSummary;
use crate::proto::InsightsResponse;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use time::OffsetDateTime;
use tokio::fs;
use tokio::sync::Mutex;

const HISTORY_LIMIT: usize = 6;
const PATH_LIMIT: usize = 16;

pub(crate) struct InsightHistoryStore {
    state: Mutex<InsightHistoryState>,
    path: PathBuf,
    tmp_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct InsightHistoryState {
    #[serde(default)]
    entries: Vec<InsightHistoryEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InsightHistoryEntry {
    recorded_at: OffsetDateTime,
    sections: Vec<InsightHistorySection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InsightHistorySection {
    kind: InsightSectionKind,
    paths: Vec<String>,
}

impl InsightHistoryStore {
    pub(crate) fn new(profile: &ProjectProfile) -> Result<Self> {
        let path = profile.insights_path();
        let tmp_path = profile.temp_insights_path();
        let state = if path.exists() {
            load_entries(&path)?
        } else {
            InsightHistoryState::default()
        };
        Ok(Self {
            state: Mutex::new(state),
            path,
            tmp_path,
        })
    }

    pub(crate) async fn record(
        &self,
        response: &InsightsResponse,
    ) -> Result<Option<InsightTrendSummary>> {
        let mut guard = self.state.lock().await;
        let entry = InsightHistoryEntry::from_response(response);
        let prev = guard.entries.last().cloned();
        guard.entries.push(entry.clone());
        if guard.entries.len() > HISTORY_LIMIT {
            let overflow = guard.entries.len() - HISTORY_LIMIT;
            guard.entries.drain(0..overflow);
        }
        persist_entries(&self.tmp_path, &self.path, &guard).await?;
        Ok(prev.map(|previous| summarize_trend(&previous, &entry)))
    }

    pub(crate) async fn latest_summary(&self) -> Option<InsightTrendSummary> {
        let guard = self.state.lock().await;
        if guard.entries.len() < 2 {
            return None;
        }
        let current = guard.entries.last()?;
        let previous = guard.entries.get(guard.entries.len().saturating_sub(2))?;
        Some(summarize_trend(previous, current))
    }
}

impl InsightHistoryEntry {
    fn from_response(response: &InsightsResponse) -> Self {
        Self {
            recorded_at: response.generated_at,
            sections: response
                .sections
                .iter()
                .map(|section| InsightHistorySection {
                    kind: section.kind,
                    paths: section
                        .items
                        .iter()
                        .map(|item| item.path.clone())
                        .take(PATH_LIMIT)
                        .collect(),
                })
                .collect(),
        }
    }
}

fn summarize_trend(
    previous: &InsightHistoryEntry,
    current: &InsightHistoryEntry,
) -> InsightTrendSummary {
    let prev_map = to_map(previous);
    let curr_map = to_map(current);
    let mut trends = Vec::new();
    for (kind, paths) in &curr_map {
        let prev_paths = prev_map.get(kind).cloned().unwrap_or_default();
        let trend = build_trend(*kind, &prev_paths, paths);
        if !trend.new_paths.is_empty() || !trend.resolved_paths.is_empty() {
            trends.push(trend);
        }
    }
    for (kind, prev_paths) in &prev_map {
        if curr_map.contains_key(kind) {
            continue;
        }
        let trend = build_trend(*kind, prev_paths, &HashSet::new());
        if !trend.resolved_paths.is_empty() {
            trends.push(trend);
        }
    }
    InsightTrendSummary {
        recorded_at: current.recorded_at,
        trends,
    }
}

fn build_trend(
    kind: InsightSectionKind,
    prev: &HashSet<String>,
    curr: &HashSet<String>,
) -> InsightTrend {
    let mut new_paths = curr
        .difference(prev)
        .take(PATH_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    new_paths.sort();
    let mut resolved = prev
        .difference(curr)
        .take(PATH_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    resolved.sort();
    InsightTrend {
        kind,
        new_paths,
        resolved_paths: resolved,
    }
}

fn to_map(entry: &InsightHistoryEntry) -> HashMap<InsightSectionKind, HashSet<String>> {
    let mut map = HashMap::new();
    for section in &entry.sections {
        map.insert(section.kind, section.paths.iter().cloned().collect());
    }
    map
}

fn load_entries(path: &PathBuf) -> Result<InsightHistoryState> {
    let data = std::fs::read(path).context("read insights history")?;
    bincode::deserialize(&data).context("decode insights history")
}

async fn persist_entries(
    tmp_path: &PathBuf,
    path: &PathBuf,
    state: &InsightHistoryState,
) -> Result<()> {
    let data = bincode::serialize(state)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(tmp_path, &data).await?;
    fs::rename(tmp_path, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InsightSection;
    use crate::proto::NavigatorInsight;

    #[test]
    fn summarize_reports_new_and_resolved() {
        let prev = InsightHistoryEntry {
            recorded_at: OffsetDateTime::UNIX_EPOCH,
            sections: vec![InsightHistorySection {
                kind: InsightSectionKind::AttentionHotspots,
                paths: vec!["src/a.rs".into(), "src/b.rs".into()],
            }],
        };
        let current = InsightHistoryEntry {
            recorded_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60),
            sections: vec![InsightHistorySection {
                kind: InsightSectionKind::AttentionHotspots,
                paths: vec!["src/b.rs".into(), "src/c.rs".into()],
            }],
        };
        let summary = summarize_trend(&prev, &current);
        assert_eq!(summary.trends.len(), 1);
        let trend = &summary.trends[0];
        assert_eq!(trend.new_paths, vec!["src/c.rs"]);
        assert_eq!(trend.resolved_paths, vec!["src/a.rs"]);
    }

    #[test]
    fn entry_derives_from_response() {
        let response = InsightsResponse {
            generated_at: OffsetDateTime::UNIX_EPOCH,
            sections: vec![InsightSection {
                kind: InsightSectionKind::LintRisks,
                title: "lint".into(),
                summary: None,
                items: vec![NavigatorInsight {
                    path: "src/lib.rs".into(),
                    score: 1.0,
                    reasons: vec!["lint".into()],
                    owners: vec![],
                    categories: vec![],
                    line_count: 10,
                    attention: 0,
                    attention_density: 0,
                    lint_suppressions: 1,
                    lint_density: 2,
                    churn: 0,
                    freshness_days: 1,
                    recent: true,
                }],
            }],
            trend_summary: None,
        };
        let entry = InsightHistoryEntry::from_response(&response);
        assert_eq!(entry.sections.len(), 1);
        assert_eq!(entry.sections[0].paths, vec!["src/lib.rs"]);
    }
}
