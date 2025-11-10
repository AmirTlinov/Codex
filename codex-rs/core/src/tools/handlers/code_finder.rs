use std::path::PathBuf;

use anyhow::Context;
use async_trait::async_trait;
use codex_code_finder::client::ClientOptions;
use codex_code_finder::client::CodeFinderClient;
use codex_code_finder::client::DaemonSpawn;
use codex_code_finder::proto::FileCategory;
use codex_code_finder::proto::Language;
use codex_code_finder::proto::OpenRequest;
use codex_code_finder::proto::PROTOCOL_VERSION;
use codex_code_finder::proto::QueryId;
use codex_code_finder::proto::SearchFilters;
use codex_code_finder::proto::SearchRequest;
use codex_code_finder::proto::SnippetRequest;
use codex_code_finder::proto::SymbolKind;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use uuid::Uuid;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct CodeFinderHandler;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum CodeFinderPayload {
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

#[derive(Debug, Default, Deserialize)]
struct CodeFinderSearchArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    path_globs: Vec<String>,
    #[serde(default)]
    file_substrings: Vec<String>,
    #[serde(default)]
    symbol_exact: Option<String>,
    #[serde(default)]
    recent_only: Option<bool>,
    #[serde(default)]
    only_tests: Option<bool>,
    #[serde(default)]
    only_docs: Option<bool>,
    #[serde(default)]
    only_deps: Option<bool>,
    #[serde(default)]
    with_refs: Option<bool>,
    #[serde(default)]
    refs_limit: Option<usize>,
    #[serde(default)]
    help_symbol: Option<String>,
    #[serde(default)]
    refine: Option<String>,
    #[serde(default)]
    wait_for_index: Option<bool>,
}

const DEFAULT_SEARCH_LIMIT: usize = 40;
const DEFAULT_SNIPPET_CONTEXT: usize = 8;

fn default_snippet_context() -> usize {
    DEFAULT_SNIPPET_CONTEXT
}

#[async_trait]
impl ToolHandler for CodeFinderHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &crate::tools::context::ToolPayload) -> bool {
        matches!(
            payload,
            crate::tools::context::ToolPayload::Function { .. }
                | crate::tools::context::ToolPayload::Custom { .. }
        )
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { turn, payload, .. } = invocation;
        let request = match payload {
            crate::tools::context::ToolPayload::Function { arguments } => {
                parse_function_payload(&arguments)?
            }
            crate::tools::context::ToolPayload::Custom { input } => parse_freeform_payload(&input)?,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "code_finder received unsupported payload".to_string(),
                ));
            }
        };

        let config = turn.client.config();
        let project_root = turn.cwd.clone();
        let codex_home = config.codex_home.clone();
        let client = build_client(project_root, codex_home).await?;

        match request {
            CodeFinderPayload::Search(args) => {
                let req = build_search_request(*args)?;
                let resp = client
                    .search(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
            CodeFinderPayload::Open { id } => {
                let req = OpenRequest {
                    id,
                    schema_version: PROTOCOL_VERSION,
                };
                let resp = client
                    .open(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
            CodeFinderPayload::Snippet { id, context } => {
                let req = SnippetRequest {
                    id,
                    context,
                    schema_version: PROTOCOL_VERSION,
                };
                let resp = client
                    .snippet(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
        }
    }
}

async fn build_client(
    project_root: PathBuf,
    codex_home: PathBuf,
) -> Result<CodeFinderClient, FunctionCallError> {
    static EXE: OnceCell<PathBuf> = OnceCell::new();
    let exe = EXE
        .get_or_try_init(|| std::env::current_exe().context("resolve current executable"))
        .map_err(|err| {
            FunctionCallError::Fatal(format!(
                "code_finder failed to resolve current executable: {err}"
            ))
        })?;

    let spawn = DaemonSpawn {
        program: exe.clone(),
        args: vec![
            "code-finder-daemon".to_string(),
            "--project-root".to_string(),
            project_root.display().to_string(),
        ],
        env: vec![("CODEX_HOME".to_string(), codex_home.display().to_string())],
    };

    let opts = ClientOptions {
        project_root: Some(project_root),
        codex_home: Some(codex_home),
        spawn: Some(spawn),
    };

    CodeFinderClient::new(opts)
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}

fn build_search_request(args: CodeFinderSearchArgs) -> Result<SearchRequest, FunctionCallError> {
    let mut filters = SearchFilters::default();
    for kind in args.kinds {
        let parsed = parse_symbol_kind(&kind)?;
        if !filters.kinds.contains(&parsed) {
            filters.kinds.push(parsed);
        }
    }
    for lang in args.languages {
        let parsed = parse_language(&lang)?;
        if !filters.languages.contains(&parsed) {
            filters.languages.push(parsed);
        }
    }

    if !args.categories.is_empty() {
        for cat in args.categories {
            let parsed = parse_category(&cat)?;
            if !filters.categories.contains(&parsed) {
                filters.categories.push(parsed);
            }
        }
    } else {
        if args.only_tests.unwrap_or(false) {
            filters.categories.push(FileCategory::Tests);
        }
        if args.only_docs.unwrap_or(false) {
            filters.categories.push(FileCategory::Docs);
        }
        if args.only_deps.unwrap_or(false) {
            filters.categories.push(FileCategory::Deps);
        }
    }

    filters.path_globs = args.path_globs;
    filters.file_substrings = args.file_substrings;
    filters.symbol_exact = args.symbol_exact;
    filters.recent_only = args.recent_only.unwrap_or(false);

    let limit = args.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1);
    let refine = match args.refine {
        Some(value) => Some(parse_query_id(&value)?),
        None => None,
    };

    let request = SearchRequest {
        query: args.query,
        filters,
        limit,
        with_refs: args.with_refs.unwrap_or(false),
        refs_limit: args.refs_limit,
        help_symbol: args.help_symbol,
        refine,
        wait_for_index: args.wait_for_index.unwrap_or(true),
        schema_version: PROTOCOL_VERSION,
    };

    Ok(request)
}

fn parse_function_payload(arguments: &str) -> Result<CodeFinderPayload, FunctionCallError> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "code_finder payload must not be empty".to_string(),
        ));
    }
    if trimmed.starts_with('{') {
        serde_json::from_str::<CodeFinderPayload>(trimmed).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to parse code_finder arguments: {err:?}"
            ))
        })
    } else {
        parse_freeform_payload(trimmed)
    }
}

fn parse_freeform_payload(input: &str) -> Result<CodeFinderPayload, FunctionCallError> {
    let mut action: Option<String> = None;
    let mut search_args = CodeFinderSearchArgs::default();
    let mut symbol_id: Option<String> = None;
    let mut snippet_context: Option<usize> = None;

    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if action.is_none() && !line.contains(':') && !line.contains('=') {
            let mut tokens = line.split_whitespace();
            let verb = tokens
                .next()
                .ok_or_else(|| FunctionCallError::RespondToModel("missing action".to_string()))?
                .to_ascii_lowercase();
            let remainder = tokens.collect::<Vec<_>>().join(" ");
            if !remainder.is_empty() {
                if matches!(verb.as_str(), "open" | "snippet") {
                    symbol_id = Some(remainder.clone());
                } else {
                    search_args.query = Some(remainder.clone());
                }
            }
            action = Some(verb);
            continue;
        }

        let (key, value) = parse_key_value(line)?;
        apply_freeform_pair(
            key,
            value,
            &mut action,
            &mut search_args,
            &mut symbol_id,
            &mut snippet_context,
        )?;
    }

    let verb = action.ok_or_else(|| {
        FunctionCallError::RespondToModel("code_finder freeform input missing action".to_string())
    })?;

    match verb.as_str() {
        "search" => Ok(CodeFinderPayload::Search(Box::new(search_args))),
        "open" => {
            let target = symbol_id.ok_or_else(|| {
                FunctionCallError::RespondToModel("code_finder open requires an id".to_string())
            })?;
            Ok(CodeFinderPayload::Open { id: target })
        }
        "snippet" => {
            let target = symbol_id.ok_or_else(|| {
                FunctionCallError::RespondToModel("code_finder snippet requires an id".to_string())
            })?;
            Ok(CodeFinderPayload::Snippet {
                id: target,
                context: snippet_context.unwrap_or(DEFAULT_SNIPPET_CONTEXT),
            })
        }
        other => Err(FunctionCallError::RespondToModel(format!(
            "unknown code_finder action '{other}'"
        ))),
    }
}

fn parse_key_value(line: &str) -> Result<(String, String), FunctionCallError> {
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
        Err(FunctionCallError::RespondToModel(format!(
            "could not parse line '{line}'"
        )))
    }
}

fn apply_freeform_pair(
    key: String,
    raw_value: String,
    action: &mut Option<String>,
    args: &mut CodeFinderSearchArgs,
    symbol_id: &mut Option<String>,
    snippet_context: &mut Option<usize>,
) -> Result<(), FunctionCallError> {
    match key.as_str() {
        "action" => {
            *action = Some(raw_value.to_ascii_lowercase());
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
            return Err(FunctionCallError::RespondToModel(format!(
                "unsupported code_finder field '{other}'"
            )));
        }
    }
    Ok(())
}

fn parse_bool(field: &str, value: &str) -> Result<bool, FunctionCallError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(FunctionCallError::RespondToModel(format!(
            "invalid boolean for {field}: {other}"
        ))),
    }
}

fn parse_usize(field: &str, value: &str) -> Result<usize, FunctionCallError> {
    value
        .trim()
        .parse()
        .map_err(|err| FunctionCallError::RespondToModel(format!("invalid {field} value: {err}")))
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

fn parse_symbol_kind(raw: &str) -> Result<SymbolKind, FunctionCallError> {
    match raw.to_ascii_lowercase().as_str() {
        "function" => Ok(SymbolKind::Function),
        "method" => Ok(SymbolKind::Method),
        "struct" => Ok(SymbolKind::Struct),
        "enum" => Ok(SymbolKind::Enum),
        "trait" => Ok(SymbolKind::Trait),
        "impl" => Ok(SymbolKind::Impl),
        "module" => Ok(SymbolKind::Module),
        "class" => Ok(SymbolKind::Class),
        "interface" => Ok(SymbolKind::Interface),
        "constant" | "const" => Ok(SymbolKind::Constant),
        "type" | "typealias" => Ok(SymbolKind::TypeAlias),
        "test" => Ok(SymbolKind::Test),
        "document" | "doc" => Ok(SymbolKind::Document),
        other => Err(FunctionCallError::RespondToModel(format!(
            "unsupported symbol kind '{other}'"
        ))),
    }
}

fn parse_language(raw: &str) -> Result<Language, FunctionCallError> {
    match raw.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Ok(Language::Rust),
        "ts" | "typescript" => Ok(Language::Typescript),
        "tsx" => Ok(Language::Tsx),
        "js" | "javascript" => Ok(Language::Javascript),
        "python" | "py" => Ok(Language::Python),
        "go" | "golang" => Ok(Language::Go),
        "bash" | "sh" => Ok(Language::Bash),
        "md" | "markdown" => Ok(Language::Markdown),
        "json" => Ok(Language::Json),
        "yaml" | "yml" => Ok(Language::Yaml),
        "toml" => Ok(Language::Toml),
        "unknown" => Ok(Language::Unknown),
        other => Err(FunctionCallError::RespondToModel(format!(
            "unsupported language '{other}'"
        ))),
    }
}

fn parse_category(raw: &str) -> Result<FileCategory, FunctionCallError> {
    match raw.to_ascii_lowercase().as_str() {
        "source" | "src" => Ok(FileCategory::Source),
        "tests" | "test" => Ok(FileCategory::Tests),
        "docs" | "doc" => Ok(FileCategory::Docs),
        "deps" | "dependencies" => Ok(FileCategory::Deps),
        other => Err(FunctionCallError::RespondToModel(format!(
            "unsupported category '{other}'"
        ))),
    }
}

fn parse_query_id(value: &str) -> Result<QueryId, FunctionCallError> {
    Uuid::parse_str(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid query_id '{value}': {err}"))
    })
}

fn make_json_output<T: serde::Serialize>(resp: T) -> Result<ToolOutput, FunctionCallError> {
    let json = serde_json::to_string_pretty(&resp).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize code_finder response: {err}"))
    })?;
    Ok(ToolOutput::Function {
        content: json,
        content_items: None,
        success: Some(true),
    })
}
