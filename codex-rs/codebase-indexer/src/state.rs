use crate::error::{IndexerError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// File indexing state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// File path relative to root
    pub path: PathBuf,

    /// SHA256 hash of file content
    pub content_hash: String,

    /// Last modification time
    pub modified_at: SystemTime,

    /// Number of chunks generated
    pub chunk_count: usize,

    /// Index timestamp
    pub indexed_at: SystemTime,
}

/// Index state tracking all indexed files
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IndexState {
    /// Version of the index format
    pub version: u32,

    /// Root directory being indexed
    pub root_dir: PathBuf,

    /// Map of file path -> file state
    pub files: HashMap<PathBuf, FileState>,

    /// Total number of chunks across all files
    pub total_chunks: usize,

    /// Last full index timestamp
    pub last_full_index: Option<SystemTime>,

    /// Last incremental update timestamp
    pub last_update: Option<SystemTime>,
}

impl IndexState {
    const CURRENT_VERSION: u32 = 1;
    const STATE_FILENAME: &'static str = "index-state.json";

    /// Create new index state
    pub fn new(root_dir: PathBuf) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root_dir,
            files: HashMap::new(),
            total_chunks: 0,
            last_full_index: None,
            last_update: None,
        }
    }

    /// Load index state from disk
    pub fn load(index_dir: &Path) -> Result<Self> {
        let state_path = index_dir.join(Self::STATE_FILENAME);

        if !state_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&state_path)?;
        let state: IndexState = serde_json::from_str(&content)?;

        // Validate version
        if state.version != Self::CURRENT_VERSION {
            log::warn!(
                "Index state version mismatch: {} vs {}. Rebuilding index.",
                state.version,
                Self::CURRENT_VERSION
            );
            return Ok(Self::default());
        }

        Ok(state)
    }

    /// Save index state to disk
    pub fn save(&self, index_dir: &Path) -> Result<()> {
        fs::create_dir_all(index_dir)?;

        let state_path = index_dir.join(Self::STATE_FILENAME);
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&state_path, content)?;

        Ok(())
    }

    /// Check if file needs reindexing
    pub fn needs_reindex(&self, file_path: &Path) -> Result<bool> {
        let relative_path = self.make_relative(file_path)?;

        // File not in index
        let Some(file_state) = self.files.get(&relative_path) else {
            return Ok(true);
        };

        // Check if file exists
        if !file_path.exists() {
            return Ok(false);
        }

        // Check modification time
        let metadata = fs::metadata(file_path)?;
        let modified = metadata.modified()?;

        if modified > file_state.modified_at {
            return Ok(true);
        }

        // Verify content hash
        let current_hash = Self::compute_file_hash(file_path)?;
        if current_hash != file_state.content_hash {
            return Ok(true);
        }

        Ok(false)
    }

    /// Update file state after indexing
    pub fn update_file(
        &mut self,
        file_path: &Path,
        chunk_count: usize,
    ) -> Result<()> {
        let relative_path = self.make_relative(file_path)?;
        let content_hash = Self::compute_file_hash(file_path)?;
        let metadata = fs::metadata(file_path)?;
        let modified_at = metadata.modified()?;
        let indexed_at = SystemTime::now();

        // Remove old chunk count if updating
        if let Some(old_state) = self.files.get(&relative_path) {
            self.total_chunks = self.total_chunks.saturating_sub(old_state.chunk_count);
        }

        let file_state = FileState {
            path: relative_path.clone(),
            content_hash,
            modified_at,
            chunk_count,
            indexed_at,
        };

        self.files.insert(relative_path, file_state);
        self.total_chunks += chunk_count;
        self.last_update = Some(SystemTime::now());

        Ok(())
    }

    /// Remove file from index
    pub fn remove_file(&mut self, file_path: &Path) -> Result<()> {
        let relative_path = self.make_relative(file_path)?;

        if let Some(file_state) = self.files.remove(&relative_path) {
            self.total_chunks = self.total_chunks.saturating_sub(file_state.chunk_count);
            self.last_update = Some(SystemTime::now());
        }

        Ok(())
    }

    /// Get files that need reindexing
    pub fn get_changed_files(&self, all_files: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut changed = Vec::new();

        for file_path in all_files {
            if self.needs_reindex(file_path)? {
                changed.push(file_path.clone());
            }
        }

        Ok(changed)
    }

    /// Mark as fully indexed
    pub fn mark_full_index(&mut self) {
        self.last_full_index = Some(SystemTime::now());
        self.last_update = Some(SystemTime::now());
    }

    /// Compute SHA256 hash of file content
    fn compute_file_hash(file_path: &Path) -> Result<String> {
        let content = fs::read(file_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = hasher.finalize();
        Ok(format!("{:x}", hash))
    }

    /// Convert absolute path to relative
    fn make_relative(&self, file_path: &Path) -> Result<PathBuf> {
        file_path
            .strip_prefix(&self.root_dir)
            .map(|p| p.to_path_buf())
            .map_err(|_| {
                IndexerError::InvalidPath(format!(
                    "Path {:?} is not relative to root {:?}",
                    file_path, self.root_dir
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_index_state_creation() {
        let state = IndexState::new(PathBuf::from("/tmp/test"));
        assert_eq!(state.version, IndexState::CURRENT_VERSION);
        assert_eq!(state.files.len(), 0);
        assert_eq!(state.total_chunks, 0);
    }

    #[test]
    fn test_file_hash_computation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = temp_dir.path().join("test.txt");

        let mut file = fs::File::create(&file_path).expect("Failed to create file");
        file.write_all(b"test content").expect("Failed to write");
        drop(file);

        let hash1 = IndexState::compute_file_hash(&file_path).expect("Failed to compute hash");
        let hash2 = IndexState::compute_file_hash(&file_path).expect("Failed to compute hash");

        assert_eq!(hash1, hash2);
        assert!(!hash1.is_empty());
    }

    #[test]
    fn test_state_persistence() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let index_dir = temp_dir.path().join("index");
        let root_dir = temp_dir.path().to_path_buf();

        let mut state = IndexState::new(root_dir.clone());
        state.total_chunks = 42;

        state.save(&index_dir).expect("Failed to save");

        let loaded = IndexState::load(&index_dir).expect("Failed to load");
        assert_eq!(loaded.total_chunks, 42);
        assert_eq!(loaded.root_dir, root_dir);
    }

    #[test]
    fn test_needs_reindex() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = temp_dir.path().join("test.rs");

        let mut file = fs::File::create(&file_path).expect("Failed to create file");
        file.write_all(b"fn main() {}").expect("Failed to write");
        drop(file);

        let mut state = IndexState::new(temp_dir.path().to_path_buf());

        // New file needs indexing
        assert!(state.needs_reindex(&file_path).expect("Failed to check"));

        // Update state
        state
            .update_file(&file_path, 1)
            .expect("Failed to update");

        // Now doesn't need reindexing
        assert!(!state.needs_reindex(&file_path).expect("Failed to check"));

        // Modify file
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(&file_path)
            .expect("Failed to open");
        file.write_all(b"\n// comment").expect("Failed to write");
        drop(file);

        // Now needs reindexing
        assert!(state.needs_reindex(&file_path).expect("Failed to check"));
    }
}
