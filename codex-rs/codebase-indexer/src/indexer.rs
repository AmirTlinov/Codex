use crate::config::IndexerConfig;
use crate::error::{IndexerError, Result};
use crate::state::IndexState;
use codex_code_chunker::{Chunker, ChunkerConfig};
use codex_vector_store::VectorStore;
use ignore::WalkBuilder;
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

/// Progress callback for indexing operations
pub type ProgressCallback = Arc<dyn Fn(IndexProgress) + Send + Sync>;

/// Indexing progress information
#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: IndexPhase,
    pub current: usize,
    pub total: usize,
    pub current_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexPhase {
    Discovering,
    Chunking,
    Embedding,
    Storing,
    Complete,
}

/// Statistics about indexing operation
#[derive(Debug, Default)]
pub struct IndexStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub chunks_created: usize,
    pub chunks_embedded: usize,
}

/// Main codebase indexer with incremental support
pub struct CodebaseIndexer {
    config: IndexerConfig,
    vector_store: Arc<Mutex<VectorStore>>,
    state: Arc<Mutex<IndexState>>,
}

impl CodebaseIndexer {
    /// Create new indexer
    pub async fn new(config: IndexerConfig) -> Result<Self> {
        config.validate().map_err(IndexerError::IndexState)?;

        // Initialize vector store
        let store_path = config.index_dir.join("vectors.json");
        let vector_store = VectorStore::new(&store_path).await?;

        // Load existing state
        let mut state = IndexState::load(&config.index_dir)?;
        state.root_dir = config.root_dir.clone();

        Ok(Self {
            config,
            vector_store: Arc::new(Mutex::new(vector_store)),
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Index the codebase
    pub async fn index(&self, progress_callback: Option<ProgressCallback>) -> Result<IndexStats> {
        info!("Starting codebase indexing in {:?}", self.config.root_dir);

        let mut stats = IndexStats::default();

        // Discover files
        self.report_progress(
            &progress_callback,
            IndexPhase::Discovering,
            0,
            0,
            None,
        );

        let all_files = self.discover_files()?;
        info!("Discovered {} files", all_files.len());

        // Determine which files need indexing
        let files_to_index = if self.config.incremental {
            let state = self.state.lock().await;
            let changed = state.get_changed_files(&all_files)?;
            drop(state);
            info!(
                "Incremental mode: {} of {} files need reindexing",
                changed.len(),
                all_files.len()
            );
            stats.files_skipped = all_files.len() - changed.len();
            changed
        } else {
            info!("Full reindex mode: processing all {} files", all_files.len());
            all_files
        };

        if files_to_index.is_empty() {
            info!("No files need indexing");
            return Ok(stats);
        }

        // Process files in batches
        let total_files = files_to_index.len();
        for (batch_idx, batch) in files_to_index.chunks(self.config.batch_size).enumerate() {
            let batch_start = batch_idx * self.config.batch_size;

            info!(
                "Processing batch {}/{} ({} files)",
                batch_idx + 1,
                (total_files + self.config.batch_size - 1) / self.config.batch_size,
                batch.len()
            );

            let batch_stats = self
                .process_batch(batch, batch_start, total_files, &progress_callback)
                .await?;

            stats.files_processed += batch_stats.files_processed;
            stats.files_failed += batch_stats.files_failed;
            stats.chunks_created += batch_stats.chunks_created;
            stats.chunks_embedded += batch_stats.chunks_embedded;
        }

        // Update state
        let mut state = self.state.lock().await;
        if !self.config.incremental {
            state.mark_full_index();
        }
        state.save(&self.config.index_dir)?;
        drop(state);

        self.report_progress(
            &progress_callback,
            IndexPhase::Complete,
            total_files,
            total_files,
            None,
        );

        info!(
            "Indexing complete: {} files processed, {} chunks created",
            stats.files_processed, stats.chunks_created
        );

        Ok(stats)
    }

    /// Process a batch of files concurrently
    async fn process_batch(
        &self,
        files: &[PathBuf],
        batch_offset: usize,
        total: usize,
        progress_callback: &Option<ProgressCallback>,
    ) -> Result<IndexStats> {
        let mut stats = IndexStats::default();
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent));

        let mut tasks = Vec::new();

        for (idx, file_path) in files.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await.map_err(|e| {
                IndexerError::IndexState(format!("Semaphore error: {}", e))
            })?;

            let file_path = file_path.clone();
            let chunker_config = self.config.chunker.clone();
            let vector_store = self.vector_store.clone();
            let state = self.state.clone();
            let current_idx = batch_offset + idx + 1;
            let progress_callback = progress_callback.clone();

            let task = tokio::spawn(async move {
                let result = Self::process_file(
                    &file_path,
                    &chunker_config,
                    &vector_store,
                    &state,
                    current_idx,
                    total,
                    &progress_callback,
                )
                .await;

                drop(permit);
                result
            });

            tasks.push(task);
        }

        // Wait for all tasks
        for task in tasks {
            match task.await {
                Ok(Ok(file_stats)) => {
                    stats.files_processed += file_stats.files_processed;
                    stats.chunks_created += file_stats.chunks_created;
                    stats.chunks_embedded += file_stats.chunks_embedded;
                }
                Ok(Err(e)) => {
                    warn!("File processing failed: {}", e);
                    stats.files_failed += 1;
                }
                Err(e) => {
                    warn!("Task join error: {}", e);
                    stats.files_failed += 1;
                }
            }
        }

        Ok(stats)
    }

    /// Process a single file
    async fn process_file(
        file_path: &Path,
        chunker_config: &ChunkerConfig,
        vector_store: &Arc<Mutex<VectorStore>>,
        state: &Arc<Mutex<IndexState>>,
        current_idx: usize,
        total: usize,
        progress_callback: &Option<ProgressCallback>,
    ) -> Result<IndexStats> {
        let mut stats = IndexStats::default();

        debug!("Processing file: {:?}", file_path);

        Self::report_progress_static(
            progress_callback,
            IndexPhase::Chunking,
            current_idx,
            total,
            Some(file_path.to_string_lossy().to_string()),
        );

        // Chunk file
        let chunker = Chunker::new(chunker_config.clone());
        let chunker_chunks = match chunker.chunk_file(file_path) {
            Ok(chunks) => chunks,
            Err(e) => {
                warn!("Failed to chunk {:?}: {}", file_path, e);
                return Err(e.into());
            }
        };

        stats.chunks_created = chunker_chunks.len();

        if chunker_chunks.is_empty() {
            debug!("No chunks created for {:?}", file_path);
            return Ok(stats);
        }

        Self::report_progress_static(
            progress_callback,
            IndexPhase::Embedding,
            current_idx,
            total,
            Some(file_path.to_string_lossy().to_string()),
        );

        // Convert chunker chunks to vector store chunks
        let vector_chunks: Vec<codex_vector_store::CodeChunk> = chunker_chunks
            .into_iter()
            .map(|chunk| codex_vector_store::CodeChunk {
                path: chunk.file_path,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                content: chunk.content,
                metadata: codex_vector_store::ChunkMetadata {
                    language: chunk.metadata.language,
                    ..Default::default()
                },
            })
            .collect();

        stats.chunks_embedded = vector_chunks.len();

        Self::report_progress_static(
            progress_callback,
            IndexPhase::Storing,
            current_idx,
            total,
            Some(file_path.to_string_lossy().to_string()),
        );

        // Store in vector database (embeddings generated inside)
        let mut store = vector_store.lock().await;
        store.add_chunks(vector_chunks).await?;
        drop(store);

        // Update state
        let mut state_guard = state.lock().await;
        state_guard.update_file(file_path, stats.chunks_created)?;
        drop(state_guard);

        stats.files_processed = 1;

        Ok(stats)
    }

    /// Discover files to index
    fn discover_files(&self) -> Result<Vec<PathBuf>> {
        let mut builder = WalkBuilder::new(&self.config.root_dir);

        // Configure ignore patterns
        builder
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .require_git(false);

        // Add custom ignore patterns
        for pattern in &self.config.ignore_patterns {
            builder.add_custom_ignore_filename(pattern);
        }

        let mut files = Vec::new();

        for entry in builder.build() {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            // Check if file extension is supported
            if !Self::is_supported_file(path) {
                continue;
            }

            files.push(path.to_path_buf());
        }

        Ok(files)
    }

    /// Check if file type is supported
    fn is_supported_file(path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            if let Some(ext_str) = ext.to_str() {
                return matches!(
                    ext_str,
                    "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "c" | "cpp"
                        | "cc" | "h" | "hpp" | "cs" | "rb" | "sh" | "bash"
                );
            }
        }
        false
    }

    /// Report progress
    fn report_progress(
        &self,
        callback: &Option<ProgressCallback>,
        phase: IndexPhase,
        current: usize,
        total: usize,
        current_file: Option<String>,
    ) {
        Self::report_progress_static(callback, phase, current, total, current_file);
    }

    fn report_progress_static(
        callback: &Option<ProgressCallback>,
        phase: IndexPhase,
        current: usize,
        total: usize,
        current_file: Option<String>,
    ) {
        if let Some(cb) = callback {
            cb(IndexProgress {
                phase,
                current,
                total,
                current_file,
            });
        }
    }

    /// Remove file from index
    pub async fn remove_file(&self, file_path: &Path) -> Result<()> {
        info!("Removing file from index: {:?}", file_path);

        // Remove from vector store (not implemented in simple store yet)
        // TODO: Implement removal in vector store

        // Update state
        let mut state = self.state.lock().await;
        state.remove_file(file_path)?;
        state.save(&self.config.index_dir)?;

        Ok(())
    }

    /// Get index statistics
    pub async fn get_stats(&self) -> IndexStats {
        let state = self.state.lock().await;
        IndexStats {
            files_processed: state.files.len(),
            files_skipped: 0,
            files_failed: 0,
            chunks_created: state.total_chunks,
            chunks_embedded: state.total_chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    #[ignore] // Requires embedding model download
    async fn test_indexer_creation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config = IndexerConfig {
            root_dir: temp_dir.path().to_path_buf(),
            index_dir: temp_dir.path().join(".index"),
            ..Default::default()
        };

        let indexer = CodebaseIndexer::new(config).await;
        assert!(indexer.is_ok());
    }

    #[tokio::test]
    #[ignore] // Requires embedding model download
    async fn test_file_discovery() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create test files
        let rust_file = temp_dir.path().join("test.rs");
        fs::write(&rust_file, "fn main() {}").expect("Failed to write");

        let python_file = temp_dir.path().join("test.py");
        fs::write(&python_file, "def main(): pass").expect("Failed to write");

        let ignored_file = temp_dir.path().join("test.txt");
        fs::write(&ignored_file, "ignored").expect("Failed to write");

        let config = IndexerConfig {
            root_dir: temp_dir.path().to_path_buf(),
            index_dir: temp_dir.path().join(".index"),
            ..Default::default()
        };

        let indexer = CodebaseIndexer::new(config).await.expect("Failed to create indexer");
        let files = indexer.discover_files().expect("Failed to discover");

        assert_eq!(files.len(), 2);
        assert!(files.contains(&rust_file));
        assert!(files.contains(&python_file));
        assert!(!files.contains(&ignored_file));
    }

    #[tokio::test]
    async fn test_is_supported_file() {
        assert!(CodebaseIndexer::is_supported_file(Path::new("test.rs")));
        assert!(CodebaseIndexer::is_supported_file(Path::new("test.py")));
        assert!(CodebaseIndexer::is_supported_file(Path::new("test.ts")));
        assert!(!CodebaseIndexer::is_supported_file(Path::new("test.txt")));
        assert!(!CodebaseIndexer::is_supported_file(Path::new("README.md")));
    }
}
