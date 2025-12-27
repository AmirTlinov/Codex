use crate::config::types::MemoryStalenessMode;
use crate::memory::ContextSelection;
use crate::memory::block::Block;
use crate::memory::block::BlockPriority;
use crate::memory::block::BlockStatus;
use crate::memory::block::SourceKind;
use crate::memory::block::SourceRef;
use crate::memory::fingerprint::fingerprint_for_source;
use crate::memory::store::BlockStore;
use crate::memory_context::BlockRepresentation;
use crate::memory_context::MemoryContext;
use crate::memory_context::MemoryContextBlock;
use crate::truncate::approx_token_count;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;
const MAX_GRAPH_EXPANSION: usize = 64;

pub struct ContextCompiler {
    token_budget: usize,
    staleness_mode: MemoryStalenessMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockOrigin {
    Direct,
    Linked,
}

#[derive(Debug, Clone)]
struct BlockEntry {
    block: Block,
    origin: BlockOrigin,
}

impl ContextCompiler {
    pub fn new(token_budget: usize, staleness_mode: MemoryStalenessMode) -> Self {
        Self {
            token_budget,
            staleness_mode,
        }
    }

    pub async fn compile(
        &self,
        store: &BlockStore,
        cwd: &Path,
        extra_blocks: Vec<Block>,
        selection: Option<&ContextSelection>,
    ) -> io::Result<MemoryContext> {
        let mut entries = collect_entries(store, extra_blocks, selection);
        entries.sort_by(compare_entries);

        let mut remaining = self.token_budget;
        let mut compiled = Vec::new();

        for entry in entries {
            let block = entry.block;
            if block.status == BlockStatus::Stashed
                && block.priority != BlockPriority::Pinned
                && entry.origin == BlockOrigin::Direct
            {
                continue;
            }

            let stale = block_is_stale(&block, cwd, self.staleness_mode).await?;
            let status = if stale {
                BlockStatus::Stale
            } else {
                block.status
            };

            let candidate = BlockCandidate::from_block(&block, status);
            let representation =
                select_representation(&candidate, &block, status, entry.origin, remaining);

            let Some((representation, body, tokens)) = representation else {
                continue;
            };

            remaining = remaining.saturating_sub(tokens);
            compiled.push(MemoryContextBlock {
                id: block.id.clone(),
                kind: block.kind,
                status,
                priority: block.priority,
                representation,
                title: block.title.clone(),
                body,
            });
        }

        Ok(MemoryContext {
            project_id: store.project_id().to_string(),
            blocks: compiled,
        })
    }
}

fn collect_entries(
    store: &BlockStore,
    extra_blocks: Vec<Block>,
    selection: Option<&ContextSelection>,
) -> Vec<BlockEntry> {
    let mut store_blocks = HashMap::new();
    for block in store.blocks() {
        store_blocks.insert(block.id.clone(), block.clone());
    }

    let mut selected = HashSet::new();
    let mut entries = Vec::new();
    for block in store_blocks.values() {
        let selected_by_filter = selection.map(|selection| selection.contains(&block.id));
        match selected_by_filter {
            Some(selected_by_filter) => {
                if !selected_by_filter && block.priority != BlockPriority::Pinned {
                    continue;
                }
                if block.status == BlockStatus::Stashed
                    && block.priority != BlockPriority::Pinned
                    && !selected_by_filter
                {
                    continue;
                }
            }
            None => {
                if block.status == BlockStatus::Stashed && block.priority != BlockPriority::Pinned {
                    continue;
                }
            }
        }
        selected.insert(block.id.clone());
        entries.push(BlockEntry {
            block: block.clone(),
            origin: BlockOrigin::Direct,
        });
    }

    for block in extra_blocks {
        if selected.insert(block.id.clone()) {
            entries.push(BlockEntry {
                block,
                origin: BlockOrigin::Direct,
            });
        }
    }

    let mut ordered = entries
        .iter()
        .map(|entry| entry.block.clone())
        .collect::<Vec<_>>();
    ordered.sort_by(compare_blocks);

    let mut expanded = Vec::new();
    for block in ordered {
        if expanded.len() >= MAX_GRAPH_EXPANSION {
            break;
        }
        for link in &block.links {
            if expanded.len() >= MAX_GRAPH_EXPANSION {
                break;
            }
            if selected.contains(&link.to) {
                continue;
            }
            if let Some(target) = store_blocks.get(&link.to) {
                selected.insert(target.id.clone());
                expanded.push(BlockEntry {
                    block: target.clone(),
                    origin: BlockOrigin::Linked,
                });
            }
        }
    }

    entries.extend(expanded);
    entries
}

fn compare_entries(a: &BlockEntry, b: &BlockEntry) -> Ordering {
    priority_rank(a.block.priority)
        .cmp(&priority_rank(b.block.priority))
        .then_with(|| origin_rank(a.origin).cmp(&origin_rank(b.origin)))
        .then_with(|| b.block.updated_at.cmp(&a.block.updated_at))
        .then_with(|| a.block.id.cmp(&b.block.id))
}

fn compare_blocks(a: &Block, b: &Block) -> Ordering {
    priority_rank(a.priority)
        .cmp(&priority_rank(b.priority))
        .then_with(|| b.updated_at.cmp(&a.updated_at))
        .then_with(|| a.id.cmp(&b.id))
}

fn origin_rank(origin: BlockOrigin) -> u8 {
    match origin {
        BlockOrigin::Direct => 0,
        BlockOrigin::Linked => 1,
    }
}

fn priority_rank(priority: BlockPriority) -> u8 {
    match priority {
        BlockPriority::Pinned => 0,
        BlockPriority::High => 1,
        BlockPriority::Normal => 2,
        BlockPriority::Low => 3,
    }
}

struct BlockCandidate {
    title: String,
    full: String,
    summary: String,
    label: String,
}

impl BlockCandidate {
    fn from_block(block: &Block, status: BlockStatus) -> Self {
        let label = block
            .body_label
            .clone()
            .unwrap_or_else(|| block.title.clone());
        let summary = block
            .body_summary
            .clone()
            .or_else(|| block.body_full.clone())
            .unwrap_or_else(|| label.clone());
        let full = block
            .body_full
            .clone()
            .or_else(|| block.body_summary.clone())
            .unwrap_or_else(|| summary.clone());
        let label = if status == BlockStatus::Stale {
            format!("STALE: {label}")
        } else {
            label
        };

        Self {
            title: block.title.clone(),
            full,
            summary,
            label,
        }
    }

    fn pick(&self, budget: usize) -> Option<(BlockRepresentation, String, usize)> {
        self.full(budget)
            .or_else(|| self.summary(budget))
            .or_else(|| self.label(budget))
    }

    fn full(&self, budget: usize) -> Option<(BlockRepresentation, String, usize)> {
        let tokens = estimate_tokens(&self.title, &self.full);
        if tokens <= budget {
            Some((BlockRepresentation::Full, self.full.clone(), tokens))
        } else {
            None
        }
    }

    fn summary(&self, budget: usize) -> Option<(BlockRepresentation, String, usize)> {
        let tokens = estimate_tokens(&self.title, &self.summary);
        if tokens <= budget {
            Some((BlockRepresentation::Summary, self.summary.clone(), tokens))
        } else {
            None
        }
    }

    fn label(&self, budget: usize) -> Option<(BlockRepresentation, String, usize)> {
        let tokens = estimate_tokens(&self.title, &self.label);
        if tokens <= budget {
            Some((BlockRepresentation::Label, self.label.clone(), tokens))
        } else {
            None
        }
    }

    fn summary_or_label(&self, budget: usize) -> Option<(BlockRepresentation, String, usize)> {
        self.summary(budget).or_else(|| self.label(budget))
    }
}

fn select_representation(
    candidate: &BlockCandidate,
    block: &Block,
    status: BlockStatus,
    origin: BlockOrigin,
    budget: usize,
) -> Option<(BlockRepresentation, String, usize)> {
    if status == BlockStatus::Stale
        || (block.status == BlockStatus::Stashed && block.priority != BlockPriority::Pinned)
    {
        candidate.label(budget)
    } else if origin == BlockOrigin::Linked {
        candidate.summary_or_label(budget)
    } else {
        candidate.pick(budget)
    }
}

fn estimate_tokens(title: &str, body: &str) -> usize {
    approx_token_count(title)
        .saturating_add(approx_token_count(body))
        .saturating_add(1)
}

async fn block_is_stale(block: &Block, cwd: &Path, mode: MemoryStalenessMode) -> io::Result<bool> {
    for source in &block.sources {
        if source.kind != SourceKind::FilePath {
            continue;
        }
        if source.fingerprint.is_none() {
            return Ok(true);
        }
        let stale = source_is_stale(source, cwd, mode).await?;
        if stale {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn source_is_stale(
    source: &SourceRef,
    cwd: &Path,
    mode: MemoryStalenessMode,
) -> io::Result<bool> {
    let Some(expected) = &source.fingerprint else {
        return Ok(true);
    };
    let current = fingerprint_for_source(source, cwd, mode).await?;

    Ok(&current != expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block::Block;
    use crate::memory::block::BlockKind;
    use crate::memory::block::BlockPriority;
    use crate::memory::block::BlockStatus;
    use crate::memory::block::Edge;
    use crate::memory::block::EdgeKind;
    use crate::memory_context::BlockRepresentation;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn compiler_prioritizes_pinned_blocks() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let mut pinned = Block::new("pinned", BlockKind::Goals, "goal").with_updated_at(2);
        pinned.priority = BlockPriority::Pinned;
        let normal = Block::new("normal", BlockKind::Facts, "fact").with_updated_at(1);
        store.upsert(normal).await?;
        store.upsert(pinned).await?;

        let compiler = ContextCompiler::new(1024, MemoryStalenessMode::MtimeSize);
        let context = compiler.compile(&store, &cwd, Vec::new(), None).await?;
        assert_eq!(
            context.blocks.first().map(|b| b.id.as_str()),
            Some("pinned")
        );

        Ok(())
    }

    #[tokio::test]
    async fn compiler_degrades_representation_to_fit_budget() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let mut block = Block::new("block", BlockKind::Decisions, "decision").with_updated_at(1);
        block.body_full = Some("full full full".to_string());
        block.body_summary = Some("summary".to_string());
        block.body_label = Some("label".to_string());
        store.upsert(block).await?;

        let summary_tokens = estimate_tokens("decision", "summary");
        let compiler = ContextCompiler::new(summary_tokens, MemoryStalenessMode::MtimeSize);
        let context = compiler.compile(&store, &cwd, Vec::new(), None).await?;
        let first = context.blocks.first().expect("compiled block");
        assert_eq!(first.representation, BlockRepresentation::Summary);

        Ok(())
    }

    #[tokio::test]
    async fn compiler_expands_linked_blocks() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let mut seed = Block::new("seed", BlockKind::Decisions, "seed").with_updated_at(2);
        seed.links.push(Edge {
            from: "seed".to_string(),
            to: "archived".to_string(),
            rel: EdgeKind::Explains,
        });
        store.upsert(seed).await?;

        let mut archived = Block::new("archived", BlockKind::Facts, "archived").with_updated_at(1);
        archived.status = BlockStatus::Stashed;
        archived.body_label = Some("archived label".to_string());
        store.upsert(archived).await?;

        let compiler = ContextCompiler::new(1024, MemoryStalenessMode::MtimeSize);
        let context = compiler.compile(&store, &cwd, Vec::new(), None).await?;
        let archived_block = context
            .blocks
            .iter()
            .find(|block| block.id == "archived")
            .expect("linked block");
        assert_eq!(archived_block.representation, BlockRepresentation::Label);

        Ok(())
    }
}
