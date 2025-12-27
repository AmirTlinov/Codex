use crate::git_info::get_git_repo_root;
use crate::memory::block::Block;
use crate::memory::block::BlockKind;
use crate::memory::block::BlockPriority;
use crate::memory::block::BlockStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use sha1::Digest;
use sha1::Sha1;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::AsyncBufReadExt;

const MEMORY_LOG_FILENAME: &str = "memory.log.jsonl";
const MEMORY_SNAPSHOT_FILENAME: &str = "snapshot.json";
#[allow(dead_code)]
const SNAPSHOT_VERSION: u64 = 1;
const MEMORY_SOFT_CAP_RATIO: f64 = 0.8;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemorySnapshot {
    pub version: u64,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryEvent {
    Upsert { block: Block },
    Delete { id: String },
}

pub struct BlockStore {
    root_dir: AbsolutePathBuf,
    project_id: String,
    #[allow(dead_code)]
    log_path: PathBuf,
    #[allow(dead_code)]
    snapshot_path: PathBuf,
    blocks: HashMap<String, Block>,
}

impl BlockStore {
    pub async fn open(root_dir: &AbsolutePathBuf, cwd: &Path) -> io::Result<Self> {
        let project_id = project_id_for_path(cwd);
        let project_dir = root_dir.as_path().join(&project_id);
        tokio::fs::create_dir_all(&project_dir).await?;

        let log_path = project_dir.join(MEMORY_LOG_FILENAME);
        let snapshot_path = project_dir.join(MEMORY_SNAPSHOT_FILENAME);
        let mut blocks = HashMap::new();

        match load_snapshot(&snapshot_path).await {
            Ok(snapshot) => {
                for block in snapshot.blocks {
                    blocks.insert(block.id.clone(), block);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }

        if tokio::fs::try_exists(&log_path).await? {
            load_log(&log_path, &mut blocks).await?;
        }

        Ok(Self {
            root_dir: root_dir.clone(),
            project_id,
            log_path,
            snapshot_path,
            blocks,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root_dir(&self) -> &AbsolutePathBuf {
        &self.root_dir
    }

    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<&Block> {
        self.blocks.get(id)
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.blocks.values()
    }

    #[allow(dead_code)]
    pub async fn upsert(&mut self, block: Block) -> io::Result<()> {
        let event = MemoryEvent::Upsert {
            block: block.clone(),
        };
        self.append_event(&event).await?;
        self.blocks.insert(block.id.clone(), block);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn delete(&mut self, id: &str) -> io::Result<()> {
        let event = MemoryEvent::Delete { id: id.to_string() };
        self.append_event(&event).await?;
        self.blocks.remove(id);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn snapshot(&self) -> io::Result<()> {
        let blocks = sorted_blocks(self.blocks.values());
        write_snapshot(&self.snapshot_path, &blocks).await
    }

    pub async fn enforce_budget(&mut self, max_bytes: usize) -> io::Result<Vec<String>> {
        let Some(max_bytes) = u64::try_from(max_bytes).ok().filter(|max| *max > 0) else {
            return Ok(Vec::new());
        };

        let current_bytes = archive_bytes(&self.log_path, &self.snapshot_path).await?;
        if current_bytes <= max_bytes {
            return Ok(Vec::new());
        }

        let target_bytes = soft_cap_bytes(max_bytes);
        let mut retained = sorted_blocks(self.blocks.values());
        let mut snapshot = snapshot_bytes(&retained)?;
        let mut removed = Vec::new();

        if u64::try_from(snapshot.len()).unwrap_or(u64::MAX) > target_bytes {
            let mut candidates = retained
                .iter()
                .filter(|block| block.priority != BlockPriority::Pinned)
                .cloned()
                .collect::<Vec<_>>();
            candidates.sort_by_key(eviction_key);

            for candidate in candidates {
                if u64::try_from(snapshot.len()).unwrap_or(u64::MAX) <= target_bytes {
                    break;
                }
                retained.retain(|block| block.id != candidate.id);
                removed.push(candidate.id);
                snapshot = snapshot_bytes(&retained)?;
            }
        }

        self.blocks = retained
            .iter()
            .cloned()
            .map(|block| (block.id.clone(), block))
            .collect();
        write_snapshot(&self.snapshot_path, &retained).await?;
        truncate_log(&self.log_path).await?;

        let mut warnings = Vec::new();
        if !removed.is_empty() {
            warnings.push(format!(
                "evicted {count} blocks to honor memory.max_bytes: {ids}",
                count = removed.len(),
                ids = removed.join(", ")
            ));
        }
        if u64::try_from(snapshot.len()).unwrap_or(u64::MAX) > max_bytes {
            warnings.push(format!(
                "memory snapshot still exceeds max_bytes ({size} > {max}); pinned blocks retained",
                size = snapshot.len(),
                max = max_bytes
            ));
        }

        Ok(warnings)
    }

    #[allow(dead_code)]
    async fn append_event(&self, event: &MemoryEvent) -> io::Result<()> {
        let mut line = serde_json::to_string(event).map_err(to_io_error)?;
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .await?;
        use tokio::io::AsyncWriteExt;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}

fn project_id_for_path(cwd: &Path) -> String {
    let root = get_git_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let canonical = dunce::canonicalize(&root).unwrap_or(root);
    let mut hasher = Sha1::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

async fn load_snapshot(path: &Path) -> io::Result<MemorySnapshot> {
    let data = tokio::fs::read_to_string(path).await?;
    serde_json::from_str(&data).map_err(to_io_error)
}

async fn load_log(path: &Path, blocks: &mut HashMap<String, Block>) -> io::Result<()> {
    let file = tokio::fs::File::open(path).await?;
    let mut lines = tokio::io::BufReader::new(file).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let event: MemoryEvent = serde_json::from_str(&line).map_err(to_io_error)?;
        apply_event(blocks, event);
    }
    Ok(())
}

fn apply_event(blocks: &mut HashMap<String, Block>, event: MemoryEvent) {
    match event {
        MemoryEvent::Upsert { block } => {
            blocks.insert(block.id.clone(), block);
        }
        MemoryEvent::Delete { id } => {
            blocks.remove(&id);
        }
    }
}

fn to_io_error(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{err}"))
}

fn sorted_blocks<'a>(blocks: impl Iterator<Item = &'a Block>) -> Vec<Block> {
    let mut blocks = blocks.cloned().collect::<Vec<_>>();
    blocks.sort_by(|a, b| a.id.cmp(&b.id));
    blocks
}

async fn write_snapshot(path: &Path, blocks: &[Block]) -> io::Result<()> {
    let snapshot = MemorySnapshot {
        version: SNAPSHOT_VERSION,
        blocks: blocks.to_vec(),
    };
    let data = serde_json::to_vec_pretty(&snapshot).map_err(to_io_error)?;
    let tmp_path = path.with_extension("tmp");
    tokio::fs::write(&tmp_path, data).await?;
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
}

fn snapshot_bytes(blocks: &[Block]) -> io::Result<Vec<u8>> {
    let snapshot = MemorySnapshot {
        version: SNAPSHOT_VERSION,
        blocks: blocks.to_vec(),
    };
    serde_json::to_vec_pretty(&snapshot).map_err(to_io_error)
}

async fn archive_bytes(log_path: &Path, snapshot_path: &Path) -> io::Result<u64> {
    let log_bytes = file_len(log_path).await?;
    let snapshot_bytes = file_len(snapshot_path).await?;
    Ok(log_bytes.saturating_add(snapshot_bytes))
}

async fn file_len(path: &Path) -> io::Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err),
    }
}

async fn truncate_log(path: &Path) -> io::Result<()> {
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    file.set_len(0).await?;
    Ok(())
}

fn soft_cap_bytes(max_bytes: u64) -> u64 {
    ((max_bytes as f64) * MEMORY_SOFT_CAP_RATIO)
        .floor()
        .clamp(1.0, max_bytes as f64) as u64
}

fn eviction_key(block: &Block) -> (u8, u8, u8, u64, String) {
    (
        status_rank(block.status),
        priority_rank(block.priority),
        kind_rank(block.kind),
        block.updated_at,
        block.id.clone(),
    )
}

fn status_rank(status: BlockStatus) -> u8 {
    match status {
        BlockStatus::Stale => 0,
        BlockStatus::Stashed => 1,
        BlockStatus::Active => 2,
    }
}

fn priority_rank(priority: BlockPriority) -> u8 {
    match priority {
        BlockPriority::Low => 0,
        BlockPriority::Normal => 1,
        BlockPriority::High => 2,
        BlockPriority::Pinned => 3,
    }
}

fn kind_rank(kind: BlockKind) -> u8 {
    match kind {
        BlockKind::ToolSlice => 0,
        BlockKind::RepoMap => 1,
        BlockKind::FileSummary => 2,
        BlockKind::OpenQuestions => 3,
        BlockKind::Facts => 4,
        BlockKind::Decisions => 5,
        BlockKind::Plan => 6,
        BlockKind::Constraints => 7,
        BlockKind::Goals => 8,
        BlockKind::Focus => 9,
        BlockKind::Toolbox => 10,
        BlockKind::Workspace => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::block::BlockKind;
    use crate::memory::block::BlockPriority;
    use crate::memory::block::BlockStatus;
    use tempfile::TempDir;

    #[tokio::test]
    async fn block_store_replays_log_entries() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let block = Block::new("block-1", BlockKind::Facts, "fact").with_updated_at(1);
        store.upsert(block.clone()).await?;
        store.delete("missing").await?;

        drop(store);
        let store = BlockStore::open(&root, &cwd).await?;
        assert_eq!(store.get("block-1"), Some(&block));

        Ok(())
    }

    #[tokio::test]
    async fn block_store_snapshot_round_trip() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let block = Block {
            id: "block-2".to_string(),
            kind: BlockKind::Decisions,
            title: "decision".to_string(),
            body_full: Some("full".to_string()),
            body_summary: Some("summary".to_string()),
            body_label: Some("label".to_string()),
            tags: vec!["tag".to_string()],
            links: Vec::new(),
            sources: Vec::new(),
            status: BlockStatus::Active,
            priority: BlockPriority::Pinned,
            updated_at: 2,
        };
        store.upsert(block.clone()).await?;
        store.snapshot().await?;

        drop(store);
        let store = BlockStore::open(&root, &cwd).await?;
        assert_eq!(store.get("block-2"), Some(&block));

        Ok(())
    }

    #[tokio::test]
    async fn block_store_enforces_budget_eviction_order() -> io::Result<()> {
        let temp = TempDir::new()?;
        let root = AbsolutePathBuf::try_from(temp.path().join("memory"))?;
        let cwd = temp.path().join("project");
        tokio::fs::create_dir_all(&cwd).await?;

        let mut store = BlockStore::open(&root, &cwd).await?;
        let mut pinned = Block::new("pinned", BlockKind::Goals, "goal");
        pinned.priority = BlockPriority::Pinned;
        pinned.body_full = Some("pinned".to_string());

        let mut stashed = Block::new("stashed", BlockKind::ToolSlice, "slice");
        stashed.status = BlockStatus::Stashed;
        stashed.body_full = Some("x".repeat(4096));

        let mut active = Block::new("active", BlockKind::Facts, "fact");
        active.body_full = Some("y".repeat(4096));

        store.upsert(pinned).await?;
        store.upsert(stashed).await?;
        store.upsert(active).await?;

        let warnings = store.enforce_budget(1024).await?;
        assert!(store.get("pinned").is_some());
        assert!(store.get("stashed").is_none());
        assert!(warnings.iter().any(|w| w.contains("evicted")));
        assert_eq!(file_len(&store.log_path).await?, 0);

        Ok(())
    }
}
