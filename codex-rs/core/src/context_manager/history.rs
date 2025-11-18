use crate::codex::TurnContext;
use crate::context_manager::normalize;
use crate::truncate;
use crate::truncate::format_output_for_model_body;
use crate::truncate::globally_truncate_function_output_items;
use codex_codebase_context::ContextSearchMetadata;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_utils_tokenizer::Tokenizer;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use regex_lite::Regex;
use std::collections::HashSet;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

const CONTEXT_WINDOW_HARD_LIMIT_FACTOR: f64 = 1.1;
const CONTEXT_WINDOW_HARD_LIMIT_BYTES: usize =
    (truncate::MODEL_FORMAT_MAX_BYTES as f64 * CONTEXT_WINDOW_HARD_LIMIT_FACTOR) as usize;
const CONTEXT_WINDOW_HARD_LIMIT_LINES: usize =
    (truncate::MODEL_FORMAT_MAX_LINES as f64 * CONTEXT_WINDOW_HARD_LIMIT_FACTOR) as usize;

/// Transcript of conversation history
#[derive(Debug, Clone)]
pub(crate) struct ContextManager {
    /// The oldest items are at the beginning of the vector.
    items: Vec<ResponseItem>,
    token_info: Option<TokenUsageInfo>,

    /// Optional codebase search provider for automatic context injection
    codebase_provider: Option<Arc<Mutex<Box<dyn CodebaseSearchProvider>>>>,

    /// Codebase search configuration
    codebase_config: crate::config::types::CodebaseSearchConfig,
}

/// Trait for codebase search providers to enable testing and abstraction
pub(crate) trait CodebaseSearchProvider: Send + Sync + std::fmt::Debug {
    fn provide_context<'a>(
        &'a mut self,
        query: &'a str,
        token_budget: usize,
        metadata: Option<&'a ContextSearchMetadata>,
    ) -> BoxFuture<'a, anyhow::Result<Option<CodebaseContext>>>;
}

/// Context result from codebase search
#[derive(Debug, Clone)]
pub(crate) struct CodebaseContext {
    pub formatted_context: String,
    pub chunks_count: usize,
    pub tokens_used: usize,
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextManager {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            token_info: TokenUsageInfo::new_or_append(&None, &None, None),
            codebase_provider: None,
            codebase_config: crate::config::types::CodebaseSearchConfig::default(),
        }
    }

    pub(crate) fn new_with_config(
        codebase_provider: Option<Arc<Mutex<Box<dyn CodebaseSearchProvider>>>>,
        codebase_config: crate::config::types::CodebaseSearchConfig,
    ) -> Self {
        Self {
            items: Vec::new(),
            token_info: TokenUsageInfo::new_or_append(&None, &None, None),
            codebase_provider,
            codebase_config,
        }
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.token_info.clone()
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.token_info = info;
    }

    pub(crate) fn set_codebase_provider(
        &mut self,
        provider: Arc<Mutex<Box<dyn CodebaseSearchProvider>>>,
    ) {
        self.codebase_provider = Some(provider);
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        match &mut self.token_info {
            Some(info) => info.fill_to_context_window(context_window),
            None => {
                self.token_info = Some(TokenUsageInfo::full_context_window(context_window));
            }
        }
    }

    /// `items` is ordered from oldest to newest.
    pub(crate) fn record_items<I>(&mut self, items: I)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        for item in items {
            let item_ref = item.deref();
            let is_ghost_snapshot = matches!(item_ref, ResponseItem::GhostSnapshot { .. });
            if !is_api_message(item_ref) && !is_ghost_snapshot {
                continue;
            }

            let processed = Self::process_item(&item);
            self.items.push(processed);
        }
    }

    /// Record items with automatic codebase context injection for user messages.
    /// When `capture_recorded_items` is `true`, returns the exact sequence
    /// (including injected context) pushed into history so callers can forward
    /// it downstream (e.g., UI/events).
    pub(crate) async fn record_items_with_context<I>(
        &mut self,
        items: I,
        capture_recorded_items: bool,
        cwd: Option<&PathBuf>,
    ) -> anyhow::Result<Option<Vec<ResponseItem>>>
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        let mut captured = capture_recorded_items.then(Vec::new);
        let provider = self.codebase_provider.clone();
        let try_codebase = self.codebase_config.enabled && provider.is_some();

        for item in items {
            if try_codebase
                && let ResponseItem::Message { role, content, .. } = item.deref()
                && role == "user"
            {
                let query = Self::extract_text_from_content(content);
                if !query.trim().is_empty()
                    && let Some(provider) = &provider
                {
                    let metadata = self.build_search_metadata(cwd);
                    match provider
                        .lock()
                        .await
                        .provide_context(&query, self.codebase_config.token_budget, Some(&metadata))
                        .await
                    {
                        Ok(Some(context)) => {
                            tracing::info!(
                                "Injecting codebase context: {} chunks, {} tokens",
                                context.chunks_count,
                                context.tokens_used
                            );

                            let context_item = build_context_response_item(&context);
                            if let Some(captured_items) = captured.as_mut() {
                                captured_items.push(context_item.clone());
                            }
                            let processed = Self::process_item(&context_item);
                            self.items.push(processed);
                        }
                        Ok(None) => {
                            tracing::debug!("No codebase context found for query: {}", query);
                        }
                        Err(e) => {
                            tracing::warn!("Codebase search failed: {}", e);
                        }
                    }
                }
            }

            if let Some(captured_items) = captured.as_mut() {
                captured_items.push(item.deref().clone());
            }
            self.record_items(std::iter::once(item));
        }

        Ok(captured)
    }

    /// Extract text content from ContentItem vector
    fn extract_text_from_content(content: &[ContentItem]) -> String {
        content
            .iter()
            .filter_map(|item| match item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    Some(text.clone())
                }
                ContentItem::InputImage { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn get_history(&mut self) -> Vec<ResponseItem> {
        self.normalize_history();
        self.contents()
    }

    // Returns the history prepared for sending to the model.
    // With extra response items filtered out and GhostCommits removed.
    pub(crate) fn get_history_for_prompt(&mut self) -> Vec<ResponseItem> {
        let mut history = self.get_history();
        Self::remove_ghost_snapshots(&mut history);
        history
    }

    // Estimate the number of tokens in the history. Return None if no tokenizer
    // is available. This does not consider the reasoning traces.
    // /!\ The value is a lower bound estimate and does not represent the exact
    // context length.
    pub(crate) fn estimate_token_count(&self, turn_context: &TurnContext) -> Option<i64> {
        let model = turn_context.client.get_model();
        let tokenizer = Tokenizer::for_model(model.as_str()).ok()?;
        let model_family = turn_context.client.get_model_family();

        Some(
            self.items
                .iter()
                .map(|item| {
                    serde_json::to_string(&item)
                        .map(|item| tokenizer.count(&item))
                        .unwrap_or_default()
                })
                .sum::<i64>()
                + tokenizer.count(model_family.base_instructions.as_str()),
        )
    }

    pub(crate) fn remove_first_item(&mut self) {
        if !self.items.is_empty() {
            // Remove the oldest item (front of the list). Items are ordered from
            // oldest → newest, so index 0 is the first entry recorded.
            let removed = self.items.remove(0);
            // If the removed item participates in a call/output pair, also remove
            // its corresponding counterpart to keep the invariants intact without
            // running a full normalization pass.
            normalize::remove_corresponding_for(&mut self.items, &removed);
        }
    }

    pub(crate) fn replace(&mut self, items: Vec<ResponseItem>) {
        self.items = items;
    }

    pub(crate) fn update_token_info(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.token_info = TokenUsageInfo::new_or_append(
            &self.token_info,
            &Some(usage.clone()),
            model_context_window,
        );
    }

    /// This function enforces a couple of invariants on the in-memory history:
    /// 1. every call (function/custom) has a corresponding output entry
    /// 2. every output has a corresponding call entry
    fn normalize_history(&mut self) {
        // all function/tool calls must have a corresponding output
        normalize::ensure_call_outputs_present(&mut self.items);

        // all outputs must have a corresponding function/tool call
        normalize::remove_orphan_outputs(&mut self.items);
    }

    /// Returns a clone of the contents in the transcript.
    fn contents(&self) -> Vec<ResponseItem> {
        self.items.clone()
    }

    fn remove_ghost_snapshots(items: &mut Vec<ResponseItem>) {
        items.retain(|item| !matches!(item, ResponseItem::GhostSnapshot { .. }));
    }

    fn process_item(item: &ResponseItem) -> ResponseItem {
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                let truncated = format_output_for_model_body(
                    output.content.as_str(),
                    CONTEXT_WINDOW_HARD_LIMIT_BYTES,
                    CONTEXT_WINDOW_HARD_LIMIT_LINES,
                );
                let truncated_items = output
                    .content_items
                    .as_ref()
                    .map(|items| globally_truncate_function_output_items(items));
                ResponseItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    output: FunctionCallOutputPayload {
                        content: truncated,
                        content_items: truncated_items,
                        success: output.success,
                    },
                }
            }
            ResponseItem::CustomToolCallOutput { call_id, output } => {
                let truncated = format_output_for_model_body(
                    output,
                    CONTEXT_WINDOW_HARD_LIMIT_BYTES,
                    CONTEXT_WINDOW_HARD_LIMIT_LINES,
                );
                ResponseItem::CustomToolCallOutput {
                    call_id: call_id.clone(),
                    output: truncated,
                }
            }
            ResponseItem::Message { .. }
            | ResponseItem::Context { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::GhostSnapshot { .. }
            | ResponseItem::Other => item.clone(),
        }
    }

    fn build_search_metadata(&self, cwd: Option<&PathBuf>) -> ContextSearchMetadata {
        ContextSearchMetadata {
            cwd: cwd.cloned(),
            recent_file_paths: self.collect_recent_file_paths(12),
            recent_terms: self.collect_recent_terms(8),
        }
    }

    fn collect_recent_file_paths(&self, max_paths: usize) -> Vec<String> {
        const LOOKBACK: usize = 60;
        static FILE_RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"(?m)([\w./\\-]+\.\w{1,6})").expect("valid file regex"));
        static PATCH_RE: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r"(?m)\*\*\* (?:Add|Update|Delete) File:\s+([^\r\n]+)")
                .expect("valid patch regex")
        });

        let mut hints = Vec::new();
        let mut seen = HashSet::new();
        for item in self.items.iter().rev().take(LOOKBACK) {
            let fragments = Self::text_fragments_for_item(item);
            for fragment in fragments {
                for caps in PATCH_RE.captures_iter(&fragment) {
                    if Self::push_hint(&caps[1], &mut seen, &mut hints, max_paths) {
                        return hints;
                    }
                }
                for caps in FILE_RE.captures_iter(&fragment) {
                    if Self::push_hint(&caps[1], &mut seen, &mut hints, max_paths) {
                        return hints;
                    }
                }
                if hints.len() >= max_paths {
                    return hints;
                }
            }
            if hints.len() >= max_paths {
                break;
            }
        }
        hints
    }

    fn collect_recent_terms(&self, max_terms: usize) -> Vec<String> {
        const LOOKBACK: usize = 40;
        let mut terms = Vec::new();
        let mut seen = HashSet::new();
        for item in self.items.iter().rev().take(LOOKBACK) {
            match item {
                ResponseItem::FunctionCall { name, .. }
                | ResponseItem::CustomToolCall { name, .. } => {
                    let value = format!("tool:{name}");
                    Self::push_hint(&value, &mut seen, &mut terms, max_terms);
                }
                ResponseItem::LocalShellCall {
                    action: LocalShellAction::Exec(exec),
                    ..
                } => {
                    if let Some(cmd) = exec.command.first() {
                        Self::push_hint(cmd, &mut seen, &mut terms, max_terms);
                    }
                }
                ResponseItem::Context {
                    formatted_context, ..
                } => {
                    for line in formatted_context.lines() {
                        if let Some(path) = Self::extract_path_from_context_line(line) {
                            Self::push_hint(path, &mut seen, &mut terms, max_terms);
                        }
                    }
                }
                _ => {}
            }
            if terms.len() >= max_terms {
                break;
            }
        }
        terms
    }

    fn text_fragments_for_item(item: &ResponseItem) -> Vec<String> {
        match item {
            ResponseItem::Message { content, .. } => Self::extract_text_from_content(content)
                .split('\n')
                .map(|s| s.to_string())
                .collect(),
            ResponseItem::Context {
                formatted_context, ..
            } => vec![formatted_context.clone()],
            ResponseItem::FunctionCall { arguments, .. } => vec![arguments.clone()],
            ResponseItem::FunctionCallOutput { output, .. } => vec![output.content.clone()],
            ResponseItem::CustomToolCall { input, .. } => vec![input.clone()],
            ResponseItem::CustomToolCallOutput { output, .. } => vec![output.clone()],
            ResponseItem::LocalShellCall {
                action: LocalShellAction::Exec(exec),
                ..
            } => vec![exec.command.join(" ")],
            _ => Vec::new(),
        }
    }

    fn extract_path_from_context_line(line: &str) -> Option<&str> {
        if !line.starts_with("## ") {
            return None;
        }
        let mut segments = line.split('`');
        segments.next()?; // leading "## n. "
        segments.next()
    }

    fn push_hint(
        raw: &str,
        seen: &mut HashSet<String>,
        out: &mut Vec<String>,
        limit: usize,
    ) -> bool {
        if out.len() >= limit {
            return true;
        }
        let normalized = raw
            .replace('\\', "/")
            .trim_matches(|c| c == '"' || c == '\'')
            .trim()
            .to_string();
        if normalized.is_empty() {
            return false;
        }
        let key = normalized.to_lowercase();
        if seen.insert(key) {
            out.push(normalized);
        }
        out.len() >= limit
    }
}

pub fn format_context_for_prompt(
    formatted_context: &str,
    chunks_count: usize,
    tokens_used: usize,
) -> String {
    let chunk_suffix = if chunks_count == 1 { "" } else { "s" };
    format!(
        "<context>\n{}\n\n_Found {} chunk{chunk_suffix} ({} tokens) via codebase search._\n</context>",
        normalize_context_body(formatted_context),
        chunks_count,
        tokens_used
    )
}

fn build_context_response_item(context: &CodebaseContext) -> ResponseItem {
    ResponseItem::Context {
        formatted_context: normalize_context_body(&context.formatted_context),
        chunks_count: context.chunks_count,
        tokens_used: context.tokens_used,
    }
}

fn normalize_context_body(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("# Relevant Codebase Context") {
        trimmed.to_string()
    } else {
        format!("# Relevant Codebase Context\n\n{trimmed}")
    }
}

/// API messages include every non-system item (user/assistant messages, reasoning,
/// tool calls, tool outputs, shell calls, and web-search calls).
fn is_api_message(message: &ResponseItem) -> bool {
    match message {
        ResponseItem::Message { role, .. } => role.as_str() != "system",
        ResponseItem::Context { .. } => true,
        ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. } => true,
        ResponseItem::GhostSnapshot { .. } => false,
        ResponseItem::Other => false,
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
