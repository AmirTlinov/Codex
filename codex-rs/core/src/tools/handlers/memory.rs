use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::config::types::MemoryStalenessMode;
use crate::function_tool::FunctionCallError;
use crate::memory::Block;
use crate::memory::BlockKind;
use crate::memory::BlockPriority;
use crate::memory::BlockStatus;
use crate::memory::BlockStore;
use crate::memory::Edge;
use crate::memory::EdgeKind;
use crate::memory::Fingerprint;
use crate::memory::SourceKind;
use crate::memory::SourceRef;
use crate::memory::fill_missing_file_fingerprints;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::spec::JsonSchema;
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

const MAX_BLOCK_BODY_BYTES: usize = 32 * 1024;
const MAX_TAGS: usize = 64;
const MAX_LINKS: usize = 64;
const MAX_SOURCES: usize = 64;
const MAX_LIST_LIMIT: usize = 200;

pub struct MemoryHandler;

pub static MEMORY_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let mut fingerprint_props = BTreeMap::new();
    fingerprint_props.insert(
        "type".to_string(),
        JsonSchema::String {
            description: Some("git_oid | mtime_size".to_string()),
        },
    );
    fingerprint_props.insert(
        "oid".to_string(),
        JsonSchema::String {
            description: Some("Git object id for git_oid fingerprints.".to_string()),
        },
    );
    fingerprint_props.insert(
        "mtime_ns".to_string(),
        JsonSchema::Number {
            description: Some("mtime in nanoseconds for mtime_size fingerprints.".to_string()),
        },
    );
    fingerprint_props.insert(
        "size_bytes".to_string(),
        JsonSchema::Number {
            description: Some("size in bytes for mtime_size fingerprints.".to_string()),
        },
    );
    let fingerprint_schema = JsonSchema::Object {
        properties: fingerprint_props,
        required: None,
        additional_properties: Some(false.into()),
    };

    let mut source_props = BTreeMap::new();
    source_props.insert(
        "kind".to_string(),
        JsonSchema::String {
            description: Some("file_path | command | url | conversation | tool_output".to_string()),
        },
    );
    source_props.insert(
        "locator".to_string(),
        JsonSchema::String {
            description: Some("Path, command, or locator string.".to_string()),
        },
    );
    source_props.insert(
        "fingerprint".to_string(),
        JsonSchema::Object {
            properties: match fingerprint_schema {
                JsonSchema::Object { properties, .. } => properties,
                _ => BTreeMap::new(),
            },
            required: None,
            additional_properties: Some(false.into()),
        },
    );
    let source_schema = JsonSchema::Object {
        properties: source_props,
        required: Some(vec!["kind".to_string(), "locator".to_string()]),
        additional_properties: Some(false.into()),
    };

    let mut link_props = BTreeMap::new();
    link_props.insert(
        "to".to_string(),
        JsonSchema::String {
            description: Some("Target block id.".to_string()),
        },
    );
    link_props.insert(
        "rel".to_string(),
        JsonSchema::String {
            description: Some(
                "mentions | depends_on | implements | explains | supersedes | derived_from"
                    .to_string(),
            ),
        },
    );
    link_props.insert(
        "from".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional source block id; must match block.id if provided.".to_string(),
            ),
        },
    );
    let link_schema = JsonSchema::Object {
        properties: link_props,
        required: Some(vec!["to".to_string(), "rel".to_string()]),
        additional_properties: Some(false.into()),
    };

    let mut block_props = BTreeMap::new();
    block_props.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Block id.".to_string()),
        },
    );
    block_props.insert(
        "kind".to_string(),
        JsonSchema::String {
            description: Some(
                "focus | goals | constraints | decisions | facts | open_questions | file_summary | repo_map | workspace | toolbox | tool_slice | plan"
                    .to_string(),
            ),
        },
    );
    block_props.insert(
        "title".to_string(),
        JsonSchema::String {
            description: Some("Short block title.".to_string()),
        },
    );
    block_props.insert(
        "body_full".to_string(),
        JsonSchema::String {
            description: Some("Full body (optional).".to_string()),
        },
    );
    block_props.insert(
        "body_summary".to_string(),
        JsonSchema::String {
            description: Some("Summary body (optional).".to_string()),
        },
    );
    block_props.insert(
        "body_label".to_string(),
        JsonSchema::String {
            description: Some("Label body (optional).".to_string()),
        },
    );
    block_props.insert(
        "tags".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some("List of tags.".to_string()),
        },
    );
    block_props.insert(
        "links".to_string(),
        JsonSchema::Array {
            items: Box::new(link_schema),
            description: Some("Block graph edges.".to_string()),
        },
    );
    block_props.insert(
        "sources".to_string(),
        JsonSchema::Array {
            items: Box::new(source_schema),
            description: Some("Block sources.".to_string()),
        },
    );
    block_props.insert(
        "status".to_string(),
        JsonSchema::String {
            description: Some("active | stashed | stale".to_string()),
        },
    );
    block_props.insert(
        "priority".to_string(),
        JsonSchema::String {
            description: Some("pinned | high | normal | low".to_string()),
        },
    );
    let block_schema = JsonSchema::Object {
        properties: block_props.clone(),
        required: Some(vec![
            "id".to_string(),
            "kind".to_string(),
            "title".to_string(),
        ]),
        additional_properties: Some(false.into()),
    };
    let patch_schema = JsonSchema::Object {
        properties: block_props,
        required: None,
        additional_properties: Some(false.into()),
    };

    let mut filters_props = BTreeMap::new();
    filters_props.insert(
        "status".to_string(),
        JsonSchema::String {
            description: Some("active | stashed | stale".to_string()),
        },
    );
    filters_props.insert(
        "kind".to_string(),
        JsonSchema::String {
            description: Some("Filter by block kind.".to_string()),
        },
    );
    filters_props.insert(
        "tag".to_string(),
        JsonSchema::String {
            description: Some("Filter by tag.".to_string()),
        },
    );
    filters_props.insert(
        "priority".to_string(),
        JsonSchema::String {
            description: Some("pinned | high | normal | low".to_string()),
        },
    );
    let filters_schema = JsonSchema::Object {
        properties: filters_props,
        required: None,
        additional_properties: Some(false.into()),
    };

    let mut properties = BTreeMap::new();
    properties.insert(
        "action".to_string(),
        JsonSchema::String {
            description: Some("upsert | patch | delete | get | list".to_string()),
        },
    );
    properties.insert("block".to_string(), block_schema);
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Block id for get/delete/patch.".to_string()),
        },
    );
    properties.insert("patch".to_string(), patch_schema);
    properties.insert("filters".to_string(), filters_schema);
    properties.insert(
        "limit".to_string(),
        JsonSchema::Number {
            description: Some("Max list size (default 50, capped).".to_string()),
        },
    );
    properties.insert(
        "view".to_string(),
        JsonSchema::String {
            description: Some("full | summary | label".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "memory".to_string(),
        description:
            "Manage lego memory blocks (upsert/patch/delete/get/list). Enabled with lego_memory."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
});

#[async_trait]
impl ToolHandler for MemoryHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { turn, payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "memory handler received unsupported payload".to_string(),
                ));
            }
        };

        let request = serde_json::from_str::<MemoryToolRequest>(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse memory arguments: {err}"))
        })?;

        let mut store = BlockStore::open(&turn.memory_config.root_dir, &turn.cwd)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to open memory store: {err}"))
            })?;

        let response = apply_memory_request(
            &mut store,
            &turn.cwd,
            turn.memory_config.staleness,
            turn.memory_config.max_bytes,
            request,
        )
        .await?;

        let content = serde_json::to_string_pretty(&response).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to serialize memory response: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum MemoryToolRequest {
    Upsert {
        block: BlockInput,
    },
    Patch {
        id: String,
        patch: BlockPatch,
    },
    Delete {
        id: String,
    },
    Get {
        id: String,
        view: Option<BlockViewKind>,
    },
    List {
        filters: Option<MemoryListFilters>,
        limit: Option<usize>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct BlockInput {
    id: String,
    kind: BlockKind,
    title: String,
    body_full: Option<String>,
    body_summary: Option<String>,
    body_label: Option<String>,
    tags: Option<Vec<String>>,
    links: Option<Vec<EdgeInput>>,
    sources: Option<Vec<SourceRefInput>>,
    status: Option<BlockStatus>,
    priority: Option<BlockPriority>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct BlockPatch {
    title: Option<String>,
    body_full: Option<String>,
    body_summary: Option<String>,
    body_label: Option<String>,
    tags: Option<Vec<String>>,
    links: Option<Vec<EdgeInput>>,
    sources: Option<Vec<SourceRefInput>>,
    status: Option<BlockStatus>,
    priority: Option<BlockPriority>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct EdgeInput {
    to: String,
    rel: EdgeKind,
    from: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SourceRefInput {
    kind: SourceKind,
    locator: String,
    fingerprint: Option<Fingerprint>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MemoryListFilters {
    status: Option<BlockStatus>,
    kind: Option<BlockKind>,
    tag: Option<String>,
    priority: Option<BlockPriority>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum BlockViewKind {
    Full,
    Summary,
    Label,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct MemoryToolResponse {
    action: MemoryAction,
    result: MemoryToolResult,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MemoryAction {
    Upsert,
    Patch,
    Delete,
    Get,
    List,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MemoryToolResult {
    Block { block: BlockView },
    Blocks { blocks: Vec<BlockSummary> },
    Deleted { id: String },
    Upserted { block: BlockSummary },
    Patched { block: BlockSummary },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct BlockSummary {
    id: String,
    kind: BlockKind,
    title: String,
    status: BlockStatus,
    priority: BlockPriority,
    updated_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct BlockView {
    id: String,
    kind: BlockKind,
    title: String,
    status: BlockStatus,
    priority: BlockPriority,
    representation: BlockViewKind,
    body: String,
    tags: Vec<String>,
    links: Vec<Edge>,
    sources: Vec<SourceRef>,
    updated_at: u64,
}

async fn apply_memory_request(
    store: &mut BlockStore,
    cwd: &Path,
    mode: MemoryStalenessMode,
    max_bytes: usize,
    request: MemoryToolRequest,
) -> Result<MemoryToolResponse, FunctionCallError> {
    match request {
        MemoryToolRequest::Upsert { block } => {
            validate_block_input(&block)?;
            let mut warnings = Vec::new();
            let stored = upsert_block(store, cwd, mode, block, &mut warnings).await?;
            warnings.extend(
                store
                    .enforce_budget(max_bytes)
                    .await
                    .map_err(to_tool_error)?,
            );
            Ok(MemoryToolResponse {
                action: MemoryAction::Upsert,
                result: MemoryToolResult::Upserted {
                    block: BlockSummary::from_block(&stored),
                },
                warnings,
            })
        }
        MemoryToolRequest::Patch { id, patch } => {
            validate_patch(&patch)?;
            let mut warnings = Vec::new();
            let stored = patch_block(store, cwd, mode, &id, patch, &mut warnings).await?;
            warnings.extend(
                store
                    .enforce_budget(max_bytes)
                    .await
                    .map_err(to_tool_error)?,
            );
            Ok(MemoryToolResponse {
                action: MemoryAction::Patch,
                result: MemoryToolResult::Patched {
                    block: BlockSummary::from_block(&stored),
                },
                warnings,
            })
        }
        MemoryToolRequest::Delete { id } => {
            let mut warnings = Vec::new();
            store.delete(&id).await.map_err(to_tool_error)?;
            warnings.extend(
                store
                    .enforce_budget(max_bytes)
                    .await
                    .map_err(to_tool_error)?,
            );
            Ok(MemoryToolResponse {
                action: MemoryAction::Delete,
                result: MemoryToolResult::Deleted { id },
                warnings,
            })
        }
        MemoryToolRequest::Get { id, view } => {
            let block = store
                .get(&id)
                .ok_or_else(|| err_msg(format!("unknown block id: {id}")))?;
            let view = view.unwrap_or(BlockViewKind::Full);
            Ok(MemoryToolResponse {
                action: MemoryAction::Get,
                result: MemoryToolResult::Block {
                    block: BlockView::from_block(block, view),
                },
                warnings: Vec::new(),
            })
        }
        MemoryToolRequest::List { filters, limit } => {
            let limit = limit.unwrap_or(50).min(MAX_LIST_LIMIT);
            let blocks = list_blocks(store, filters, limit);
            Ok(MemoryToolResponse {
                action: MemoryAction::List,
                result: MemoryToolResult::Blocks { blocks },
                warnings: Vec::new(),
            })
        }
    }
}

async fn upsert_block(
    store: &mut BlockStore,
    cwd: &Path,
    mode: MemoryStalenessMode,
    input: BlockInput,
    warnings: &mut Vec<String>,
) -> Result<Block, FunctionCallError> {
    let mut block = store
        .get(&input.id)
        .cloned()
        .unwrap_or_else(|| Block::new(&input.id, input.kind, input.title.clone()));
    block.kind = input.kind;
    block.title = input.title;
    if let Some(body_full) = input.body_full {
        enforce_body_limit("body_full", &body_full)?;
        block.body_full = Some(body_full);
    }
    if let Some(body_summary) = input.body_summary {
        enforce_body_limit("body_summary", &body_summary)?;
        block.body_summary = Some(body_summary);
    }
    if let Some(body_label) = input.body_label {
        enforce_body_limit("body_label", &body_label)?;
        block.body_label = Some(body_label);
    }
    if let Some(tags) = input.tags {
        enforce_list_limit("tags", tags.len(), MAX_TAGS)?;
        block.tags = tags;
    }
    if let Some(links) = input.links {
        enforce_list_limit("links", links.len(), MAX_LINKS)?;
        block.links = convert_links(&input.id, links)?;
    }
    if let Some(sources) = input.sources {
        enforce_list_limit("sources", sources.len(), MAX_SOURCES)?;
        block.sources = convert_sources(sources);
    }
    if let Some(status) = input.status {
        block.status = status;
    }
    if let Some(priority) = input.priority {
        block.priority = priority;
    }

    warnings.extend(fill_missing_file_fingerprints(&mut block.sources, cwd, mode).await);
    block.touch();
    store.upsert(block.clone()).await.map_err(to_tool_error)?;
    Ok(block)
}

async fn patch_block(
    store: &mut BlockStore,
    cwd: &Path,
    mode: MemoryStalenessMode,
    id: &str,
    patch: BlockPatch,
    warnings: &mut Vec<String>,
) -> Result<Block, FunctionCallError> {
    let mut block = store
        .get(id)
        .cloned()
        .ok_or_else(|| err_msg(format!("unknown block id: {id}")))?;
    if let Some(title) = patch.title {
        block.title = title;
    }
    if let Some(body_full) = patch.body_full {
        enforce_body_limit("body_full", &body_full)?;
        block.body_full = Some(body_full);
    }
    if let Some(body_summary) = patch.body_summary {
        enforce_body_limit("body_summary", &body_summary)?;
        block.body_summary = Some(body_summary);
    }
    if let Some(body_label) = patch.body_label {
        enforce_body_limit("body_label", &body_label)?;
        block.body_label = Some(body_label);
    }
    if let Some(tags) = patch.tags {
        enforce_list_limit("tags", tags.len(), MAX_TAGS)?;
        block.tags = tags;
    }
    if let Some(links) = patch.links {
        enforce_list_limit("links", links.len(), MAX_LINKS)?;
        block.links = convert_links(id, links)?;
    }
    if let Some(sources) = patch.sources {
        enforce_list_limit("sources", sources.len(), MAX_SOURCES)?;
        block.sources = convert_sources(sources);
    }
    if let Some(status) = patch.status {
        block.status = status;
    }
    if let Some(priority) = patch.priority {
        block.priority = priority;
    }

    warnings.extend(fill_missing_file_fingerprints(&mut block.sources, cwd, mode).await);
    block.touch();
    store.upsert(block.clone()).await.map_err(to_tool_error)?;
    Ok(block)
}

fn list_blocks(
    store: &BlockStore,
    filters: Option<MemoryListFilters>,
    limit: usize,
) -> Vec<BlockSummary> {
    let mut blocks = store.blocks().cloned().collect::<Vec<_>>();
    blocks.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut blocks = blocks
        .into_iter()
        .filter(|block| matches_filters(block, &filters));
    blocks
        .by_ref()
        .take(limit)
        .map(|block| BlockSummary::from_block(&block))
        .collect()
}

fn matches_filters(block: &Block, filters: &Option<MemoryListFilters>) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    if let Some(status) = filters.status
        && block.status != status
    {
        return false;
    }
    if let Some(kind) = filters.kind
        && block.kind != kind
    {
        return false;
    }
    if let Some(priority) = filters.priority
        && block.priority != priority
    {
        return false;
    }
    if let Some(tag) = &filters.tag
        && !block.tags.iter().any(|candidate| candidate == tag)
    {
        return false;
    }
    true
}

fn validate_block_input(block: &BlockInput) -> Result<(), FunctionCallError> {
    ensure_non_empty("id", &block.id)?;
    ensure_non_empty("title", &block.title)?;
    if let Some(body_full) = &block.body_full {
        enforce_body_limit("body_full", body_full)?;
    }
    if let Some(body_summary) = &block.body_summary {
        enforce_body_limit("body_summary", body_summary)?;
    }
    if let Some(body_label) = &block.body_label {
        enforce_body_limit("body_label", body_label)?;
    }
    if let Some(tags) = &block.tags {
        enforce_list_limit("tags", tags.len(), MAX_TAGS)?;
    }
    if let Some(links) = &block.links {
        enforce_list_limit("links", links.len(), MAX_LINKS)?;
    }
    if let Some(sources) = &block.sources {
        enforce_list_limit("sources", sources.len(), MAX_SOURCES)?;
    }
    Ok(())
}

fn validate_patch(patch: &BlockPatch) -> Result<(), FunctionCallError> {
    if patch.title.is_none()
        && patch.body_full.is_none()
        && patch.body_summary.is_none()
        && patch.body_label.is_none()
        && patch.tags.is_none()
        && patch.links.is_none()
        && patch.sources.is_none()
        && patch.status.is_none()
        && patch.priority.is_none()
    {
        return Err(err_msg("patch must include at least one field".to_string()));
    }
    if let Some(body_full) = &patch.body_full {
        enforce_body_limit("body_full", body_full)?;
    }
    if let Some(body_summary) = &patch.body_summary {
        enforce_body_limit("body_summary", body_summary)?;
    }
    if let Some(body_label) = &patch.body_label {
        enforce_body_limit("body_label", body_label)?;
    }
    if let Some(tags) = &patch.tags {
        enforce_list_limit("tags", tags.len(), MAX_TAGS)?;
    }
    if let Some(links) = &patch.links {
        enforce_list_limit("links", links.len(), MAX_LINKS)?;
    }
    if let Some(sources) = &patch.sources {
        enforce_list_limit("sources", sources.len(), MAX_SOURCES)?;
    }
    Ok(())
}

fn convert_links(id: &str, inputs: Vec<EdgeInput>) -> Result<Vec<Edge>, FunctionCallError> {
    let mut edges = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let Some(from) = &input.from
            && from != id
        {
            return Err(err_msg(format!(
                "link.from must match block id: expected {id}, got {from}"
            )));
        }
        edges.push(Edge {
            from: id.to_string(),
            to: input.to,
            rel: input.rel,
        });
    }
    Ok(edges)
}

fn convert_sources(inputs: Vec<SourceRefInput>) -> Vec<SourceRef> {
    inputs
        .into_iter()
        .map(|input| SourceRef {
            kind: input.kind,
            locator: input.locator,
            fingerprint: input.fingerprint,
        })
        .collect()
}

fn ensure_non_empty(field: &str, value: &str) -> Result<(), FunctionCallError> {
    if value.trim().is_empty() {
        return Err(err_msg(format!("{field} cannot be empty")));
    }
    Ok(())
}

fn enforce_body_limit(field: &str, body: &str) -> Result<(), FunctionCallError> {
    if body.len() > MAX_BLOCK_BODY_BYTES {
        return Err(err_msg(format!(
            "{field} exceeds {MAX_BLOCK_BODY_BYTES} bytes"
        )));
    }
    Ok(())
}

fn enforce_list_limit(field: &str, len: usize, max: usize) -> Result<(), FunctionCallError> {
    if len > max {
        return Err(err_msg(format!("{field} exceeds {max} entries")));
    }
    Ok(())
}

fn err_msg(message: String) -> FunctionCallError {
    FunctionCallError::RespondToModel(message)
}

fn to_tool_error(err: std::io::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("memory store error: {err}"))
}

impl BlockSummary {
    fn from_block(block: &Block) -> Self {
        Self {
            id: block.id.clone(),
            kind: block.kind,
            title: block.title.clone(),
            status: block.status,
            priority: block.priority,
            updated_at: block.updated_at,
        }
    }
}

impl BlockView {
    fn from_block(block: &Block, view: BlockViewKind) -> Self {
        let (representation, body) = match view {
            BlockViewKind::Full => (
                BlockViewKind::Full,
                block
                    .body_full
                    .clone()
                    .or_else(|| block.body_summary.clone())
                    .or_else(|| block.body_label.clone())
                    .unwrap_or_else(|| block.title.clone()),
            ),
            BlockViewKind::Summary => (
                BlockViewKind::Summary,
                block
                    .body_summary
                    .clone()
                    .or_else(|| block.body_full.clone())
                    .or_else(|| block.body_label.clone())
                    .unwrap_or_else(|| block.title.clone()),
            ),
            BlockViewKind::Label => (
                BlockViewKind::Label,
                block
                    .body_label
                    .clone()
                    .unwrap_or_else(|| block.title.clone()),
            ),
        };

        Self {
            id: block.id.clone(),
            kind: block.kind,
            title: block.title.clone(),
            status: block.status,
            priority: block.priority,
            representation,
            body,
            tags: block.tags.clone(),
            links: block.links.clone(),
            sources: block.sources.clone(),
            updated_at: block.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn memory_tool_upserts_and_lists_blocks() -> Result<(), FunctionCallError> {
        let temp = TempDir::new().expect("tempdir");
        let root = AbsolutePathBuf::try_from(temp.path().join("memory")).expect("root");
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await.expect("create cwd");

        let mut store = BlockStore::open(&root, &cwd).await.expect("open store");
        let request = MemoryToolRequest::Upsert {
            block: BlockInput {
                id: "b1".to_string(),
                kind: BlockKind::Facts,
                title: "fact".to_string(),
                body_full: Some("fact body".to_string()),
                body_summary: None,
                body_label: None,
                tags: Some(vec!["tag".to_string()]),
                links: None,
                sources: None,
                status: None,
                priority: Some(BlockPriority::High),
            },
        };

        let response = apply_memory_request(
            &mut store,
            &cwd,
            MemoryStalenessMode::MtimeSize,
            usize::MAX,
            request,
        )
        .await?;
        assert_eq!(response.action, MemoryAction::Upsert);

        let list = MemoryToolRequest::List {
            filters: None,
            limit: Some(10),
        };
        let response = apply_memory_request(
            &mut store,
            &cwd,
            MemoryStalenessMode::MtimeSize,
            usize::MAX,
            list,
        )
        .await?;
        match response.result {
            MemoryToolResult::Blocks { blocks } => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].id, "b1");
            }
            _ => return Err(err_msg("expected blocks list".to_string())),
        }

        Ok(())
    }

    #[tokio::test]
    async fn memory_tool_patches_status() -> Result<(), FunctionCallError> {
        let temp = TempDir::new().expect("tempdir");
        let root = AbsolutePathBuf::try_from(temp.path().join("memory")).expect("root");
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await.expect("create cwd");

        let mut store = BlockStore::open(&root, &cwd).await.expect("open store");
        let request = MemoryToolRequest::Upsert {
            block: BlockInput {
                id: "b2".to_string(),
                kind: BlockKind::Goals,
                title: "goal".to_string(),
                body_full: None,
                body_summary: None,
                body_label: None,
                tags: None,
                links: None,
                sources: None,
                status: None,
                priority: None,
            },
        };
        apply_memory_request(
            &mut store,
            &cwd,
            MemoryStalenessMode::MtimeSize,
            usize::MAX,
            request,
        )
        .await?;

        let patch = MemoryToolRequest::Patch {
            id: "b2".to_string(),
            patch: BlockPatch {
                title: None,
                body_full: None,
                body_summary: None,
                body_label: None,
                tags: None,
                links: None,
                sources: None,
                status: Some(BlockStatus::Stashed),
                priority: None,
            },
        };
        let response = apply_memory_request(
            &mut store,
            &cwd,
            MemoryStalenessMode::MtimeSize,
            usize::MAX,
            patch,
        )
        .await?;

        match response.result {
            MemoryToolResult::Patched { block } => {
                assert_eq!(block.status, BlockStatus::Stashed);
            }
            _ => return Err(err_msg("expected patched block".to_string())),
        }

        Ok(())
    }
}
