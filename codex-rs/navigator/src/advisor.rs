use anyhow::Result;

use crate::client::NavigatorClient;
use crate::planner::NavigatorSearchArgs;
use crate::proto::InsightSectionKind;
use crate::proto::InsightsRequest;
use crate::proto::NavigatorInsight;
use crate::proto::PROTOCOL_VERSION;
use crate::proto::default_insights_limit;

#[derive(Clone, Debug)]
pub struct HotspotMarker {
    pub section: InsightSectionKind,
    pub insight: NavigatorInsight,
}

pub async fn maybe_seed_hotspot_hint(
    client: &NavigatorClient,
    args: &mut NavigatorSearchArgs,
) -> Result<Option<HotspotMarker>> {
    if !should_seed_hotspot_hint(args) {
        return Ok(None);
    }
    let request = InsightsRequest {
        schema_version: PROTOCOL_VERSION,
        project_root: None,
        limit: default_insights_limit(),
        kinds: Vec::new(),
    };
    let response = client.insights(request).await?;
    let marker = response.sections.iter().find_map(|section| {
        section.items.first().map(|item| HotspotMarker {
            section: section.kind,
            insight: item.clone(),
        })
    });
    if let Some(marker) = marker.clone() {
        args.hints
            .push(format!("hotspot: {}", format_hotspot_hint(&marker)));
    }
    Ok(marker)
}

pub fn format_hotspot_hint(marker: &HotspotMarker) -> String {
    let mut parts = Vec::new();
    if marker.insight.attention_density > 0 {
        parts.push(format!("attention {}", marker.insight.attention_density));
    }
    if marker.insight.lint_density > 0 {
        parts.push(format!("lint {}", marker.insight.lint_density));
    }
    if marker.insight.churn > 0 {
        parts.push(format!("churn {}", marker.insight.churn));
    }
    if marker.insight.owners.is_empty() {
        parts.push("unowned".to_string());
    }
    let summary = if parts.is_empty() {
        "noisy".to_string()
    } else {
        parts.join(" · ")
    };
    format!("{} ({summary})", marker.insight.path)
}

fn should_seed_hotspot_hint(args: &NavigatorSearchArgs) -> bool {
    fn empty(value: &Option<String>) -> bool {
        value.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true)
    }
    empty(&args.query)
        && empty(&args.symbol_exact)
        && empty(&args.help_symbol)
        && args.refine.is_none()
        && args.owners.is_empty()
        && args.path_globs.is_empty()
        && args.file_substrings.is_empty()
        && args.kinds.is_empty()
        && args.languages.is_empty()
        && args.categories.is_empty()
        && !args.inherit_filters
        && !args.clear_filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_seed_only_for_empty_queries() {
        let args = NavigatorSearchArgs::default();
        assert!(should_seed_hotspot_hint(&args));
        let mut with_query = NavigatorSearchArgs::default();
        with_query.query = Some("main".to_string());
        assert!(!should_seed_hotspot_hint(&with_query));
    }
}
