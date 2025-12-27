use crate::memory::BlockKind;
use crate::memory::BlockPriority;
use crate::memory::BlockStatus;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use codex_protocol::protocol::MEMORY_CONTEXT_CLOSE_TAG;
use codex_protocol::protocol::MEMORY_CONTEXT_OPEN_TAG;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRepresentation {
    Full,
    Summary,
    Label,
}

impl BlockRepresentation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Summary => "summary",
            Self::Label => "label",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryContextBlock {
    pub id: String,
    pub kind: BlockKind,
    pub status: BlockStatus,
    pub priority: BlockPriority,
    pub representation: BlockRepresentation,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct MemoryContext {
    pub project_id: String,
    pub blocks: Vec<MemoryContextBlock>,
}

impl MemoryContext {
    pub fn serialize_to_xml(&self) -> String {
        let mut lines = vec![MEMORY_CONTEXT_OPEN_TAG.to_string()];
        let project_id = &self.project_id;
        lines.push(format!("  <project_id>{project_id}</project_id>"));
        lines.push("  <blocks>".to_string());
        for block in &self.blocks {
            let id = &block.id;
            let kind = block_kind_name(block.kind);
            let status = block_status_name(block.status);
            let priority = block_priority_name(block.priority);
            let representation = block.representation.as_str();
            lines.push(format!(
                "    <block id=\"{id}\" kind=\"{kind}\" status=\"{status}\" priority=\"{priority}\" representation=\"{representation}\">",
            ));
            let title = &block.title;
            lines.push(format!("      <title>{title}</title>"));
            if !block.body.is_empty() {
                let body = &block.body;
                lines.push(format!("      <body>{body}</body>"));
            }
            lines.push("    </block>".to_string());
        }
        lines.push("  </blocks>".to_string());
        lines.push(MEMORY_CONTEXT_CLOSE_TAG.to_string());
        lines.join("\n")
    }
}

impl From<MemoryContext> for ResponseItem {
    fn from(ctx: MemoryContext) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: ctx.serialize_to_xml(),
            }],
        }
    }
}

pub fn memory_injection_index(items: &[ResponseItem]) -> usize {
    items
        .iter()
        .rposition(is_environment_context_item)
        .map(|idx| idx + 1)
        .unwrap_or(0)
}

fn is_environment_context_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { content, .. } => content.iter().any(|item| match item {
            ContentItem::InputText { text } => text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG),
            _ => false,
        }),
        _ => false,
    }
}

fn block_kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Focus => "focus",
        BlockKind::Goals => "goals",
        BlockKind::Constraints => "constraints",
        BlockKind::Decisions => "decisions",
        BlockKind::Facts => "facts",
        BlockKind::OpenQuestions => "open_questions",
        BlockKind::FileSummary => "file_summary",
        BlockKind::RepoMap => "repo_map",
        BlockKind::Workspace => "workspace",
        BlockKind::Toolbox => "toolbox",
        BlockKind::ToolSlice => "tool_slice",
        BlockKind::Plan => "plan",
    }
}

fn block_status_name(status: BlockStatus) -> &'static str {
    match status {
        BlockStatus::Active => "active",
        BlockStatus::Stashed => "stashed",
        BlockStatus::Stale => "stale",
    }
}

fn block_priority_name(priority: BlockPriority) -> &'static str {
    match priority {
        BlockPriority::Pinned => "pinned",
        BlockPriority::High => "high",
        BlockPriority::Normal => "normal",
        BlockPriority::Low => "low",
    }
}
