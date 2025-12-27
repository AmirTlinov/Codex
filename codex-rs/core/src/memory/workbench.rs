use crate::event_mapping::parse_turn_item;
use crate::memory::block::Block;
use crate::memory::block::BlockKind;
use crate::memory::block::BlockPriority;
use crate::memory::block::BlockStatus;
use crate::memory::block::SourceKind;
use crate::memory::block::SourceRef;
use crate::memory::store::BlockStore;
use crate::truncate::TruncationPolicy;
use crate::truncate::truncate_text;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::user_input::UserInput;
use std::collections::HashSet;
use std::io;

const DEFAULT_MAX_SELECTED_BLOCKS: usize = 24;
const MAX_QUERY_TERMS: usize = 24;
const MAX_FOCUS_BYTES: usize = 2048;
const MAX_FOCUS_SUMMARY_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 160;
const MAX_PLAN_SUMMARY_BYTES: usize = 1024;

#[derive(Debug, Clone, Default)]
pub struct ContextSelection {
    ids: HashSet<String>,
}

impl ContextSelection {
    pub fn new(ids: HashSet<String>) -> Self {
        Self { ids }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }
}

#[derive(Debug, Clone)]
pub struct MemoryWorkbench {
    max_selected_blocks: usize,
}

impl Default for MemoryWorkbench {
    fn default() -> Self {
        Self {
            max_selected_blocks: DEFAULT_MAX_SELECTED_BLOCKS,
        }
    }
}

impl MemoryWorkbench {
    pub fn select(&self, store: &BlockStore, query: &str) -> ContextSelection {
        let terms = extract_terms(query);
        let mut selected = HashSet::new();
        let mut candidates = Vec::new();

        for block in store.blocks() {
            if block.priority == BlockPriority::Pinned
                || matches!(
                    block.kind,
                    BlockKind::Focus | BlockKind::Constraints | BlockKind::Goals | BlockKind::Plan
                )
            {
                selected.insert(block.id.clone());
                continue;
            }

            if block.status == BlockStatus::Stashed && terms.is_empty() {
                continue;
            }

            let score = score_block(block, &terms);
            candidates.push((score, block.updated_at, block.id.clone()));
        }

        candidates.sort_by(|(score_a, updated_a, id_a), (score_b, updated_b, id_b)| {
            score_b
                .cmp(score_a)
                .then_with(|| updated_b.cmp(updated_a))
                .then_with(|| id_a.cmp(id_b))
        });

        let mut remaining = self.max_selected_blocks.saturating_sub(selected.len());
        for (score, _updated_at, id) in candidates {
            if score <= 0 || remaining == 0 || selected.len() >= self.max_selected_blocks {
                continue;
            }
            if selected.insert(id) {
                remaining = remaining.saturating_sub(1);
            }
        }

        ContextSelection::new(selected)
    }

    pub async fn sync_focus(
        &self,
        store: &mut BlockStore,
        turn_id: &str,
        items: &[ResponseItem],
    ) -> io::Result<bool> {
        let Some(text) = latest_user_text(items) else {
            return Ok(false);
        };
        let full = truncate_text(text.trim(), TruncationPolicy::Bytes(MAX_FOCUS_BYTES));
        let summary = truncate_text(
            text.trim(),
            TruncationPolicy::Bytes(MAX_FOCUS_SUMMARY_BYTES),
        );
        let label = build_label("Focus", &full);

        if let Some(existing) = store.get("focus")
            && existing.body_full.as_deref() == Some(full.as_str())
        {
            return Ok(false);
        }

        let mut block = Block::new("focus", BlockKind::Focus, "Focus");
        block.priority = BlockPriority::Pinned;
        block.status = BlockStatus::Active;
        block.body_full = Some(full);
        block.body_summary = Some(summary);
        block.body_label = Some(label);
        block.tags = vec!["focus".to_string()];
        block.sources = vec![SourceRef {
            kind: SourceKind::Conversation,
            locator: format!("turn:{turn_id}"),
            fingerprint: None,
        }];

        store.upsert(block).await?;
        Ok(true)
    }

    pub async fn apply_plan_update(
        &self,
        store: &mut BlockStore,
        turn_id: &str,
        args: &UpdatePlanArgs,
    ) -> io::Result<bool> {
        let full = format_plan_body(args);
        let summary = truncate_text(full.trim(), TruncationPolicy::Bytes(MAX_PLAN_SUMMARY_BYTES));
        let label = format_plan_label(args);

        if let Some(existing) = store.get("plan")
            && existing.body_full.as_deref() == Some(full.as_str())
        {
            return Ok(false);
        }

        let mut block = Block::new("plan", BlockKind::Plan, "Plan");
        block.priority = BlockPriority::High;
        block.status = BlockStatus::Active;
        block.body_full = Some(full);
        block.body_summary = Some(summary);
        block.body_label = Some(label);
        block.tags = vec!["plan".to_string()];
        block.sources = vec![SourceRef {
            kind: SourceKind::ToolOutput,
            locator: format!("update_plan:{turn_id}"),
            fingerprint: None,
        }];

        store.upsert(block).await?;
        Ok(true)
    }
}

fn latest_user_text(items: &[ResponseItem]) -> Option<String> {
    items
        .iter()
        .rev()
        .find_map(|item| match parse_turn_item(item) {
            Some(TurnItem::UserMessage(message)) => user_message_text(&message),
            _ => None,
        })
}

fn user_message_text(message: &codex_protocol::items::UserMessageItem) -> Option<String> {
    let mut chunks = Vec::new();
    for entry in &message.content {
        if let UserInput::Text { text } = entry {
            if !text.trim().is_empty() {
                chunks.push(text.trim().to_string());
            }
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n"))
    }
}

fn extract_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for term in query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 2)
    {
        if terms.len() >= MAX_QUERY_TERMS {
            break;
        }
        let lowered = term.to_ascii_lowercase();
        if !terms.contains(&lowered) {
            terms.push(lowered);
        }
    }
    terms
}

fn score_block(block: &Block, terms: &[String]) -> i64 {
    let base_score = priority_weight(block.priority) + kind_weight(block.kind);
    if block.status == BlockStatus::Stashed {
        if terms.is_empty() {
            return base_score.saturating_sub(20);
        }
    }
    if block.status == BlockStatus::Stale {
        if terms.is_empty() {
            return base_score.saturating_sub(10);
        }
    }
    if terms.is_empty() {
        return base_score;
    }

    let title = block.title.to_ascii_lowercase();
    let body = block
        .body_summary
        .as_deref()
        .or(block.body_label.as_deref())
        .unwrap_or_default();
    let body = body.to_ascii_lowercase();

    let mut match_score = 0;
    for term in terms {
        if title.contains(term) {
            match_score += 8;
        }
        if body.contains(term) {
            match_score += 3;
        }
        if block.tags.iter().any(|tag| tag.eq_ignore_ascii_case(term)) {
            match_score += 10;
        }
    }

    if match_score == 0 {
        0
    } else {
        base_score + match_score
    }
}

fn priority_weight(priority: BlockPriority) -> i64 {
    match priority {
        BlockPriority::Pinned => 1000,
        BlockPriority::High => 600,
        BlockPriority::Normal => 300,
        BlockPriority::Low => 100,
    }
}

fn kind_weight(kind: BlockKind) -> i64 {
    match kind {
        BlockKind::Focus => 250,
        BlockKind::Constraints => 200,
        BlockKind::Goals => 180,
        BlockKind::Plan => 170,
        BlockKind::Decisions => 150,
        BlockKind::Facts => 120,
        BlockKind::OpenQuestions => 110,
        BlockKind::FileSummary => 100,
        BlockKind::RepoMap => 90,
        BlockKind::Toolbox => 60,
        BlockKind::Workspace => 40,
        BlockKind::ToolSlice => 30,
    }
}

fn build_label(prefix: &str, text: &str) -> String {
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim();
    let label = if first_line.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {first_line}")
    };
    truncate_text(&label, TruncationPolicy::Bytes(MAX_LABEL_BYTES))
}

fn format_plan_body(args: &UpdatePlanArgs) -> String {
    let mut lines = Vec::new();
    if let Some(explanation) = args.explanation.as_ref() {
        let explanation = explanation.trim();
        if !explanation.is_empty() {
            lines.push(format!("Explanation: {explanation}"));
            lines.push(String::new());
        }
    }

    lines.push("Steps:".to_string());
    for item in &args.plan {
        let status = plan_status_label(&item.status);
        let step = item.step.trim();
        lines.push(format!("- [{status}] {step}"));
    }

    if lines.len() == 1 {
        lines.push("(empty)".to_string());
    }

    lines.join("\n")
}

fn format_plan_label(args: &UpdatePlanArgs) -> String {
    let chosen = args
        .plan
        .iter()
        .find(|item| matches!(item.status, StepStatus::InProgress))
        .or_else(|| args.plan.first())
        .map(|item| item.step.trim());

    if let Some(step) = chosen
        && !step.is_empty()
    {
        let label = format!("Plan: {step}");
        return truncate_text(&label, TruncationPolicy::Bytes(MAX_LABEL_BYTES));
    }

    "Plan: (empty)".to_string()
}

fn plan_status_label(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "in_progress",
        StepStatus::Completed => "completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block::BlockKind;
    use crate::memory::block::BlockPriority;
    use crate::memory::block::BlockStatus;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn sync_focus_upserts_focus_block() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;
        let mut store = BlockStore::open(&root, &cwd).await?;

        let items = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::InputText {
                text: "Ship it".to_string(),
            }],
        }];

        let workbench = MemoryWorkbench::default();
        let updated = workbench.sync_focus(&mut store, "turn-1", &items).await?;
        assert_eq!(updated, true);

        let block = store.get("focus").expect("focus block");
        assert_eq!(block.kind, BlockKind::Focus);
        assert_eq!(block.priority, BlockPriority::Pinned);
        assert_eq!(block.status, BlockStatus::Active);
        assert_eq!(block.body_full.as_deref(), Some("Ship it"));

        Ok(())
    }

    #[tokio::test]
    async fn select_includes_pinned_and_focus() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;
        let mut store = BlockStore::open(&root, &cwd).await?;

        let mut pinned = Block::new("pinned", BlockKind::Goals, "Goals");
        pinned.priority = BlockPriority::Pinned;
        store.upsert(pinned).await?;

        let focus = Block::new("focus", BlockKind::Focus, "Focus");
        store.upsert(focus).await?;

        let normal = Block::new("facts", BlockKind::Facts, "Facts");
        store.upsert(normal).await?;

        let selection = MemoryWorkbench::default().select(&store, "focus");
        assert!(selection.contains("focus"));
        assert!(selection.contains("pinned"));
        assert!(!selection.contains("facts"));

        Ok(())
    }
}
