use crate::project::ProjectProfile;
use crate::proto::CoverageDiagnostics;
use crate::proto::CoverageGap;
use crate::proto::HealthIssue;
use crate::proto::HealthPanel;
use crate::proto::HealthRisk;
use crate::proto::HealthSummary;
use crate::proto::IngestKind;
use crate::proto::IngestRunSummary;
use crate::proto::LiteralStatsSummary;
use crate::proto::SearchStats;
use crate::proto::SkippedReasonSummary;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::fs as async_fs;
use tokio::sync::Mutex;

const MAX_INGEST_HISTORY: usize = 8;
const MAX_SCAN_SAMPLES: usize = 64;
const SEARCH_PERSIST_INTERVAL: u32 = 32;
const STALE_INGEST_YELLOW_HOURS: i64 = 24;
const STALE_INGEST_RED_HOURS: i64 = 72;
const LITERAL_RATE_YELLOW: f32 = 0.45;
const LITERAL_RATE_RED: f32 = 0.7;
const MIN_LITERAL_SAMPLE: u64 = 12;
const COVERAGE_PENDING_THRESHOLD: usize = 16;

pub(crate) struct HealthStore {
    state: Mutex<HealthState>,
    path: PathBuf,
    tmp_path: PathBuf,
}

struct HealthState {
    snapshot: HealthSnapshot,
    dirty_events: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub(crate) struct HealthSnapshot {
    #[serde(default)]
    ingest_history: Vec<IngestRun>,
    #[serde(default)]
    literal: LiteralTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IngestRun {
    kind: IngestKind,
    completed_at: OffsetDateTime,
    duration_ms: u64,
    files_indexed: usize,
    skipped_total: usize,
    #[serde(default)]
    skipped_reasons: Vec<SkippedReasonSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct LiteralTotals {
    total_queries: u64,
    literal_fallbacks: u64,
    scanned_files: u64,
    scanned_bytes: u64,
    #[serde(default)]
    scan_samples: Vec<u64>,
}

impl HealthStore {
    pub(crate) fn new(profile: &ProjectProfile) -> Result<Self> {
        let path = profile.health_path();
        let tmp_path = profile.temp_health_path();
        let snapshot = if path.exists() {
            load_snapshot(&path)?
        } else {
            HealthSnapshot::default()
        };
        Ok(Self {
            state: Mutex::new(HealthState {
                snapshot,
                dirty_events: 0,
            }),
            path,
            tmp_path,
        })
    }

    pub(crate) async fn record_ingest(
        &self,
        kind: IngestKind,
        duration: Duration,
        files_indexed: usize,
        skipped: &[CoverageGap],
    ) -> Result<()> {
        let completed_at = OffsetDateTime::now_utc();
        let duration_ms = duration.as_millis().min(u64::MAX as u128) as u64;
        let mut buckets: HashMap<_, usize> = HashMap::new();
        for gap in skipped {
            *buckets.entry(gap.reason.clone()).or_insert(0) += 1;
        }
        let skipped_reasons = buckets
            .into_iter()
            .map(|(reason, count)| SkippedReasonSummary { reason, count })
            .collect();
        let mut guard = self.state.lock().await;
        let run = IngestRun {
            kind,
            completed_at,
            duration_ms,
            files_indexed,
            skipped_total: skipped.len(),
            skipped_reasons,
        };
        guard.snapshot.ingest_history.push(run);
        if guard.snapshot.ingest_history.len() > MAX_INGEST_HISTORY {
            let overflow = guard.snapshot.ingest_history.len() - MAX_INGEST_HISTORY;
            guard.snapshot.ingest_history.drain(0..overflow);
        }
        guard.dirty_events = 0;
        let snapshot = guard.snapshot.clone();
        drop(guard);
        self.persist(&snapshot).await?;
        Ok(())
    }

    pub(crate) async fn record_search(&self, stats: &SearchStats) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard.snapshot.literal.total_queries += 1;
        if stats.literal_fallback {
            guard.snapshot.literal.literal_fallbacks += 1;
        }
        if let Some(files) = stats.literal_scanned_files {
            guard.snapshot.literal.scanned_files = guard
                .snapshot
                .literal
                .scanned_files
                .saturating_add(files as u64);
        }
        if let Some(bytes) = stats.literal_scanned_bytes {
            guard.snapshot.literal.scanned_bytes =
                guard.snapshot.literal.scanned_bytes.saturating_add(bytes);
        }
        if let Some(micros) = stats.literal_scan_micros {
            guard.snapshot.literal.scan_samples.push(micros);
            if guard.snapshot.literal.scan_samples.len() > MAX_SCAN_SAMPLES {
                let overflow = guard.snapshot.literal.scan_samples.len() - MAX_SCAN_SAMPLES;
                guard.snapshot.literal.scan_samples.drain(0..overflow);
            }
        }
        guard.dirty_events = guard.dirty_events.saturating_add(1);
        let snapshot = if guard.dirty_events >= SEARCH_PERSIST_INTERVAL {
            guard.dirty_events = 0;
            Some(guard.snapshot.clone())
        } else {
            None
        };
        drop(guard);
        if let Some(snapshot) = snapshot {
            self.persist(&snapshot).await?;
        }
        Ok(())
    }

    pub(crate) async fn panel(&self, coverage: &CoverageDiagnostics) -> HealthPanel {
        let snapshot = {
            let guard = self.state.lock().await;
            guard.snapshot.clone()
        };
        build_panel(snapshot, coverage)
    }

    pub(crate) async fn summary(&self, coverage: &CoverageDiagnostics) -> HealthSummary {
        let panel = self.panel(coverage).await;
        HealthSummary {
            risk: panel.risk,
            issues: panel.issues,
        }
    }

    async fn persist(&self, snapshot: &HealthSnapshot) -> Result<()> {
        let data = bincode::serialize(snapshot)?;
        if let Some(parent) = self.path.parent() {
            async_fs::create_dir_all(parent).await?;
        }
        async_fs::write(&self.tmp_path, &data).await?;
        async_fs::rename(&self.tmp_path, &self.path).await?;
        Ok(())
    }
}

fn build_panel(snapshot: HealthSnapshot, coverage: &CoverageDiagnostics) -> HealthPanel {
    let ingest = snapshot
        .ingest_history
        .iter()
        .map(|run| IngestRunSummary {
            kind: run.kind,
            completed_at: Some(run.completed_at),
            duration_ms: run.duration_ms,
            files_indexed: run.files_indexed,
            skipped_total: run.skipped_total,
            skipped_reasons: run.skipped_reasons.clone(),
        })
        .collect();
    let literal = summarize_literal(&snapshot.literal);
    let mut issues = Vec::new();
    let now = OffsetDateTime::now_utc();
    match snapshot.ingest_history.last() {
        Some(run) => {
            let diff = now - run.completed_at;
            let hours = diff.whole_hours();
            if hours >= STALE_INGEST_RED_HOURS {
                issues.push(HealthIssue {
                    level: HealthRisk::Red,
                    message: format!("last ingest {hours}h ago"),
                    remediation: Some(
                        "run `codex navigator daemon --project-root <repo>` to trigger a rebuild"
                            .to_string(),
                    ),
                });
            } else if hours >= STALE_INGEST_YELLOW_HOURS {
                issues.push(HealthIssue {
                    level: HealthRisk::Yellow,
                    message: format!("last ingest {hours}h ago"),
                    remediation: Some("kick off `codex navigator daemon` or `navigator daemon --project-root <repo>`".to_string()),
                });
            }
        }
        None => issues.push(HealthIssue {
            level: HealthRisk::Red,
            message: "navigator index has not been built yet".to_string(),
            remediation: Some("run `codex navigator daemon`".to_string()),
        }),
    }

    let pending = coverage.pending.len();
    if pending >= COVERAGE_PENDING_THRESHOLD {
        issues.push(HealthIssue {
            level: HealthRisk::Yellow,
            message: format!("{pending} paths still pending ingest"),
            remediation: Some("keep navigator daemon running until ingest completes".to_string()),
        });
    }
    if !coverage.errors.is_empty() {
        issues.push(HealthIssue {
            level: HealthRisk::Yellow,
            message: format!("{} files failed to index", coverage.errors.len()),
            remediation: Some("inspect `navigator doctor --json` for failing paths".to_string()),
        });
    }

    if let Some(rate) = literal.fallback_rate
        && literal.total_queries >= MIN_LITERAL_SAMPLE
    {
        if rate >= LITERAL_RATE_RED {
            issues.push(HealthIssue {
                level: HealthRisk::Red,
                message: format!("literal fallback {:.0}% of the time", rate * 100.0),
                remediation: Some("improve query specificity or rebuild index".to_string()),
            });
        } else if rate >= LITERAL_RATE_YELLOW {
            issues.push(HealthIssue {
                level: HealthRisk::Yellow,
                message: format!("literal fallback {:.0}% of the time", rate * 100.0),
                remediation: Some(
                    "consider indexing more modules or adjusting filters".to_string(),
                ),
            });
        }
    }

    let risk = issues
        .iter()
        .map(|issue| issue.level)
        .max_by_key(|level| match level {
            HealthRisk::Red => 2,
            HealthRisk::Yellow => 1,
            HealthRisk::Green => 0,
        })
        .unwrap_or(HealthRisk::Green);

    HealthPanel {
        risk,
        issues,
        ingest,
        literal,
    }
}

fn summarize_literal(totals: &LiteralTotals) -> LiteralStatsSummary {
    let fallback_rate = if totals.total_queries == 0 {
        None
    } else {
        Some(totals.literal_fallbacks as f32 / totals.total_queries as f32)
    };
    let median_scan_micros = if totals.scan_samples.is_empty() {
        None
    } else {
        let mut samples = totals.scan_samples.clone();
        samples.sort_unstable();
        let mid = samples.len() / 2;
        if samples.len() % 2 == 1 {
            Some(samples[mid])
        } else if samples.is_empty() {
            None
        } else {
            Some(((samples[mid - 1] as u128 + samples[mid] as u128) / 2) as u64)
        }
    };
    LiteralStatsSummary {
        total_queries: totals.total_queries,
        literal_fallbacks: totals.literal_fallbacks,
        fallback_rate,
        scanned_files: totals.scanned_files,
        scanned_bytes: totals.scanned_bytes,
        median_scan_micros,
    }
}

fn load_snapshot(path: &Path) -> Result<HealthSnapshot> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("opening navigator health snapshot at {path:?}"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(bincode::deserialize(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::Duration;

    #[test]
    fn health_panel_marks_stale_ingest_as_issue() {
        let mut snapshot = HealthSnapshot::default();
        snapshot.ingest_history.push(IngestRun {
            kind: IngestKind::Full,
            completed_at: OffsetDateTime::now_utc() - Duration::hours(80),
            duration_ms: 5_000,
            files_indexed: 120,
            skipped_total: 0,
            skipped_reasons: Vec::new(),
        });
        let panel = build_panel(snapshot, &CoverageDiagnostics::default());
        assert_eq!(panel.risk, HealthRisk::Red);
        assert!(
            panel
                .issues
                .iter()
                .any(|issue| issue.message.contains("ingest"))
        );
    }

    #[test]
    fn literal_fallback_rate_affects_risk() {
        let mut snapshot = HealthSnapshot::default();
        snapshot.ingest_history.push(IngestRun {
            kind: IngestKind::Full,
            completed_at: OffsetDateTime::now_utc(),
            duration_ms: 3_000,
            files_indexed: 42,
            skipped_total: 0,
            skipped_reasons: Vec::new(),
        });
        snapshot.literal.total_queries = 20;
        snapshot.literal.literal_fallbacks = 15;
        let panel = build_panel(snapshot, &CoverageDiagnostics::default());
        assert_eq!(panel.risk, HealthRisk::Red);
        assert!(
            panel
                .issues
                .iter()
                .any(|issue| issue.message.contains("literal"))
        );
    }
}
