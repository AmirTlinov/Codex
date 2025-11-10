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
use uuid::Uuid;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_code_finder::freeform::CodeFinderPayload;
use codex_code_finder::freeform::CodeFinderSearchArgs;
use codex_code_finder::freeform::parse_payload as parse_code_finder_payload;

pub struct CodeFinderHandler;

const DEFAULT_SEARCH_LIMIT: usize = 40;

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
                parse_request_payload(&arguments)?
            }
            crate::tools::context::ToolPayload::Custom { input } => parse_request_payload(&input)?,
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

fn parse_request_payload(raw: &str) -> Result<CodeFinderPayload, FunctionCallError> {
    parse_code_finder_payload(raw)
        .map_err(|err| FunctionCallError::RespondToModel(err.message().to_string()))
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
