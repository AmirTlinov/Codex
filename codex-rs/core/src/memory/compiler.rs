use crate::config::types::MemoryStalenessMode;
use crate::memory::block::Block;
use crate::memory::block::BlockPriority;
use crate::memory::block::BlockStatus;
use crate::memory::block::SourceKind;
use crate::memory::block::SourceRef;
use crate::memory::store::BlockStore;
use crate::memory_context::BlockRepresentation;
use crate::memory_context::MemoryContext;
use crate::memory_context::MemoryContextBlock;
use crate::truncate::approx_token_count;
use std::cmp::Ordering;
use std::io;
use std::path::Path;
use tokio::process::Command;
use tokio::time::Duration;
use tokio::time::timeout;

const GIT_HASH_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ContextCompiler {
    token_budget: usize,
    staleness_mode: MemoryStalenessMode,
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
    ) -> io::Result<MemoryContext> {
        let mut blocks: Vec<Block> = store.blocks().cloned().collect();
        blocks.extend(extra_blocks);
        blocks.sort_by(|a, b| compare_blocks(a, b));

        let mut remaining = self.token_budget;
        let mut compiled = Vec::new();

        for block in blocks {
            if block.status == BlockStatus::Stashed && block.priority != BlockPriority::Pinned {
                continue;
            }

            let stale = block_is_stale(&block, cwd, self.staleness_mode).await?;
            let status = if stale {
                BlockStatus::Stale
            } else {
                block.status
            };

            let candidate = BlockCandidate::from_block(&block, status);
            let representation = if stale {
                candidate.label(remaining)
            } else {
                candidate.pick(remaining)
            };

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

fn compare_blocks(a: &Block, b: &Block) -> Ordering {
    priority_rank(a.priority)
        .cmp(&priority_rank(b.priority))
        .then_with(|| b.updated_at.cmp(&a.updated_at))
        .then_with(|| a.id.cmp(&b.id))
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
    let path = resolve_source_path(source, cwd);
    let current = match mode {
        MemoryStalenessMode::GitOid => match fingerprint_git_oid(&path).await {
            Ok(fingerprint) => fingerprint,
            Err(_) => fingerprint_mtime_size(&path).await?,
        },
        MemoryStalenessMode::MtimeSize => fingerprint_mtime_size(&path).await?,
    };

    Ok(&current != expected)
}

fn resolve_source_path(source: &SourceRef, cwd: &Path) -> std::path::PathBuf {
    let path = Path::new(&source.locator);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

async fn fingerprint_git_oid(path: &Path) -> io::Result<crate::memory::block::Fingerprint> {
    let output = timeout(
        GIT_HASH_TIMEOUT,
        Command::new("git").arg("hash-object").arg(path).output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "git hash-object timed out"))?
    .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("{err}")))?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "git hash-object failed",
        ));
    }

    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(crate::memory::block::Fingerprint::GitOid { oid })
}

async fn fingerprint_mtime_size(path: &Path) -> io::Result<crate::memory::block::Fingerprint> {
    let metadata = tokio::fs::metadata(path).await?;
    let modified = metadata.modified()?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mtime_ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    Ok(crate::memory::block::Fingerprint::MtimeSize {
        mtime_ns,
        size_bytes: metadata.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block::Block;
    use crate::memory::block::BlockKind;
    use crate::memory::block::BlockPriority;
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
        let context = compiler.compile(&store, &cwd, Vec::new()).await?;
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
        let context = compiler.compile(&store, &cwd, Vec::new()).await?;
        let first = context.blocks.first().expect("compiled block");
        assert_eq!(first.representation, BlockRepresentation::Summary);

        Ok(())
    }
}
