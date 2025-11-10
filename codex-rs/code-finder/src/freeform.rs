use serde::Deserialize;
use std::fmt;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CodeFinderPayload {
    Search(Box<CodeFinderSearchArgs>),
    Open {
        id: String,
    },
    Snippet {
        id: String,
        #[serde(default = "default_snippet_context")]
        context: usize,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CodeFinderSearchArgs {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    #[serde(default)]
    pub file_substrings: Vec<String>,
    #[serde(default)]
    pub symbol_exact: Option<String>,
    #[serde(default)]
    pub recent_only: Option<bool>,
    #[serde(default)]
    pub only_tests: Option<bool>,
    #[serde(default)]
    pub only_docs: Option<bool>,
    #[serde(default)]
    pub only_deps: Option<bool>,
    #[serde(default)]
    pub with_refs: Option<bool>,
    #[serde(default)]
    pub refs_limit: Option<usize>,
    #[serde(default)]
    pub help_symbol: Option<String>,
    #[serde(default)]
    pub refine: Option<String>,
    #[serde(default)]
    pub wait_for_index: Option<bool>,
}

pub const DEFAULT_SNIPPET_CONTEXT: usize = 8;

fn default_snippet_context() -> usize {
    DEFAULT_SNIPPET_CONTEXT
}

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
        Self::new(format!("failed to parse code_finder arguments: {err:?}"))
    }
}

pub fn parse_payload(arguments: &str) -> Result<CodeFinderPayload, PayloadParseError> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Err(PayloadParseError::new(
            "code_finder payload must not be empty",
        ));
    }

    if trimmed.starts_with('{') {
        serde_json::from_str::<CodeFinderPayload>(trimmed).map_err(PayloadParseError::from)
    } else {
        parse_freeform_payload(trimmed)
    }
}

pub fn parse_freeform_payload(input: &str) -> Result<CodeFinderPayload, PayloadParseError> {
    let normalized = normalize_freeform_input(input)?;
    let mut lines = normalized.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| PayloadParseError::new("code_finder block is empty"))?;
    let (action, mut symbol_id) = parse_header_line(header_line)?;

    let mut search_args = CodeFinderSearchArgs::default();
    let mut snippet_context: Option<usize> = None;
    let mut footer_found = false;

    for raw_line in lines.by_ref() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_footer_line(trimmed, &action) {
            footer_found = true;
            if lines.any(|line| !line.trim().is_empty()) {
                return Err(PayloadParseError::new(
                    "text after *** End block is not allowed",
                ));
            }
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

    if !footer_found {
        return Err(PayloadParseError::new(format!(
            "missing *** End {action} footer"
        )));
    }

    match action.as_str() {
        "search" => Ok(CodeFinderPayload::Search(Box::new(search_args))),
        "open" => {
            let target = symbol_id
                .ok_or_else(|| PayloadParseError::new("code_finder open requires an id"))?;
            Ok(CodeFinderPayload::Open { id: target })
        }
        "snippet" => {
            let target = symbol_id
                .ok_or_else(|| PayloadParseError::new("code_finder snippet requires an id"))?;
            Ok(CodeFinderPayload::Snippet {
                id: target,
                context: snippet_context.unwrap_or(DEFAULT_SNIPPET_CONTEXT),
            })
        }
        other => Err(PayloadParseError::new(format!(
            "unknown code_finder action '{other}'"
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
            "code_finder block must start with *** Begin <Action>",
        ))
    }
}

fn parse_header_line(line: &str) -> Result<(String, Option<String>), PayloadParseError> {
    const HEADER_PREFIX: &str = "*** Begin ";
    let trimmed = line.trim();
    if !trimmed.starts_with(HEADER_PREFIX) {
        return Err(PayloadParseError::new(
            "code_finder block must start with *** Begin <Action>",
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
    const FOOTER_PREFIX: &str = "*** End ";
    if !line
        .to_ascii_lowercase()
        .starts_with(&FOOTER_PREFIX.to_ascii_lowercase())
    {
        return false;
    }
    let rest = line[FOOTER_PREFIX.len()..].trim();
    rest.eq_ignore_ascii_case(action)
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
    args: &mut CodeFinderSearchArgs,
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
        "path" | "paths" | "glob" | "globs" | "path_globs" => {
            args.path_globs.extend(split_list(&raw_value));
        }
        "file" | "files" | "file_substrings" => {
            args.file_substrings.extend(split_list(&raw_value));
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
        "help" | "help_symbol" => args.help_symbol = Some(raw_value),
        "refine" | "query_id" => args.refine = Some(raw_value),
        "wait" | "wait_for_index" => {
            args.wait_for_index = Some(parse_bool("wait_for_index", &raw_value)?);
        }
        "id" => *symbol_id = Some(raw_value),
        "context" => *snippet_context = Some(parse_usize("context", &raw_value)?),
        other => {
            return Err(PayloadParseError::new(format!(
                "unsupported code_finder field '{other}'"
            )));
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

    #[test]
    fn parse_freeform_with_leading_text() {
        let input = "Please search this repo\n\n*** Begin Search\nquery: foo\n*** End Search";
        match parse_freeform_payload(input).expect("should parse search block") {
            CodeFinderPayload::Search(args) => {
                assert_eq!(args.query, Some("foo".to_string()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn parse_freeform_inside_code_fence() {
        let input = "```text\n*** Begin Search\nquery: bar\n*** End Search\n```";
        match parse_freeform_payload(input).expect("should parse fenced block") {
            CodeFinderPayload::Search(args) => {
                assert_eq!(args.query, Some("bar".to_string()));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
