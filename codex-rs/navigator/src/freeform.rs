use crate::planner::NavigatorSearchArgs;
use crate::planner::resolve_profile_token;
use crate::planner::suggest_from_options;
use crate::proto::InputFormat;
use crate::proto::SearchProfile;
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NavigatorPayload {
    Search(Box<NavigatorSearchArgs>),
    Open {
        id: String,
    },
    Snippet {
        id: String,
        #[serde(default = "default_snippet_context")]
        context: usize,
    },
    AtlasSummary {
        #[serde(default)]
        target: Option<String>,
    },
    History {
        #[serde(rename = "mode")]
        mode: HistoryActionKind,
        index: usize,
        #[serde(default)]
        pinned: bool,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryActionKind {
    Stack,
    ClearStack,
    Repeat,
}

pub const DEFAULT_SNIPPET_CONTEXT: usize = 8;

fn default_snippet_context() -> usize {
    DEFAULT_SNIPPET_CONTEXT
}

const FREEFORM_KEY_SUGGESTIONS: &[&str] = &[
    "query",
    "q",
    "limit",
    "kind",
    "kinds",
    "language",
    "languages",
    "lang",
    "owners",
    "owner",
    "category",
    "categories",
    "path",
    "paths",
    "paths_include",
    "paths_include_glob",
    "glob",
    "globs",
    "path_globs",
    "paths_include",
    "paths_include_glob",
    "file",
    "files",
    "file_substrings",
    "symbol",
    "symbol_exact",
    "recent",
    "recent_only",
    "tests",
    "only_tests",
    "docs",
    "only_docs",
    "deps",
    "only_deps",
    "dependencies",
    "with_refs",
    "refs",
    "refs_limit",
    "refs_mode",
    "refs_role",
    "refs_mode",
    "refs_role",
    "references_role",
    "references_limit",
    "help",
    "help_symbol",
    "refine",
    "query_id",
    "wait",
    "wait_for_index",
    "id",
    "context",
    "profile",
    "profiles",
    "mode",
    "modes",
    "preset",
    "presets",
    "focus",
    "focuses",
];

const JSON_SEARCH_KEYS: &[&str] = &[
    "action",
    "query",
    "q",
    "limit",
    "kinds",
    "languages",
    "owners",
    "categories",
    "path_globs",
    "paths_include",
    "paths_include_glob",
    "path",
    "paths",
    "glob",
    "globs",
    "file_substrings",
    "file",
    "files",
    "symbol_exact",
    "recent_only",
    "only_tests",
    "only_docs",
    "only_deps",
    "with_refs",
    "refs_limit",
    "refs_role",
    "help_symbol",
    "refine",
    "query_id",
    "wait_for_index",
    "wait",
    "profiles",
    "input_format",
    "schema_version",
];

#[derive(Debug, Clone)]
pub struct PayloadParseError {
    message: String,
}

impl PayloadParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PayloadParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PayloadParseError {}

impl From<serde_json::Error> for PayloadParseError {
    fn from(err: serde_json::Error) -> Self {
        Self::new(format!("failed to parse navigator arguments: {err:?}"))
    }
}

fn try_quick_command(input: &str) -> Option<Result<NavigatorPayload, PayloadParseError>> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let mut action_end = trimmed.len();
    for (idx, ch) in trimmed.char_indices() {
        if ch.is_whitespace() {
            action_end = idx;
            break;
        }
    }
    let (action_raw, rest_raw) = trimmed.split_at(action_end);
    let action = action_raw.to_ascii_lowercase();
    let rest = rest_raw.trim_start();
    match action.as_str() {
        "search" | "find" => Some(parse_quick_search(rest)),
        "open" => Some(parse_quick_open(rest)),
        "snippet" | "snip" => Some(parse_quick_snippet(rest)),
        "atlas" => Some(parse_quick_atlas(rest)),
        "facet" => Some(parse_quick_facet(rest)),
        "history" => Some(parse_quick_history(rest)),
        _ => None,
    }
}

fn parse_quick_search(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let tokens = split_shellwords(rest)?;
    let mut query_tokens: Vec<String> = Vec::new();
    let mut kv_pairs: Vec<(String, String)> = Vec::new();
    let mut args = NavigatorSearchArgs::default();
    let mut only_scope = OnlyScope::Inactive;
    for token in tokens {
        if let Some((key, value)) = token.split_once('=') {
            only_scope.reset();
            kv_pairs.push((key.to_string(), value.to_string()));
            continue;
        }
        let normalized = token.to_ascii_lowercase();
        if normalized == "only" {
            only_scope = OnlyScope::Pending;
            continue;
        }
        match apply_quick_shorthand(&normalized, &mut args, &mut only_scope) {
            ShorthandResult::Category => continue,
            ShorthandResult::Other => {
                only_scope.reset();
                continue;
            }
            ShorthandResult::NotMatched => {}
        }
        only_scope.reset();
        query_tokens.push(token);
    }

    if !query_tokens.is_empty() {
        args.query = Some(query_tokens.join(" "));
    }

    let mut symbol_id: Option<String> = None;
    let mut snippet_context: Option<usize> = None;
    for (key_raw, value_raw) in kv_pairs {
        let key = key_raw.trim().to_ascii_lowercase();
        let value = clean_value(&value_raw);
        apply_freeform_pair(key, value, &mut args, &mut symbol_id, &mut snippet_context)?;
    }

    args.finalize_freeform_hints();
    Ok(NavigatorPayload::Search(Box::new(args)))
}

fn parse_quick_open(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let id = rest.trim();
    if id.is_empty() {
        return Err(PayloadParseError::new(
            "quick open requires an id after 'open'",
        ));
    }
    Ok(NavigatorPayload::Open { id: id.to_string() })
}

fn parse_quick_snippet(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let mut parts = rest.split_whitespace();
    let Some(id) = parts.next() else {
        return Err(PayloadParseError::new(
            "quick snippet requires an id after 'snippet'",
        ));
    };
    let mut context = DEFAULT_SNIPPET_CONTEXT;
    if let Some(token) = parts.next() {
        context = parse_snippet_context_token(token)?;
    }
    if parts.next().is_some() {
        return Err(PayloadParseError::new(
            "quick snippet accepts only id and optional context",
        ));
    }
    Ok(NavigatorPayload::Snippet {
        id: id.to_string(),
        context,
    })
}

fn parse_quick_atlas(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let tokens = split_shellwords(rest)?;
    let Some(mode_token) = tokens.first() else {
        return Err(PayloadParseError::new(
            "atlas command requires a verb (summary|jump)",
        ));
    };
    let mode = mode_token.to_ascii_lowercase();
    match mode.as_str() {
        "summary" | "sum" => {
            let target = tokens.get(1).map(std::string::ToString::to_string);
            Ok(NavigatorPayload::AtlasSummary { target })
        }
        "jump" => {
            let Some(target) = tokens.get(1) else {
                return Err(PayloadParseError::new(
                    "atlas jump requires a target name or path",
                ));
            };
            let mut args = NavigatorSearchArgs::default();
            args.file_substrings.push(target.to_string());
            push_profile(&mut args, SearchProfile::Files);
            args.hints
                .push(format!("atlas jump constrained to '{target}'"));
            Ok(NavigatorPayload::Search(Box::new(args)))
        }
        other => Err(PayloadParseError::new(format!(
            "unknown atlas verb '{other}'"
        ))),
    }
}

fn parse_quick_facet(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let tokens = split_shellwords(rest)?;
    if tokens.is_empty() {
        return Err(PayloadParseError::new(
            "facet command requires arguments like from=<query_id>",
        ));
    }
    let mut args = NavigatorSearchArgs::default();
    args.inherit_filters = true;
    for token in tokens {
        let trimmed = token.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("from=") {
            args.refine = Some(value.to_string());
            continue;
        }
        if let Some(value) = lower.strip_prefix("lang=") {
            args.languages.push(value.to_string());
            continue;
        }
        if let Some(value) = lower.strip_prefix("remove_lang=") {
            args.remove_languages.push(value.to_string());
            continue;
        }
        if let Some(value) = lower.strip_prefix("owner=") {
            args.owners.push(value.to_string());
            continue;
        }
        if let Some(value) = lower.strip_prefix("remove_owner=") {
            args.remove_owners.push(value.to_string());
            continue;
        }
        match lower.as_str() {
            "docs" => args.only_docs = Some(true),
            "tests" => args.only_tests = Some(true),
            "deps" => args.only_deps = Some(true),
            "recent" => args.recent_only = Some(true),
            "no_docs" | "no-docs" => args.remove_categories.push("docs".to_string()),
            "no_tests" | "no-tests" => args.remove_categories.push("tests".to_string()),
            "no_deps" | "no-deps" => args.remove_categories.push("deps".to_string()),
            "no_recent" | "no-recent" => args.disable_recent_only = true,
            "clear" | "clear=true" => args.clear_filters = true,
            _ => args.record_unknown_freeform_key(trimmed, None),
        }
    }
    if args.refine.is_none() {
        return Err(PayloadParseError::new(
            "facet command requires from=<query_id>",
        ));
    }
    if args.clear_filters {
        args.hints
            .push("cleared previously applied filters".to_string());
    }
    args.finalize_freeform_hints();
    Ok(NavigatorPayload::Search(Box::new(args)))
}

fn parse_quick_history(rest: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let tokens = split_shellwords(rest)?;
    let mut mode = HistoryActionKind::Repeat;
    let mut index: usize = 0;
    let mut pinned = false;
    for token in tokens {
        if token.eq_ignore_ascii_case("--pinned") || token.eq_ignore_ascii_case("-p") {
            pinned = true;
            continue;
        }
        if let Some(value) = token.strip_prefix("--index=") {
            index = value
                .parse::<usize>()
                .map_err(|_| PayloadParseError::new(format!("invalid history index `{value}`")))?;
            continue;
        }
        let lowered = token.to_ascii_lowercase();
        if lowered.chars().all(|ch| ch.is_ascii_digit()) {
            index = lowered.parse::<usize>().unwrap_or(0);
            continue;
        }
        match lowered.as_str() {
            "stack" => mode = HistoryActionKind::Stack,
            "clear" | "clear-stack" | "remove" => mode = HistoryActionKind::ClearStack,
            "repeat" | "redo" => mode = HistoryActionKind::Repeat,
            "" => {}
            _ => {
                return Err(PayloadParseError::new(format!(
                    "unsupported history token `{token}`"
                )));
            }
        }
    }
    Ok(NavigatorPayload::History {
        mode,
        index,
        pinned,
    })
}

fn parse_snippet_context_token(token: &str) -> Result<usize, PayloadParseError> {
    let raw = token
        .trim()
        .strip_prefix("context=")
        .unwrap_or(token.trim());
    raw.parse::<usize>()
        .map_err(|err| PayloadParseError::new(format!("invalid snippet context '{token}': {err}")))
}

fn split_shellwords(input: &str) -> Result<Vec<String>, PayloadParseError> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(active) = in_quote {
            if ch == active {
                in_quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_quote = Some(ch),
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if escape {
        return Err(PayloadParseError::new(
            "quick command ended with escape character",
        ));
    }
    if in_quote.is_some() {
        return Err(PayloadParseError::new(
            "quick command contains unterminated quote",
        ));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum OnlyScope {
    #[default]
    Inactive,
    Pending,
    Latched,
}

impl OnlyScope {
    fn reset(&mut self) {
        *self = OnlyScope::Inactive;
    }

    fn consume_category(&mut self) -> bool {
        match self {
            OnlyScope::Pending => {
                *self = OnlyScope::Latched;
                true
            }
            OnlyScope::Latched => true,
            OnlyScope::Inactive => false,
        }
    }
}

enum ShorthandResult {
    Category,
    Other,
    NotMatched,
}

fn apply_quick_shorthand(
    token: &str,
    args: &mut NavigatorSearchArgs,
    only_scope: &mut OnlyScope,
) -> ShorthandResult {
    match token {
        "tests" => {
            if only_scope.consume_category() {
                args.only_tests = Some(true);
            }
            push_profile(args, SearchProfile::Tests);
            ShorthandResult::Category
        }
        "docs" => {
            if only_scope.consume_category() {
                args.only_docs = Some(true);
            }
            push_profile(args, SearchProfile::Docs);
            ShorthandResult::Category
        }
        "deps" => {
            if only_scope.consume_category() {
                args.only_deps = Some(true);
            }
            push_profile(args, SearchProfile::Deps);
            ShorthandResult::Category
        }
        "recent" => {
            args.recent_only = Some(true);
            push_profile(args, SearchProfile::Recent);
            ShorthandResult::Other
        }
        "references" | "refs" => {
            push_profile(args, SearchProfile::References);
            ShorthandResult::Other
        }
        "symbols" => {
            push_profile(args, SearchProfile::Symbols);
            ShorthandResult::Other
        }
        "files" => {
            push_profile(args, SearchProfile::Files);
            ShorthandResult::Other
        }
        "ai" => {
            push_profile(args, SearchProfile::Ai);
            ShorthandResult::Other
        }
        "text" | "literal" | "content" => {
            push_profile(args, SearchProfile::Text);
            ShorthandResult::Other
        }
        "with_refs" | "withrefs" => {
            args.with_refs = Some(true);
            ShorthandResult::Other
        }
        _ => ShorthandResult::NotMatched,
    }
}

fn push_profile(args: &mut NavigatorSearchArgs, profile: SearchProfile) {
    if !args.profiles.contains(&profile) {
        args.profiles.push(profile);
    }
}

pub fn parse_payload(arguments: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Err(PayloadParseError::new(
            "navigator payload must not be empty",
        ));
    }

    if let Some(result) = try_quick_command(trimmed) {
        return result;
    }

    if trimmed.starts_with('{') {
        let mut payload = parse_json_payload(trimmed)?;
        set_input_format(&mut payload, InputFormat::Json);
        Ok(payload)
    } else {
        let mut payload = parse_freeform_payload(trimmed)?;
        set_input_format(&mut payload, InputFormat::Freeform);
        Ok(payload)
    }
}

fn set_input_format(payload: &mut NavigatorPayload, format: InputFormat) {
    if let NavigatorPayload::Search(args) = payload {
        args.input_format = format;
    }
}

fn parse_json_payload(raw: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let value: Value = serde_json::from_str(raw).map_err(|err| {
        PayloadParseError::new(format!(
            "Could not parse JSON payload ({err}). Try {{\"action\":\"search\", ...}} or the *** Begin Search block described in the tool spec."
        ))
    })?;

    if let Some(block) = extract_embedded_block(&value) {
        return parse_payload(&block);
    }

    if let Ok(mut payload) = serde_json::from_value::<NavigatorPayload>(value.clone()) {
        if let NavigatorPayload::Search(args) = &mut payload {
            if let Some(obj) = value.as_object() {
                hydrate_json_aliases(obj, args);
                record_unknown_json_keys(obj, args, None);
            }
            args.finalize_freeform_hints();
        }
        return Ok(payload);
    }

    if let Some(obj) = value.as_object() {
        if let Some(search_body) = obj.get("search") {
            let raw_map = search_body
                .as_object()
                .cloned()
                .ok_or_else(|| PayloadParseError::new("search payload must be an object"))?;
            let mut search_map = raw_map.clone();
            let profiles_value = search_map.remove("profiles");
            let sanitized = Value::Object(search_map);
            let mut args: NavigatorSearchArgs =
                serde_json::from_value(sanitized).map_err(|err| {
                    PayloadParseError::new(format!(
                        "Invalid search payload: {err}. Provide fields like {{\"query\": \"foo\"}}"
                    ))
                })?;
            apply_json_profiles(profiles_value, &mut args)?;
            hydrate_json_aliases(&raw_map, &mut args);
            record_unknown_json_keys(&raw_map, &mut args, Some("search"));
            args.finalize_freeform_hints();
            return Ok(NavigatorPayload::Search(Box::new(args)));
        }
        if let Some(open_body) = obj.get("open") {
            const OPEN_INLINE_EXAMPLE: &str = r#"{"open": "symbol-id"}"#;
            const OPEN_OBJECT_EXAMPLE: &str = r#"{"open": {"id": "symbol-id"}}"#;
            if let Some(id_value) = open_body.as_str() {
                return Ok(NavigatorPayload::Open {
                    id: id_value.to_string(),
                });
            }
            #[derive(Deserialize)]
            struct OpenJson {
                id: String,
            }
            let parsed: OpenJson = serde_json::from_value(open_body.clone()).map_err(|err| {
                PayloadParseError::new(format!(
                    "Invalid open payload: {err}. Use {OPEN_INLINE_EXAMPLE} or {OPEN_OBJECT_EXAMPLE}",
                ))
            })?;
            return Ok(NavigatorPayload::Open { id: parsed.id });
        }
        if let Some(snippet_body) = obj.get("snippet") {
            if let Some(id_value) = snippet_body.as_str() {
                return Ok(NavigatorPayload::Snippet {
                    id: id_value.to_string(),
                    context: DEFAULT_SNIPPET_CONTEXT,
                });
            }
            #[derive(Deserialize)]
            struct SnippetJson {
                id: String,
                #[serde(default = "default_snippet_context")]
                context: usize,
            }
            let parsed: SnippetJson = serde_json::from_value(snippet_body.clone()).map_err(|err| {
                PayloadParseError::new(format!(
                    "Invalid snippet payload: {err}. Use {{\"snippet\": {{\"id\": \"...\", \"context\": 8}}}}"
                ))
            })?;
            return Ok(NavigatorPayload::Snippet {
                id: parsed.id,
                context: parsed.context,
            });
        }
    }

    Err(PayloadParseError::new(
        "Unrecognized JSON shape. Provide {\"action\":\"search\", ...} or shorthand like {\"search\": {\"query\":\"foo\"}}.",
    ))
}

fn apply_json_profiles(
    raw_value: Option<Value>,
    args: &mut NavigatorSearchArgs,
) -> Result<(), PayloadParseError> {
    let Some(raw) = raw_value else {
        return Ok(());
    };

    let tokens: Vec<String> = match raw {
        Value::String(token) => vec![token],
        Value::Array(items) => items
            .into_iter()
            .map(|value| match value {
                Value::String(s) => Ok(s),
                other => Err(PayloadParseError::new(format!(
                    "profiles array must contain strings (found {other})"
                ))),
            })
            .collect::<Result<_, _>>()?,
        other => {
            return Err(PayloadParseError::new(format!(
                "profiles must be a string or array of strings (found {other})"
            )));
        }
    };

    for token in tokens {
        let (profile, hint) = resolve_profile_token(&token).map_err(PayloadParseError::new)?;
        if !args.profiles.contains(&profile) {
            args.profiles.push(profile);
        }
        if let Some(note) = hint {
            args.record_autocorrection(note);
        }
    }

    Ok(())
}

pub fn parse_freeform_payload(input: &str) -> Result<NavigatorPayload, PayloadParseError> {
    let normalized = normalize_freeform_input(input)?;
    let mut lines = normalized.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| PayloadParseError::new("navigator block is empty"))?;
    let (action, mut symbol_id) = parse_header_line(header_line)?;

    let mut search_args = NavigatorSearchArgs::default();
    let mut snippet_context: Option<usize> = None;

    for raw_line in lines.by_ref() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_footer_line(trimmed, &action) {
            break;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = parse_key_value(trimmed)?;
        apply_freeform_pair(
            key,
            value,
            &mut search_args,
            &mut symbol_id,
            &mut snippet_context,
        )?;
    }

    match action.as_str() {
        "search" => {
            search_args.finalize_freeform_hints();
            Ok(NavigatorPayload::Search(Box::new(search_args)))
        }
        "open" => {
            let target =
                symbol_id.ok_or_else(|| PayloadParseError::new("navigator open requires an id"))?;
            Ok(NavigatorPayload::Open { id: target })
        }
        "snippet" => {
            let target = symbol_id
                .ok_or_else(|| PayloadParseError::new("navigator snippet requires an id"))?;
            Ok(NavigatorPayload::Snippet {
                id: target,
                context: snippet_context.unwrap_or(DEFAULT_SNIPPET_CONTEXT),
            })
        }
        other => Err(PayloadParseError::new(format!(
            "unknown navigator action '{other}'"
        ))),
    }
}

fn normalize_freeform_input(raw: &str) -> Result<String, PayloadParseError> {
    let mut trimmed = raw.trim();
    if trimmed.starts_with("```")
        && let Some(end) = trimmed.rfind("```")
        && end > 3
    {
        trimmed = &trimmed[3..end];
        trimmed = trimmed.trim_start();
        if let Some(pos) = trimmed.find('\n') {
            trimmed = trimmed[pos + 1..].trim_start();
        }
    }

    if let Some(idx) = trimmed.find("*** Begin ") {
        Ok(trimmed[idx..].trim_start().to_string())
    } else {
        Err(PayloadParseError::new(
            "navigator block must start with *** Begin <Action>",
        ))
    }
}

fn parse_header_line(line: &str) -> Result<(String, Option<String>), PayloadParseError> {
    const HEADER_PREFIX: &str = "*** Begin ";
    let trimmed = line.trim();
    if !trimmed.starts_with(HEADER_PREFIX) {
        return Err(PayloadParseError::new(
            "navigator block must start with *** Begin <Action>",
        ));
    }
    let rest = trimmed[HEADER_PREFIX.len()..].trim();
    if rest.is_empty() {
        return Err(PayloadParseError::new("missing action after *** Begin"));
    }
    let (action_token, remainder) = split_first_word(rest);
    if action_token.is_empty() {
        return Err(PayloadParseError::new("missing action after *** Begin"));
    }
    let action = action_token.to_ascii_lowercase();
    let header_id = remainder
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    Ok((action, header_id))
}

fn split_first_word(input: &str) -> (&str, Option<&str>) {
    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            let head = &input[..idx];
            let tail = input[idx..].trim();
            return (head, if tail.is_empty() { None } else { Some(tail) });
        }
    }
    (input, None)
}

fn is_footer_line(line: &str, action: &str) -> bool {
    const FOOTER_PREFIX: &str = "*** end";
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == FOOTER_PREFIX {
        return true;
    }
    if !lower.starts_with(FOOTER_PREFIX) {
        return false;
    }
    let rest = lower[FOOTER_PREFIX.len()..].trim();
    if let Some(stripped) = rest.strip_prefix(':') {
        stripped.trim().eq_ignore_ascii_case(action)
    } else {
        rest.is_empty() || rest.eq_ignore_ascii_case(action)
    }
}

fn parse_key_value(line: &str) -> Result<(String, String), PayloadParseError> {
    if let Some(idx) = line.find(':') {
        Ok((
            line[..idx].trim().to_ascii_lowercase(),
            clean_value(&line[idx + 1..]),
        ))
    } else if let Some(idx) = line.find('=') {
        Ok((
            line[..idx].trim().to_ascii_lowercase(),
            clean_value(&line[idx + 1..]),
        ))
    } else {
        Err(PayloadParseError::new(format!(
            "could not parse line '{line}'"
        )))
    }
}

fn apply_freeform_pair(
    key: String,
    raw_value: String,
    args: &mut NavigatorSearchArgs,
    symbol_id: &mut Option<String>,
    snippet_context: &mut Option<usize>,
) -> Result<(), PayloadParseError> {
    match key.as_str() {
        "action" => {
            return Err(PayloadParseError::new(
                "action is defined by the *** Begin header",
            ));
        }
        "query" | "q" => args.query = Some(raw_value),
        "limit" => args.limit = Some(parse_usize("limit", &raw_value)?),
        "kind" | "kinds" => args.kinds.extend(split_list(&raw_value)),
        "language" | "languages" | "lang" => {
            args.languages.extend(split_list(&raw_value));
        }
        "category" | "categories" => args.categories.extend(split_list(&raw_value)),
        "path" | "paths" | "paths_include" | "paths_include_glob" | "glob" | "globs"
        | "path_globs" => {
            args.path_globs.extend(split_list(&raw_value));
        }
        "file" | "files" | "file_substrings" => {
            args.file_substrings.extend(split_list(&raw_value));
        }
        "owner" | "owners" => {
            args.owners.extend(split_list(&raw_value));
        }
        "symbol" | "symbol_exact" => args.symbol_exact = Some(raw_value),
        "recent" => args.recent_only = Some(parse_bool("recent", &raw_value)?),
        "tests" => args.only_tests = Some(parse_bool("tests", &raw_value)?),
        "docs" | "documentation" => args.only_docs = Some(parse_bool("docs", &raw_value)?),
        "deps" | "dependencies" => {
            args.only_deps = Some(parse_bool("deps", &raw_value)?);
        }
        "with_refs" | "refs" => args.with_refs = Some(parse_bool("with_refs", &raw_value)?),
        "refs_limit" | "references_limit" => {
            args.refs_limit = Some(parse_usize("refs_limit", &raw_value)?);
        }
        "refs_mode" | "refs-mode" | "refs_role" | "references_role" => {
            let normalized = raw_value.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                return Err(PayloadParseError::new(
                    "refs_mode must not be empty; try refs_mode=definitions",
                ));
            }
            args.refs_role = Some(normalized);
        }
        "help" | "help_symbol" => args.help_symbol = Some(raw_value),
        "refine" | "query_id" => args.refine = Some(raw_value),
        "wait" | "wait_for_index" => {
            args.wait_for_index = Some(parse_bool("wait_for_index", &raw_value)?);
        }
        "id" => *symbol_id = Some(raw_value),
        "context" => *snippet_context = Some(parse_usize("context", &raw_value)?),
        "profile" | "profiles" | "mode" | "modes" | "preset" | "presets" | "focus" | "focuses" => {
            let tokens = split_list(&raw_value);
            if tokens.is_empty() {
                return Err(PayloadParseError::new(
                    "profile list must contain at least one entry",
                ));
            }
            for token in tokens {
                let (profile, hint) =
                    resolve_profile_token(&token).map_err(PayloadParseError::new)?;
                if !args.profiles.contains(&profile) {
                    args.profiles.push(profile);
                }
                if let Some(note) = hint {
                    args.record_autocorrection(note);
                }
            }
        }
        _ => {
            record_unknown_key_hint(&key, args);
        }
    }
    Ok(())
}

fn parse_bool(field: &str, value: &str) -> Result<bool, PayloadParseError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(PayloadParseError::new(format!(
            "invalid boolean for {field}: {other}"
        ))),
    }
}

fn parse_usize(field: &str, value: &str) -> Result<usize, PayloadParseError> {
    value
        .trim()
        .parse()
        .map_err(|err| PayloadParseError::new(format!("invalid {field} value: {err}")))
}

fn record_unknown_key_hint(key: &str, args: &mut NavigatorSearchArgs) {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return;
    }
    let leaf = trimmed
        .rsplit_once('.')
        .map(|(_, leaf)| leaf)
        .unwrap_or(trimmed);
    let suggestion = suggest_from_options(leaf, FREEFORM_KEY_SUGGESTIONS)
        .filter(|candidate| !candidate.eq_ignore_ascii_case(leaf))
        .map(std::string::ToString::to_string);
    args.record_unknown_freeform_key(trimmed, suggestion);
}

fn record_unknown_json_keys(
    map: &serde_json::Map<String, Value>,
    args: &mut NavigatorSearchArgs,
    path: Option<&str>,
) {
    for key in map.keys() {
        if is_known_json_key(key) {
            continue;
        }
        if let Some(prefix) = path {
            if prefix.is_empty() {
                record_unknown_key_hint(key, args);
            } else {
                let combined = format!("{prefix}.{key}");
                record_unknown_key_hint(&combined, args);
            }
        } else {
            record_unknown_key_hint(key, args);
        }
    }
}

fn hydrate_json_aliases(map: &serde_json::Map<String, Value>, args: &mut NavigatorSearchArgs) {
    if args.query.is_none()
        && let Some(Value::String(text)) = map.get("q")
    {
        args.query = Some(text.clone());
    }
    if args.refine.is_none()
        && let Some(Value::String(text)) = map.get("query_id")
    {
        args.refine = Some(text.clone());
    }
    if args.wait_for_index.is_none()
        && let Some(Value::Bool(flag)) = map.get("wait")
    {
        args.wait_for_index = Some(*flag);
    }
    if map.contains_key("paths_include") || map.contains_key("paths_include_glob") {
        if let Some(Value::String(text)) = map.get("paths_include") {
            args.path_globs.extend(split_list(text));
        }
        if let Some(Value::String(text)) = map.get("paths_include_glob") {
            args.path_globs.extend(split_list(text));
        }
    }
    if let Some(Value::String(owner)) = map.get("owner") {
        args.owners.push(owner.clone());
    }
    if let Some(Value::Array(values)) = map.get("owners") {
        for value in values {
            if let Some(text) = value.as_str() {
                args.owners.push(text.to_string());
            }
        }
    }
    if args.refs_role.is_none()
        && let Some(Value::String(mode)) = map
            .get("refs_mode")
            .or_else(|| map.get("refs-role"))
            .or_else(|| map.get("refs_role"))
    {
        args.refs_role = Some(mode.to_ascii_lowercase());
    }
}

fn extract_embedded_block(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            trimmed
                .starts_with("*** Begin ")
                .then(|| trimmed.to_string())
        }
        Value::Object(map) => map.values().find_map(extract_embedded_block),
        Value::Array(items) => items.iter().find_map(extract_embedded_block),
        _ => None,
    }
}

fn is_known_json_key(key: &str) -> bool {
    JSON_SEARCH_KEYS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(key))
}

fn split_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split(',')
        .map(clean_value)
        .filter(|s| !s.is_empty())
        .collect()
}

fn clean_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InputFormat;
    use crate::proto::SearchProfile;

    #[test]
    fn parse_freeform_with_leading_text() {
        let input = "Please search this repo\n\n*** Begin Search\nquery: foo\n*** End Search";
        match parse_freeform_payload(input).expect("should parse search block") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query, Some("foo".to_string()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_freeform_inside_code_fence() {
        let input = "```text\n*** Begin Search\nquery: bar\n*** End Search\n```";
        match parse_freeform_payload(input).expect("should parse fenced block") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query, Some("bar".to_string()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_profiles_list() {
        let input = "*** Begin Search\nprofile: tests, symbols\n*** End Search";
        match parse_freeform_payload(input).expect("should parse profiles") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.profiles.len(), 2);
                assert!(args.profiles.contains(&SearchProfile::Tests));
                assert!(args.profiles.contains(&SearchProfile::Symbols));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_profile_mode_alias() {
        let input = "*** Begin Search\nmode: references\n*** End Search";
        match parse_freeform_payload(input).expect("should parse mode alias") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.profiles, vec![SearchProfile::References]);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_json_action_sets_input_format() {
        let input = r#"{"action":"search","query":"foo"}"#;
        match parse_payload(input).expect("json search") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.input_format, InputFormat::Json);
                assert_eq!(args.query, Some("foo".into()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_json_shorthand_search() {
        let input = r#"{"search": {"query": "bar"}}"#;
        match parse_payload(input).expect("json shorthand") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query, Some("bar".into()));
                assert_eq!(args.input_format, InputFormat::Json);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn quick_search_supports_query_and_options() {
        let input = "search \"async executor\" profiles=symbols with_refs=true";
        match parse_payload(input).expect("quick search") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("async executor"));
                assert_eq!(args.profiles, vec![SearchProfile::Symbols]);
                assert_eq!(args.with_refs, Some(true));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn quick_search_supports_shorthand_sequences() {
        let input = "search tests only docs \"http server\"";
        match parse_payload(input).expect("quick shorthand search") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("http server"));
                assert!(args.profiles.contains(&SearchProfile::Tests));
                assert!(args.profiles.contains(&SearchProfile::Docs));
                assert_eq!(args.only_docs, Some(true));
                assert_eq!(args.only_tests, None);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn quick_search_chain_only_categories() {
        let input = "search only docs deps \"multi scope\"";
        match parse_payload(input).expect("quick only chain search") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("multi scope"));
                assert_eq!(args.only_docs, Some(true));
                assert_eq!(args.only_deps, Some(true));
                assert!(args.profiles.contains(&SearchProfile::Docs));
                assert!(args.profiles.contains(&SearchProfile::Deps));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn quick_search_accepts_query_after_options() {
        let input = "search profiles=symbols async limit=5 executor";
        match parse_payload(input).expect("quick mixed order search") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("async executor"));
                assert_eq!(args.limit, Some(5));
                assert_eq!(args.profiles, vec![SearchProfile::Symbols]);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn quick_open_and_snippet_commands_parse() {
        match parse_payload("open nav_123").expect("quick open") {
            NavigatorPayload::Open { id } => assert_eq!(id, "nav_123"),
            other => panic!("unexpected payload: {other:?}"),
        }
        match parse_payload("snippet nav_321 12").expect("quick snippet") {
            NavigatorPayload::Snippet { id, context } => {
                assert_eq!(id, "nav_321");
                assert_eq!(context, 12);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn json_action_unknown_key_adds_hint() {
        let input = r#"{"action":"search","queery":"foo"}"#;
        match parse_payload(input).expect("json action search") {
            NavigatorPayload::Search(args) => {
                let hint = args
                    .hints
                    .iter()
                    .find(|hint| hint.contains("ignored unsupported keys"))
                    .expect("aggregated hint missing");
                assert!(hint.contains("queery -> query"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn json_shorthand_unknown_key_reports_path() {
        let input = r#"{"search": {"queery": "foo"}}"#;
        match parse_payload(input).expect("json shorthand") {
            NavigatorPayload::Search(args) => {
                let hint = args
                    .hints
                    .iter()
                    .find(|hint| hint.contains("ignored unsupported keys"))
                    .expect("aggregated hint missing");
                assert!(hint.contains("search.queery -> query"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn json_alias_q_and_wait_are_accepted() {
        let input = r#"{"action":"search","q":"foo","wait":false}"#;
        match parse_payload(input).expect("json alias q/wait") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("foo"));
                assert_eq!(args.wait_for_index, Some(false));
                assert!(args.hints.is_empty());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn json_alias_query_id_is_accepted() {
        let input = r#"{"action":"search","query_id":"abc"}"#;
        match parse_payload(input).expect("json alias query_id") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.refine.as_deref(), Some("abc"));
                assert!(args.hints.is_empty());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn json_action_string_block_is_unwrapped() {
        let input = r#"{"action":"*** Begin Search\nquery: App\n*** End Search"}"#;
        match parse_payload(input).expect("embedded block in JSON") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query.as_deref(), Some("App"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn profile_suggestion_is_autocorrected() {
        let input = "*** Begin Search\nprofile: refernces\n*** End Search";
        match parse_freeform_payload(input).expect("suggestion applied") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.profiles, vec![SearchProfile::References]);
                assert!(
                    args.hints
                        .iter()
                        .any(|hint| hint.contains("auto-corrected"))
                );
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn unsupported_key_adds_hint_instead_of_error() {
        let input = "*** Begin Search\nqueery: foo\n*** End Search";
        match parse_freeform_payload(input).expect("unknown key ignored") {
            NavigatorPayload::Search(args) => {
                assert!(args.query.is_none());
                let hint = args
                    .hints
                    .iter()
                    .find(|hint| hint.contains("ignored unsupported keys"))
                    .expect("aggregated hint missing");
                assert!(hint.contains("queery -> query"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn many_unknown_keys_are_compacted() {
        let input = "*** Begin Search\nqueery: foo\nlimmit: 10\nlangg: rust\nunk: bar\nunknown_two: baz\nunknown_three: qux\n*** End Search";
        match parse_freeform_payload(input).expect("multiple keys tolerated") {
            NavigatorPayload::Search(args) => {
                let hint = args
                    .hints
                    .iter()
                    .find(|hint| hint.contains("ignored unsupported keys"))
                    .expect("aggregated hint missing");
                assert!(hint.contains("queery -> query"));
                assert!(hint.contains("limmit -> limit"));
                assert!(hint.contains("(+"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_freeform_without_footer_succeeds() {
        let input = "*** Begin Search\nquery: fallback\n";
        match parse_freeform_payload(input).expect("missing footer tolerated") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.query, Some("fallback".into()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_atlas_summary() {
        match parse_payload("atlas summary core").expect("summary parsed") {
            NavigatorPayload::AtlasSummary { target } => {
                assert_eq!(target.as_deref(), Some("core"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_atlas_jump_becomes_search() {
        match parse_payload("atlas jump services/api").expect("jump parsed") {
            NavigatorPayload::Search(args) => {
                assert!(args.file_substrings.contains(&"services/api".to_string()));
                assert!(args.profiles.contains(&SearchProfile::Files));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_facet_requires_from() {
        let err = parse_payload("facet lang=rust").expect_err("missing from");
        assert!(err.message().contains("from=<query_id>"));
    }

    #[test]
    fn parse_quick_facet_builds_args() {
        match parse_payload("facet from=abc123 clear lang=rust docs").expect("facet parsed") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.refine.as_deref(), Some("abc123"));
                assert!(args.languages.contains(&"rust".to_string()));
                assert_eq!(args.only_docs, Some(true));
                assert!(args.hints.iter().any(|hint| hint.contains("cleared")));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_facet_supports_remove_tokens() {
        match parse_payload(
            "facet from=q123 remove_lang=rust no-docs no-recent owner=@core remove_owner=legacy",
        )
        .expect("facet parsed")
        {
            NavigatorPayload::Search(args) => {
                assert!(args.remove_languages.contains(&"rust".to_string()));
                assert!(args.remove_categories.contains(&"docs".to_string()));
                assert!(args.disable_recent_only);
                assert!(args.owners.contains(&"@core".to_string()));
                assert!(args.remove_owners.contains(&"legacy".to_string()));
                assert!(args.inherit_filters);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_history_defaults_to_repeat() {
        match parse_quick_history("").expect("history parsed") {
            NavigatorPayload::History {
                mode,
                index,
                pinned,
            } => {
                assert!(matches!(mode, HistoryActionKind::Repeat));
                assert_eq!(index, 0);
                assert!(!pinned);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_quick_history_accepts_stack_with_index() {
        match parse_quick_history("stack 4 --pinned").expect("history parsed") {
            NavigatorPayload::History {
                mode,
                index,
                pinned,
            } => {
                assert!(matches!(mode, HistoryActionKind::Stack));
                assert_eq!(index, 4);
                assert!(pinned);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_json_profile_autocorrects() {
        let input = r#"{"search": {"profiles": ["refernces"], "query": "foo"}}"#;
        match parse_payload(input).expect("json autocorrect") {
            NavigatorPayload::Search(args) => {
                assert_eq!(args.profiles, vec![SearchProfile::References]);
                assert!(
                    args.hints
                        .iter()
                        .any(|hint| hint.contains("auto-corrected"))
                );
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_refs_mode_in_freeform() {
        let input = "*** Begin Search\nquery: Session\nrefs_mode: definitions\n*** End Search";
        let payload = parse_freeform_payload(input).expect("search payload");
        let NavigatorPayload::Search(args) = payload else {
            panic!("expected search payload");
        };
        assert_eq!(args.refs_role.as_deref(), Some("definitions"));
    }

    #[test]
    fn parse_paths_include_glob_alias() {
        let json = r#"{"action":"search","paths_include_glob":"src/**/*.rs"}"#;
        let payload = parse_payload(json).expect("json payload");
        let NavigatorPayload::Search(args) = payload else {
            panic!("expected search payload");
        };
        assert_eq!(args.path_globs, vec!["src/**/*.rs".to_string()]);
        assert!(args.hints.is_empty());
    }
}
