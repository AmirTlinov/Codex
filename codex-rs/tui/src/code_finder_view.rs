use std::path::PathBuf;

use codex_code_finder::freeform::CodeFinderPayload;
use codex_code_finder::freeform::CodeFinderSearchArgs;
use codex_code_finder::freeform::parse_payload as parse_code_finder_payload;
use codex_code_finder::proto::ErrorPayload;
use codex_code_finder::proto::IndexStatus;
use codex_code_finder::proto::NavHit;
use codex_code_finder::proto::OpenResponse;
use codex_code_finder::proto::SearchResponse;
use codex_code_finder::proto::SnippetResponse;
use codex_protocol::parse_command::ParsedCommand;
use serde_json::Value;

use crate::text_formatting::truncate_text;

const CODE_FINDER_MAX_HITS: usize = 5;
const CODE_FINDER_MAX_PREVIEW_CHARS: usize = 160;
const CODE_FINDER_MAX_RAW_PREVIEW: usize = 200;
const CODE_FINDER_SNIPPET_MAX_LINES: usize = 12;
const CODE_FINDER_MAX_REFS_PER_HIT: usize = 3;

#[derive(Debug, Clone)]
pub(crate) struct CodeFinderExecRequest {
    pub command: Vec<String>,
    pub parsed: Vec<ParsedCommand>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodeFinderExecOutput {
    pub success: bool,
    pub lines: Vec<String>,
}

pub(crate) fn summarize_code_finder_request(raw_input: &str) -> CodeFinderExecRequest {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        return fallback_request("<empty>");
    }

    match parse_code_finder_payload(trimmed) {
        Ok(CodeFinderPayload::Search(args)) => summarize_search_request(&args),
        Ok(CodeFinderPayload::Open { id }) => summarize_open_request(&id),
        Ok(CodeFinderPayload::Snippet { id, context }) => summarize_snippet_request(&id, context),
        Err(_) => fallback_request(trimmed),
    }
}

pub(crate) fn summarize_code_finder_response(raw_output: &str) -> CodeFinderExecOutput {
    match parse_code_finder_output(raw_output) {
        CodeFinderOutcome::Search(resp) => CodeFinderExecOutput {
            success: resp.error.is_none(),
            lines: render_search_outcome(&resp),
        },
        CodeFinderOutcome::Open(resp) => CodeFinderExecOutput {
            success: resp.error.is_none(),
            lines: render_open_outcome(&resp),
        },
        CodeFinderOutcome::Snippet(resp) => CodeFinderExecOutput {
            success: resp.error.is_none(),
            lines: render_snippet_outcome(&resp),
        },
        CodeFinderOutcome::Raw(text) => CodeFinderExecOutput {
            success: true,
            lines: if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            },
        },
    }
}

fn summarize_search_request(args: &CodeFinderSearchArgs) -> CodeFinderExecRequest {
    let summary = build_search_summary(args);
    let query = summary
        .or_else(|| args.query.clone())
        .or_else(|| args.symbol_exact.clone())
        .or_else(|| args.help_symbol.clone());
    let path = args
        .path_globs
        .first()
        .cloned()
        .or_else(|| args.file_substrings.first().cloned());

    CodeFinderExecRequest {
        command: vec!["code_finder".into(), "search".into()],
        parsed: vec![ParsedCommand::Search {
            cmd: "code_finder search".into(),
            query,
            path,
        }],
    }
}

fn summarize_open_request(id: &str) -> CodeFinderExecRequest {
    let display = truncate_text(id, CODE_FINDER_MAX_PREVIEW_CHARS);
    CodeFinderExecRequest {
        command: vec!["code_finder".into(), "open".into()],
        parsed: vec![ParsedCommand::Read {
            cmd: "code_finder open".into(),
            name: display,
            path: PathBuf::from(id),
        }],
    }
}

fn summarize_snippet_request(id: &str, context: usize) -> CodeFinderExecRequest {
    let mut display = truncate_text(id, CODE_FINDER_MAX_PREVIEW_CHARS);
    display.push_str(&format!(" (context {context})"));
    CodeFinderExecRequest {
        command: vec!["code_finder".into(), "snippet".into()],
        parsed: vec![ParsedCommand::Read {
            cmd: "code_finder snippet".into(),
            name: display,
            path: PathBuf::from(id),
        }],
    }
}

fn fallback_request(preview_src: &str) -> CodeFinderExecRequest {
    let preview = preview_from_raw(preview_src).unwrap_or_else(|| "code_finder".to_string());
    CodeFinderExecRequest {
        command: vec!["code_finder".into()],
        parsed: vec![ParsedCommand::Search {
            cmd: "code_finder".into(),
            query: Some(preview),
            path: None,
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
    }
    if let Some(query_id) = resp.query_id {
        lines.push(format!("query_id: {query_id}"));
    }
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    for (idx, hit) in resp.hits.iter().take(CODE_FINDER_MAX_HITS).enumerate() {
        if idx > 0 {
            lines.push(String::new());
        }
        lines.extend(render_nav_hit(hit));
    }
    if resp.hits.len() > CODE_FINDER_MAX_HITS {
        lines.push(format!(
            "… +{} more hits",
            resp.hits.len() - CODE_FINDER_MAX_HITS
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
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    lines.extend(render_snippet_body(&resp.contents));
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
    lines.push(format_index_status(&resp.index));
    if let Some(error) = &resp.error {
        lines.push(format_error_line(error));
    }
    lines.extend(render_snippet_body(&resp.snippet));
    lines.extend(render_index_counters(&resp.index));
    lines
}

fn render_snippet_body(body: &str) -> Vec<String> {
    if body.trim().is_empty() {
        return vec!["(empty snippet)".to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let segments: Vec<&str> = body.lines().collect();
    for (idx, line) in segments
        .iter()
        .take(CODE_FINDER_SNIPPET_MAX_LINES)
        .enumerate()
    {
        lines.push(format!("  | {line}"));
        if idx == CODE_FINDER_SNIPPET_MAX_LINES - 1
            && segments.len() > CODE_FINDER_SNIPPET_MAX_LINES
        {
            lines.push(format!(
                "  … +{} more lines",
                segments.len() - CODE_FINDER_SNIPPET_MAX_LINES
            ));
            break;
        }
    }
    lines
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
        for reference in references.iter().take(CODE_FINDER_MAX_REFS_PER_HIT) {
            lines.push(format!(
                "  refs: {}:{} {}",
                reference.path,
                reference.line,
                sanitize_preview(&reference.preview)
            ));
        }
        if references.len() > CODE_FINDER_MAX_REFS_PER_HIT {
            lines.push(format!(
                "  … +{} more refs",
                references.len() - CODE_FINDER_MAX_REFS_PER_HIT
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
        "files: {} · symbols: {}",
        index.files, index.symbols
    )]
}

fn format_categories(categories: &[codex_code_finder::proto::FileCategory]) -> String {
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
    if let Some(notice) = &status.notice {
        parts.push(notice.clone());
    }
    parts.join(" · ")
}

fn format_error_line(error: &ErrorPayload) -> String {
    format!("error ({:?}): {}", error.code, error.message)
}

fn build_search_summary(args: &CodeFinderSearchArgs) -> Option<String> {
    let mut summary = args
        .query
        .clone()
        .or_else(|| args.symbol_exact.clone())
        .or_else(|| args.help_symbol.clone())
        .or_else(|| args.path_globs.first().cloned())
        .or_else(|| args.file_substrings.first().cloned());

    if let (Some(text), Some(path)) = (
        summary.as_ref(),
        args.path_globs
            .first()
            .or_else(|| args.file_substrings.first()),
    ) && !text.contains(path)
    {
        summary = Some(format!("{text} in {path}"));
    }

    if let (Some(text), Some(lang)) = (summary.as_ref(), args.languages.first()) {
        summary = Some(format!("{text} ({lang})"));
    }

    summary
}

fn preview_from_raw(raw: &str) -> Option<String> {
    let first = raw.lines().find(|line| !line.trim().is_empty())?;
    Some(truncate_text(first.trim(), CODE_FINDER_MAX_RAW_PREVIEW))
}

fn sanitize_preview(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    truncate_text(&collapsed, CODE_FINDER_MAX_PREVIEW_CHARS)
}

fn parse_code_finder_output(raw: &str) -> CodeFinderOutcome {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CodeFinderOutcome::Raw(String::new());
    }
    if let Ok(resp) = serde_json::from_str::<SearchResponse>(trimmed) {
        return CodeFinderOutcome::Search(resp);
    }
    if let Ok(resp) = serde_json::from_str::<OpenResponse>(trimmed) {
        return CodeFinderOutcome::Open(resp);
    }
    if let Ok(resp) = serde_json::from_str::<SnippetResponse>(trimmed) {
        return CodeFinderOutcome::Snippet(resp);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Ok(pretty) = serde_json::to_string_pretty(&value)
    {
        return CodeFinderOutcome::Raw(pretty);
    }
    CodeFinderOutcome::Raw(truncate_text(trimmed, CODE_FINDER_MAX_RAW_PREVIEW))
}

enum CodeFinderOutcome {
    Search(SearchResponse),
    Open(OpenResponse),
    Snippet(SnippetResponse),
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

    pub fn code_finder_history_lines_for_test(
        request_block: &str,
        response_json: Option<&str>,
        width: u16,
    ) -> Vec<Line<'static>> {
        let summary = summarize_code_finder_request(request_block);
        let mut cell = new_active_exec_command(
            "integration-call".into(),
            summary.command.clone(),
            summary.parsed,
            false,
        );

        if let Some(payload) = response_json {
            let result = summarize_code_finder_response(payload);
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
