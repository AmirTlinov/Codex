use std::path::PathBuf;

use codex_navigator::freeform::NavigatorPayload;
use codex_navigator::freeform::parse_payload as parse_navigator_payload;
use codex_navigator::navigator_flags;
use codex_navigator::planner::NavigatorSearchArgs;
use codex_navigator::profile_badges;
use codex_navigator::proto::ActiveFilters;
use codex_navigator::proto::AtlasHint;
use codex_navigator::proto::AtlasNode;
use codex_navigator::proto::ErrorPayload;
use codex_navigator::proto::IndexStatus;
use codex_navigator::proto::NavHit;
use codex_navigator::proto::OpenResponse;
use codex_navigator::proto::SearchResponse;
use codex_navigator::proto::SnippetResponse;
use codex_navigator::summarize_args;
use codex_protocol::parse_command::ParsedCommand;
use serde::Deserialize;
use serde_json::Value;

use crate::text_formatting::truncate_text;

const NAVIGATOR_MAX_HITS: usize = 5;
const NAVIGATOR_MAX_PREVIEW_CHARS: usize = 160;
const NAVIGATOR_MAX_RAW_PREVIEW: usize = 200;
const NAVIGATOR_SNIPPET_MAX_LINES: usize = 12;
const NAVIGATOR_MAX_REFS_PER_HIT: usize = 3;

#[derive(Debug, Clone)]
pub(crate) struct NavigatorExecRequest {
    pub command: Vec<String>,
    pub parsed: Vec<ParsedCommand>,
}

#[derive(Debug, Clone)]
pub(crate) struct NavigatorExecOutput {
    pub success: bool,
    pub lines: Vec<String>,
}

pub(crate) fn summarize_navigator_request(raw_input: &str) -> NavigatorExecRequest {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        return fallback_request("<empty>");
    }

    match parse_navigator_payload(trimmed) {
        Ok(NavigatorPayload::Search(args)) => summarize_search_request(&args),
        Ok(NavigatorPayload::Open { id }) => summarize_open_request(&id),
        Ok(NavigatorPayload::Snippet { id, context }) => summarize_snippet_request(&id, context),
        Ok(NavigatorPayload::AtlasSummary { target }) => {
            summarize_atlas_summary_request(target.as_deref())
        }
        Err(_) => fallback_request(trimmed),
    }
}

pub(crate) fn summarize_navigator_response(raw_output: &str) -> NavigatorExecOutput {
    match parse_navigator_output(raw_output) {
        NavigatorOutcome::Search(resp) => NavigatorExecOutput {
            success: resp.error.is_none(),
            lines: render_search_outcome(&resp),
        },
        NavigatorOutcome::Open(resp) => NavigatorExecOutput {
            success: resp.error.is_none(),
            lines: render_open_outcome(&resp),
        },
        NavigatorOutcome::Snippet(resp) => NavigatorExecOutput {
            success: resp.error.is_none(),
            lines: render_snippet_outcome(&resp),
        },
        NavigatorOutcome::AtlasSummary(resp) => NavigatorExecOutput {
            success: resp.focus.is_some(),
            lines: render_atlas_summary_outcome(&resp),
        },
        NavigatorOutcome::Raw(text) => NavigatorExecOutput {
            success: true,
            lines: if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            },
        },
    }
}

fn summarize_search_request(args: &NavigatorSearchArgs) -> NavigatorExecRequest {
    let summary = summarize_args(args);
    let query = args
        .query
        .clone()
        .or_else(|| args.symbol_exact.clone())
        .or_else(|| args.help_symbol.clone());
    let path = args
        .path_globs
        .first()
        .cloned()
        .or_else(|| args.file_substrings.first().cloned());

    NavigatorExecRequest {
        command: vec!["navigator".into(), "search".into()],
        parsed: vec![ParsedCommand::Navigator {
            summary,
            query,
            path,
            profiles: profile_badges(&args.profiles),
            flags: navigator_flags(args),
        }],
    }
}

fn summarize_open_request(id: &str) -> NavigatorExecRequest {
    let display = truncate_text(id, NAVIGATOR_MAX_PREVIEW_CHARS);
    NavigatorExecRequest {
        command: vec!["navigator".into(), "open".into()],
        parsed: vec![ParsedCommand::Read {
            cmd: "navigator open".into(),
            name: display,
            path: PathBuf::from(id),
        }],
    }
}

fn summarize_snippet_request(id: &str, context: usize) -> NavigatorExecRequest {
    let mut display = truncate_text(id, NAVIGATOR_MAX_PREVIEW_CHARS);
    display.push_str(&format!(" (context {context})"));
    NavigatorExecRequest {
        command: vec!["navigator".into(), "snippet".into()],
        parsed: vec![ParsedCommand::Read {
            cmd: "navigator snippet".into(),
            name: display,
            path: PathBuf::from(id),
        }],
    }
}

fn summarize_atlas_summary_request(target: Option<&str>) -> NavigatorExecRequest {
    let focus = target.unwrap_or("workspace");
    let summary = format!("atlas summary {focus}");
    NavigatorExecRequest {
        command: vec!["navigator".into(), "atlas".into(), "--summary".into()],
        parsed: vec![ParsedCommand::Navigator {
            summary: Some(summary.clone()),
            query: Some(summary),
            path: target.map(std::string::ToString::to_string),
            profiles: Vec::new(),
            flags: vec!["summary".into()],
        }],
    }
}

fn fallback_request(preview_src: &str) -> NavigatorExecRequest {
    let preview = preview_from_raw(preview_src).unwrap_or_else(|| "navigator".to_string());
    NavigatorExecRequest {
        command: vec!["navigator".into()],
        parsed: vec![ParsedCommand::Navigator {
            summary: Some(preview.clone()),
            query: Some(preview),
            path: None,
            profiles: Vec::new(),
            flags: Vec::new(),
        }],
    }
}

fn render_search_outcome(resp: &SearchResponse) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("hits: {}", resp.hits.len()));
    if let Some(stats) = &resp.stats {
        let mut parts = vec![format!("took {} ms", stats.took_ms)];
        parts.push(format!("candidates: {}", stats.candidate_size));
        if stats.cache_hit {
            parts.push("cache hit".to_string());
        }
        lines.push(parts.join(" · "));
        if stats.recent_fallback {
            lines.push("warning: no recent hits — showing full results".into());
        }
        if stats.refine_fallback {
            lines.push("warning: refine expanded to full workspace results".into());
        }
        if stats.smart_refine {
            lines.push("smart refine: combined query + refine".into());
        }
        if !stats.applied_profiles.is_empty() {
            let badges: Vec<_> = stats
                .applied_profiles
                .iter()
                .map(|p| p.badge().to_string())
                .collect();
            lines.push(format!("applied profiles: {}", badges.join(", ")));
        }
        if !stats.autocorrections.is_empty() {
            lines.push("autocorrections:".into());
            for note in &stats.autocorrections {
                lines.push(format!("  - {note}"));
            }
        }
        lines.push(format!("input format: {}", stats.input_format));
        if let Some(trigrams) = &stats.literal_missing_trigrams
            && !trigrams.is_empty()
        {
            lines.push(format!("literal missing trigrams: {}", trigrams.join(" ")));
        }
        if let Some(paths) = &stats.literal_pending_paths
            && !paths.is_empty()
        {
            lines.push(format!("literal pending files: {}", paths.join(", ")));
        }
        if let Some(files) = stats.literal_scanned_files {
            if let Some(bytes) = stats.literal_scanned_bytes {
                lines.push(format!("literal scanned {files} files ({bytes} bytes)"));
            } else {
                lines.push(format!("literal scanned {files} files"));
            }
        } else if let Some(bytes) = stats.literal_scanned_bytes {
            lines.push(format!("literal scanned {bytes} bytes"));
        }
    }
    if !resp.hints.is_empty() {
        lines.push("hints:".into());
        for hint in &resp.hints {
            lines.push(format!("  - {hint}"));
        }
    }
    if let Some(filters) = resp.active_filters.as_ref() {
        lines.extend(render_active_filters(filters));
    }
    if let Some(hint) = resp.atlas_hint.as_ref() {
        lines.extend(render_search_atlas_hint(hint));
    }
    if let Some(query_id) = resp.query_id {
        lines.push(format!("query_id: {query_id}"));
    }
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    for (idx, hit) in resp.hits.iter().take(NAVIGATOR_MAX_HITS).enumerate() {
        if idx > 0 {
            lines.push(String::new());
        }
        lines.extend(render_nav_hit(hit));
    }
    if resp.hits.len() > NAVIGATOR_MAX_HITS {
        lines.push(format!(
            "… +{} more hits",
            resp.hits.len() - NAVIGATOR_MAX_HITS
        ));
    }
    lines.extend(render_index_counters(&resp.index));
    lines
}

fn render_open_outcome(resp: &OpenResponse) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("path: {} ({:?})", resp.path, resp.language));
    lines.push(format!(
        "range: {}-{}",
        resp.range.start + 1,
        resp.range.end + 1
    ));
    let displayed_lines = resp.contents.lines().count() as u32;
    if displayed_lines > 0 {
        let end = resp.display_start + displayed_lines.saturating_sub(1);
        lines.push(format!(
            "displaying lines {}-{}",
            resp.display_start,
            end.max(resp.display_start)
        ));
    }
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    lines.extend(render_snippet_body(&resp.contents, resp.truncated));
    lines.extend(render_index_counters(&resp.index));
    lines
}

fn render_snippet_outcome(resp: &SnippetResponse) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let snippet_line_count = resp.snippet.lines().count();
    lines.push(format!("path: {} ({:?})", resp.path, resp.language));
    lines.push(format!(
        "range: {}-{} (context {})",
        resp.range.start + 1,
        resp.range.end + 1,
        snippet_line_count
    ));
    if snippet_line_count > 0 {
        let end = resp.display_start + snippet_line_count.saturating_sub(1) as u32;
        lines.push(format!(
            "displaying lines {}-{}",
            resp.display_start,
            end.max(resp.display_start)
        ));
    }
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    lines.extend(render_snippet_body(&resp.snippet, resp.truncated));
    lines.extend(render_index_counters(&resp.index));
    lines
}

fn render_snippet_body(body: &str, truncated: bool) -> Vec<String> {
    if body.trim().is_empty() {
        return vec!["(empty snippet)".to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let segments: Vec<&str> = body.lines().collect();
    for (idx, line) in segments
        .iter()
        .take(NAVIGATOR_SNIPPET_MAX_LINES)
        .enumerate()
    {
        lines.push(format!("  | {line}"));
        if idx == NAVIGATOR_SNIPPET_MAX_LINES - 1 && segments.len() > NAVIGATOR_SNIPPET_MAX_LINES {
            lines.push(format!(
                "  … +{} more lines",
                segments.len() - NAVIGATOR_SNIPPET_MAX_LINES
            ));
            break;
        }
    }
    if truncated {
        lines.push("  … output truncated".to_string());
    }
    lines
}

fn render_atlas_summary_outcome(resp: &AtlasSummaryResponse) -> Vec<String> {
    let Some(node) = resp.focus.as_ref() else {
        return vec!["atlas summary: index empty".into()];
    };
    let mut lines = Vec::new();
    if !resp.matched
        && resp
            .target
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    {
        lines.push(format!(
            "target '{}' not found",
            resp.target.as_deref().unwrap_or("unknown")
        ));
    }
    let crumb = if resp.breadcrumb.is_empty() {
        "workspace".to_string()
    } else {
        resp.breadcrumb.join(" / ")
    };
    lines.push(format!("atlas: {crumb} ({:?})", node.kind));
    if let Some(timestamp) = resp.generated_at.as_deref() {
        lines.push(format!("generated_at: {timestamp}"));
    }
    lines.push(format!(
        "files={} symbols={} loc={} recent={}",
        node.file_count, node.symbol_count, node.loc, node.recent_files
    ));
    let mut extras = Vec::new();
    if node.doc_files > 0 {
        extras.push(format!("docs {}", node.doc_files));
    }
    if node.test_files > 0 {
        extras.push(format!("tests {}", node.test_files));
    }
    if node.dep_files > 0 {
        extras.push(format!("deps {}", node.dep_files));
    }
    if !extras.is_empty() {
        lines.push(format!("breakdown: {}", extras.join(", ")));
    }
    if node.children.is_empty() {
        lines.push("children: none".into());
    } else {
        lines.push("children:".into());
        let mut ranked: Vec<&AtlasNode> = node.children.iter().collect();
        ranked.sort_by(|a, b| b.file_count.cmp(&a.file_count));
        for child in ranked.into_iter().take(5) {
            lines.push(format!(
                "  - {} ({:?}) files={} loc={}",
                child.name, child.kind, child.file_count, child.loc
            ));
        }
    }
    lines
}

fn render_search_atlas_hint(hint: &AtlasHint) -> Vec<String> {
    let mut lines = Vec::new();
    let crumb = if hint.breadcrumb.is_empty() {
        hint.focus.name.clone()
    } else {
        hint.breadcrumb.join(" / ")
    };
    lines.push(format!(
        "atlas: {} ({:?}) files={} symbols={} loc={} recent={}",
        crumb,
        hint.focus.kind,
        hint.focus.file_count,
        hint.focus.symbol_count,
        hint.focus.loc,
        hint.focus.recent_files
    ));
    if !hint.top_children.is_empty() {
        let preview: Vec<String> = hint
            .top_children
            .iter()
            .take(3)
            .map(|child| format!("{} ({:?})", child.name, child.kind))
            .collect();
        lines.push(format!("  nearby: {}", preview.join(", ")));
    }
    lines
}

fn render_active_filters(filters: &ActiveFilters) -> Vec<String> {
    let tokens = active_filter_tokens(filters);
    if tokens.is_empty() {
        Vec::new()
    } else {
        vec![format!("active filters: {}", tokens.join(", "))]
    }
}

fn active_filter_tokens(filters: &ActiveFilters) -> Vec<String> {
    let mut tokens = Vec::new();
    if !filters.languages.is_empty() {
        let langs = filters
            .languages
            .iter()
            .map(language_label)
            .collect::<Vec<_>>()
            .join("|");
        tokens.push(format!("lang={langs}"));
    }
    if !filters.categories.is_empty() {
        let cats = filters
            .categories
            .iter()
            .map(category_label)
            .collect::<Vec<_>>()
            .join("|");
        tokens.push(format!("cat={cats}"));
    }
    if !filters.path_globs.is_empty() {
        tokens.push(format!("path={}", filters.path_globs.join("|")));
    }
    if !filters.file_substrings.is_empty() {
        tokens.push(format!("file={}", filters.file_substrings.join("|")));
    }
    if !filters.owners.is_empty() {
        tokens.push(format!("owner={}", filters.owners.join("|")));
    }
    if filters.recent_only {
        tokens.push("recent".to_string());
    }
    tokens
}

fn language_label(language: &codex_navigator::proto::Language) -> &'static str {
    match language {
        codex_navigator::proto::Language::Rust => "rust",
        codex_navigator::proto::Language::Typescript => "ts",
        codex_navigator::proto::Language::Tsx => "tsx",
        codex_navigator::proto::Language::Javascript => "js",
        codex_navigator::proto::Language::Python => "python",
        codex_navigator::proto::Language::Go => "go",
        codex_navigator::proto::Language::Bash => "bash",
        codex_navigator::proto::Language::Markdown => "md",
        codex_navigator::proto::Language::Json => "json",
        codex_navigator::proto::Language::Yaml => "yaml",
        codex_navigator::proto::Language::Toml => "toml",
        codex_navigator::proto::Language::Unknown => "unknown",
    }
}

fn category_label(category: &codex_navigator::proto::FileCategory) -> &'static str {
    match category {
        codex_navigator::proto::FileCategory::Source => "source",
        codex_navigator::proto::FileCategory::Tests => "tests",
        codex_navigator::proto::FileCategory::Docs => "docs",
        codex_navigator::proto::FileCategory::Deps => "deps",
    }
}

fn render_nav_hit(hit: &NavHit) -> Vec<String> {
    let mut lines = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    tags.push(format!("{:?}", hit.kind));
    tags.push(format!("{:?}", hit.language));
    if hit.recent {
        tags.push("recent".to_string());
    }
    if !hit.categories.is_empty() {
        tags.push(format!("cats: {}", format_categories(&hit.categories)));
    }
    if let Some(layer) = &hit.layer {
        tags.push(format!("layer: {layer}"));
    }
    if let Some(module) = &hit.module {
        tags.push(format!("module: {module}"));
    }

    let header = if tags.is_empty() {
        format!("{}:{}", hit.path, hit.line)
    } else {
        format!("{}:{} [{}]", hit.path, hit.line, tags.join(" · "))
    };
    lines.push(header);
    lines.push(format!("  {}", sanitize_preview(&hit.preview)));

    if let Some(references) = &hit.references {
        let mut remaining = NAVIGATOR_MAX_REFS_PER_HIT;
        let mut printed = false;
        if !references.definitions.is_empty() {
            lines.push("  definitions:".to_string());
            for reference in references.definitions.iter().take(remaining) {
                lines.push(format!(
                    "    - {}:{} {}",
                    reference.path,
                    reference.line,
                    sanitize_preview(&reference.preview)
                ));
            }
            let shown = references.definitions.len().min(remaining);
            remaining = remaining.saturating_sub(shown);
            printed = true;
        }
        if remaining > 0 && !references.usages.is_empty() {
            lines.push("  usages:".to_string());
            for reference in references.usages.iter().take(remaining) {
                lines.push(format!(
                    "    - {}:{} {}",
                    reference.path,
                    reference.line,
                    sanitize_preview(&reference.preview)
                ));
            }
            let shown = references.usages.len().min(remaining);
            remaining = remaining.saturating_sub(shown);
            printed = true;
        }
        if !printed {
            lines.push("  (no references)".to_string());
        } else if remaining == 0 && references.len() > NAVIGATOR_MAX_REFS_PER_HIT {
            lines.push(format!(
                "  … +{} more refs",
                references.len() - NAVIGATOR_MAX_REFS_PER_HIT
            ));
        }
    }

    if let Some(help) = &hit.help {
        if let Some(summary) = &help.doc_summary {
            lines.push(format!("  doc: {}", sanitize_preview(summary)));
        }
        if let Some(module_path) = &help.module_path {
            lines.push(format!("  module: {module_path}"));
        }
        if let Some(layer) = &help.layer {
            lines.push(format!("  layer: {layer}"));
        }
        if !help.dependencies.is_empty() {
            lines.push(format!("  deps: {}", help.dependencies.join(", ")));
        }
    }

    lines
}

fn render_index_counters(index: &IndexStatus) -> Vec<String> {
    vec![format!(
        "files: {} · symbols: {} · auto: {}",
        index.files,
        index.symbols,
        if index.auto_indexing { "on" } else { "off" }
    )]
}

fn format_categories(categories: &[codex_navigator::proto::FileCategory]) -> String {
    categories
        .iter()
        .map(|cat| format!("{cat:?}"))
        .collect::<Vec<_>>()
        .join("/")
}

fn format_index_status(status: &IndexStatus) -> String {
    let mut parts = vec![format!("index: {:?}", status.state)];
    if let Some(progress) = status.progress {
        parts.push(format!("progress: {}%", (progress * 100.0).round() as i32));
    }
    if !status.auto_indexing {
        parts.push("auto indexing paused".to_string());
    }
    if let Some(notice) = &status.notice {
        parts.push(notice.clone());
    }
    parts.join(" · ")
}

fn format_error_line(error: &ErrorPayload) -> String {
    format!("error ({:?}): {}", error.code, error.message)
}

fn preview_from_raw(raw: &str) -> Option<String> {
    if let Some(extracted) = extract_navigator_query_hint(raw) {
        return Some(extracted);
    }
    let first = raw.lines().find(|line| !line.trim().is_empty())?;
    Some(truncate_text(first.trim(), NAVIGATOR_MAX_RAW_PREVIEW))
}

fn sanitize_preview(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    truncate_text(&collapsed, NAVIGATOR_MAX_PREVIEW_CHARS)
}

fn parse_navigator_output(raw: &str) -> NavigatorOutcome {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return NavigatorOutcome::Raw(String::new());
    }
    if let Ok(resp) = serde_json::from_str::<SearchResponse>(trimmed) {
        return NavigatorOutcome::Search(resp);
    }
    if let Ok(resp) = serde_json::from_str::<OpenResponse>(trimmed) {
        return NavigatorOutcome::Open(resp);
    }
    if let Ok(resp) = serde_json::from_str::<SnippetResponse>(trimmed) {
        return NavigatorOutcome::Snippet(resp);
    }
    if let Ok(resp) = serde_json::from_str::<AtlasSummaryResponse>(trimmed) {
        return NavigatorOutcome::AtlasSummary(resp);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Ok(pretty) = serde_json::to_string_pretty(&value)
    {
        return NavigatorOutcome::Raw(pretty);
    }
    NavigatorOutcome::Raw(truncate_text(trimmed, NAVIGATOR_MAX_RAW_PREVIEW))
}
fn extract_navigator_query_hint(raw: &str) -> Option<String> {
    let candidate = if raw.contains("*** Begin ") {
        raw.to_string()
    } else if raw.contains("\\n*** Begin ") {
        raw.replace("\\n", "\n")
    } else {
        String::new()
    };

    if candidate.is_empty() {
        return None;
    }

    let search_idx = candidate.find("*** Begin Search")?;
    let after = &candidate[search_idx..];
    let end_idx = after.find("*** End Search").unwrap_or(after.len());
    let block = &after[..end_idx];
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("query:") {
            return Some(truncate_text(value.trim(), NAVIGATOR_MAX_PREVIEW_CHARS));
        }
        if let Some(value) = trimmed.strip_prefix("symbol_exact:") {
            return Some(truncate_text(value.trim(), NAVIGATOR_MAX_PREVIEW_CHARS));
        }
        if let Some(value) = trimmed.strip_prefix("help_symbol:") {
            return Some(truncate_text(value.trim(), NAVIGATOR_MAX_PREVIEW_CHARS));
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct AtlasSummaryResponse {
    target: Option<String>,
    matched: bool,
    breadcrumb: Vec<String>,
    focus: Option<AtlasNode>,
    generated_at: Option<String>,
}

enum NavigatorOutcome {
    Search(SearchResponse),
    Open(OpenResponse),
    Snippet(SnippetResponse),
    AtlasSummary(AtlasSummaryResponse),
    Raw(String),
}

#[cfg(any(test, feature = "vt100-tests"))]
pub mod test_support {
    use super::*;
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::new_active_exec_command;
    use crate::history_cell::HistoryCell;
    use ratatui::text::Line;
    use std::time::Duration;

    pub fn navigator_history_lines_for_test(
        request_block: &str,
        response_json: Option<&str>,
        width: u16,
    ) -> Vec<Line<'static>> {
        let summary = summarize_navigator_request(request_block);
        let mut cell = new_active_exec_command(
            "integration-call".into(),
            summary.command.clone(),
            summary.parsed,
            false,
        );

        if let Some(payload) = response_json {
            let result = summarize_navigator_response(payload);
            let text = if result.lines.is_empty() {
                String::new()
            } else {
                result.lines.join("\n")
            };
            let output = CommandOutput {
                exit_code: if result.success { 0 } else { 1 },
                aggregated_output: text.clone(),
                formatted_output: text,
            };
            cell.complete_call("integration-call", output, Duration::from_millis(1));
        }

        cell.display_lines(width)
    }
}
