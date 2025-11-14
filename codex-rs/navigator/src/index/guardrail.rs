use crate::proto::CoverageDiagnostics;
use crate::proto::HealthRisk;
use crate::proto::HealthSummary;
use crate::proto::SearchStats;
use blake3::Hasher;
use reqwest::Client;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::warn;

const SLOW_QUERY_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Default)]
struct GuardrailState {
    last_health_fingerprint: Option<u64>,
    last_health_sent: Option<Instant>,
    last_latency_sent: Option<Instant>,
}

pub struct GuardrailEmitter {
    project_root: String,
    webhook: Option<String>,
    client: Client,
    latency_threshold_ms: u64,
    health_cooldown: Duration,
    state: Mutex<GuardrailState>,
}

impl GuardrailEmitter {
    pub fn new(
        project_root: &Path,
        webhook: Option<String>,
        latency_threshold_ms: u64,
        health_cooldown: Duration,
    ) -> Self {
        Self {
            project_root: project_root.to_string_lossy().into_owned(),
            webhook,
            client: Client::new(),
            latency_threshold_ms,
            health_cooldown,
            state: Mutex::new(GuardrailState::default()),
        }
    }

    pub async fn observe_health(&self, summary: &HealthSummary, coverage: &CoverageDiagnostics) {
        if matches!(summary.risk, HealthRisk::Green) && summary.hotspot_summary.is_none() {
            return;
        }
        let fingerprint = fingerprint_health(summary, coverage);
        let mut guard = self.state.lock().await;
        let now = Instant::now();
        if guard
            .last_health_fingerprint
            .is_some_and(|value| value == fingerprint)
            && guard
                .last_health_sent
                .is_some_and(|sent| now.duration_since(sent) < self.health_cooldown)
        {
            return;
        }
        guard.last_health_fingerprint = Some(fingerprint);
        guard.last_health_sent = Some(now);
        drop(guard);
        let mut issues = summary
            .issues
            .iter()
            .map(|issue| format!("[{:?}] {}", issue.level, issue.message))
            .collect::<Vec<_>>();
        if let Some(trends) = summary.hotspot_summary.as_ref() {
            let additions: usize = trends.trends.iter().map(|t| t.new_paths.len()).sum();
            if additions > 0 {
                issues.push(format!("hotspot spikes +{additions}"));
            }
        }
        if issues.is_empty() {
            issues.push("health risk escalated".to_string());
        }
        let message = format!("health risk {:?}: {}", summary.risk, issues.join("; "));
        self.emit(
            "health",
            &message,
            json!({
                "event": "health",
                "risk": summary.risk,
                "issues": summary.issues,
                "coverage": coverage,
                "hotspot_summary": summary.hotspot_summary,
            }),
        )
        .await;
    }

    pub async fn observe_search_stats(&self, stats: &SearchStats) {
        if stats.took_ms < self.latency_threshold_ms {
            return;
        }
        let mut guard = self.state.lock().await;
        let now = Instant::now();
        if guard
            .last_latency_sent
            .is_some_and(|sent| now.duration_since(sent) < SLOW_QUERY_COOLDOWN)
        {
            return;
        }
        guard.last_latency_sent = Some(now);
        drop(guard);
        let message = format!(
            "slow query: {}ms (threshold {}ms)",
            stats.took_ms, self.latency_threshold_ms
        );
        self.emit(
            "slow_query",
            &message,
            json!({
                "event": "slow_query",
                "took_ms": stats.took_ms,
                "candidate_size": stats.candidate_size,
                "profiles": stats.applied_profiles,
                "cache_hit": stats.cache_hit,
            }),
        )
        .await;
    }

    async fn emit(&self, kind: &str, message: &str, payload: Value) {
        warn!(
            target: "navigator::guardrail",
            project = %self.project_root,
            kind,
            "{message}"
        );
        if let Some(webhook) = &self.webhook {
            let body = json!({
                "project_root": self.project_root,
                "kind": kind,
                "message": message,
                "payload": payload,
            });
            let client = self.client.clone();
            let url = webhook.clone();
            tokio::spawn(async move {
                if let Err(err) = client.post(&url).json(&body).send().await {
                    warn!(
                        target: "navigator::guardrail",
                        "failed to deliver webhook: {err:?}"
                    );
                }
            });
        }
    }
}

fn fingerprint_health(summary: &HealthSummary, coverage: &CoverageDiagnostics) -> u64 {
    match serde_json::to_vec(&(summary, coverage)) {
        Ok(bytes) => {
            let mut hasher = Hasher::new();
            hasher.update(&bytes);
            let digest = hasher.finalize();
            let mut out = [0u8; 8];
            out.copy_from_slice(&digest.as_bytes()[0..8]);
            u64::from_le_bytes(out)
        }
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::CoverageGap;
    use crate::proto::CoverageReason;
    use crate::proto::HealthIssue;

    #[test]
    fn fingerprint_changes_when_issues_change() {
        let summary = HealthSummary {
            risk: HealthRisk::Yellow,
            issues: vec![HealthIssue {
                level: HealthRisk::Yellow,
                message: "pending".to_string(),
                remediation: None,
            }],
            hotspot_summary: None,
        };
        let mut coverage = CoverageDiagnostics::default();
        coverage.pending.push(CoverageGap {
            path: "src/lib.rs".to_string(),
            reason: CoverageReason::PendingIngest,
        });
        let base = fingerprint_health(&summary, &coverage);
        let mut summary2 = summary;
        summary2.issues.push(HealthIssue {
            level: HealthRisk::Red,
            message: "stale".to_string(),
            remediation: None,
        });
        assert_ne!(base, fingerprint_health(&summary2, &coverage));
    }
}
