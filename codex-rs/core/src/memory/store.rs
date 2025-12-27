use crate::git_info::get_git_repo_root;
use crate::memory::block::Block;
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
        let snapshot = MemorySnapshot {
            version: SNAPSHOT_VERSION,
            blocks: self.blocks.values().cloned().collect(),
        };
        let data = serde_json::to_vec_pretty(&snapshot).map_err(to_io_error)?;
        let tmp_path = self.snapshot_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, data).await?;
        tokio::fs::rename(&tmp_path, &self.snapshot_path).await?;
        Ok(())
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
}
