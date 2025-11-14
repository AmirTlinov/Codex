use std::cmp::Ordering;

use time::OffsetDateTime;

use crate::index::model::FileEntry;
use crate::index::model::IndexSnapshot;
use crate::proto::InsightSection;
use crate::proto::InsightSectionKind;
use crate::proto::InsightsRequest;
use crate::proto::InsightsResponse;
use crate::proto::NavigatorInsight;

const DEFAULT_SECTION_ORDER: &[InsightSectionKind] = &[
    InsightSectionKind::AttentionHotspots,
    InsightSectionKind::LintRisks,
    InsightSectionKind::OwnershipGaps,
];

pub fn build_insights(snapshot: &IndexSnapshot, request: &InsightsRequest) -> InsightsResponse {
    let limit = request.limit.max(1);
    let kinds = if request.kinds.is_empty() {
        DEFAULT_SECTION_ORDER.to_vec()
    } else {
        request.kinds.clone()
    };
    let sections = kinds
        .into_iter()
        .filter_map(|kind| build_section(snapshot, kind, limit))
        .collect();
    InsightsResponse {
        generated_at: OffsetDateTime::now_utc(),
        sections,
        trend_summary: None,
    }
}

fn build_section(
    snapshot: &IndexSnapshot,
    kind: InsightSectionKind,
    limit: usize,
) -> Option<InsightSection> {
    let mut candidates = snapshot
        .files
        .values()
        .filter_map(|file| match kind {
            InsightSectionKind::AttentionHotspots => attention_candidate(file),
            InsightSectionKind::LintRisks => lint_candidate(file),
            InsightSectionKind::OwnershipGaps => ownership_candidate(file),
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates.truncate(limit);
    Some(InsightSection {
        kind,
        title: section_title(kind).to_string(),
        summary: section_summary(kind, candidates.len()),
        items: candidates,
    })
}

fn attention_candidate(file: &FileEntry) -> Option<NavigatorInsight> {
    let mut score = 0.0f32;
    let mut reasons = Vec::new();
    if file.attention_density > 0 {
        score += file.attention_density as f32 * 1.4;
        reasons.push(format!("attention density {}", file.attention_density));
    }
    if file.attention > 0 {
        score += file.attention as f32 * 0.3;
        reasons.push(format!("{} TODO markers", file.attention));
    }
    if file.recent {
        score += 2.0;
        reasons.push("recent changes".to_string());
    } else if file.freshness_days < 14 {
        let bonus = (14u32.saturating_sub(file.freshness_days)) as f32 * 0.2;
        score += bonus;
        reasons.push(format!("touched {}d ago", file.freshness_days));
    }
    if file.churn > 0 {
        score += (file.churn as f32).ln_1p();
        if file.churn > 10 {
            reasons.push(format!("churn {}", file.churn));
        } else {
            reasons.push("recent churn".to_string());
        }
    }
    if score <= 0.0 {
        return None;
    }
    Some(make_insight(file, score, reasons))
}

fn lint_candidate(file: &FileEntry) -> Option<NavigatorInsight> {
    let mut score = 0.0f32;
    let mut reasons = Vec::new();
    if file.lint_density > 0 {
        score += file.lint_density as f32 * 1.2;
        reasons.push(format!("lint density {}", file.lint_density));
    }
    if file.lint_suppressions > 0 {
        score += file.lint_suppressions as f32 * 0.5;
        reasons.push(format!("{} suppressions", file.lint_suppressions));
    }
    if file.attention_density > 0 && file.lint_density > 0 {
        score += 0.8;
        reasons.push("lint + TODO overlap".to_string());
    }
    if score <= 0.0 {
        return None;
    }
    Some(make_insight(file, score, reasons))
}

fn ownership_candidate(file: &FileEntry) -> Option<NavigatorInsight> {
    if !file.owners.is_empty() {
        return None;
    }
    let mut score = 0.0f32;
    let mut reasons = vec!["missing CODEOWNERS".to_string()];
    if file.churn > 0 {
        score += (file.churn as f32).sqrt();
        reasons.push(format!("churn {}", file.churn));
    }
    if file.attention_density > 0 {
        score += file.attention_density as f32 * 0.5;
        reasons.push("attention required".to_string());
    }
    if file.freshness_days > 30 {
        score += (file.freshness_days.min(180) as f32) / 30.0;
        reasons.push(format!("stale ({}d)", file.freshness_days));
    }
    if score <= 0.0 {
        return None;
    }
    Some(make_insight(file, score, reasons))
}

fn section_title(kind: InsightSectionKind) -> &'static str {
    match kind {
        InsightSectionKind::AttentionHotspots => "Attention hotspots",
        InsightSectionKind::LintRisks => "Lint risk clusters",
        InsightSectionKind::OwnershipGaps => "Ownership gaps",
    }
}

fn section_summary(kind: InsightSectionKind, count: usize) -> Option<String> {
    if count == 0 {
        return None;
    }
    let label = match kind {
        InsightSectionKind::AttentionHotspots => "files dense with TODO/FIXME",
        InsightSectionKind::LintRisks => "files dominated by lint suppressions/errors",
        InsightSectionKind::OwnershipGaps => "high-churn files without owners",
    };
    Some(format!("top {count} {label}"))
}

fn make_insight(file: &FileEntry, score: f32, reasons: Vec<String>) -> NavigatorInsight {
    NavigatorInsight {
        path: file.path.clone(),
        score,
        reasons,
        owners: file.owners.clone(),
        categories: file.categories.clone(),
        line_count: file.line_count,
        attention: file.attention,
        attention_density: file.attention_density,
        lint_suppressions: file.lint_suppressions,
        lint_density: file.lint_density,
        churn: file.churn,
        freshness_days: file.freshness_days,
        recent: file.recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: crate::proto::Language::Rust,
            categories: vec![crate::proto::FileCategory::Source],
            recent: false,
            symbol_ids: Vec::new(),
            tokens: Vec::new(),
            trigrams: Vec::new(),
            line_count: 120,
            attention: 0,
            attention_density: 0,
            lint_suppressions: 0,
            lint_density: 0,
            churn: 0,
            freshness_days: 90,
            owners: Vec::new(),
            fingerprint: crate::index::model::FileFingerprint {
                mtime: None,
                size: 0,
                digest: [0; 16],
            },
        }
    }

    fn snapshot(mut files: Vec<FileEntry>) -> IndexSnapshot {
        let mut map = std::collections::HashMap::new();
        for entry in files.drain(..) {
            map.insert(entry.path.clone(), entry);
        }
        IndexSnapshot {
            symbols: std::collections::HashMap::new(),
            files: map,
            token_to_files: std::collections::HashMap::new(),
            trigram_to_files: std::collections::HashMap::new(),
            text: std::collections::HashMap::new(),
            atlas: Default::default(),
        }
    }

    #[test]
    fn attention_section_highlights_hot_files() {
        let mut a = file("src/hot.rs");
        a.attention = 6;
        a.attention_density = 4;
        a.recent = true;
        let mut b = file("src/stale.rs");
        b.attention_density = 3;
        b.churn = 20;
        let snap = snapshot(vec![a, b]);
        let request = InsightsRequest {
            schema_version: crate::proto::PROTOCOL_VERSION,
            project_root: None,
            limit: 5,
            kinds: vec![InsightSectionKind::AttentionHotspots],
        };
        let response = build_insights(&snap, &request);
        assert_eq!(response.sections.len(), 1);
        let section = &response.sections[0];
        assert_eq!(section.kind, InsightSectionKind::AttentionHotspots);
        assert!(!section.items.is_empty());
        assert!(
            section.items[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("attention"))
        );
    }

    #[test]
    fn filters_sections_by_kind() {
        let mut linty = file("src/lint.rs");
        linty.lint_density = 9;
        let snap = snapshot(vec![linty]);
        let request = InsightsRequest {
            schema_version: crate::proto::PROTOCOL_VERSION,
            project_root: None,
            limit: 3,
            kinds: vec![InsightSectionKind::LintRisks],
        };
        let response = build_insights(&snap, &request);
        assert_eq!(response.sections.len(), 1);
        assert_eq!(response.sections[0].kind, InsightSectionKind::LintRisks);
        assert_eq!(response.sections[0].items.len(), 1);
    }
}
