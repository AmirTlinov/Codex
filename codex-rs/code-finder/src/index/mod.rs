mod builder;
mod cache;
mod classify;
mod filter;
mod git;
mod language;
mod model;
mod references;
mod search;
mod storage;

use crate::index::filter::PathFilter;
use crate::project::ProjectProfile;
use crate::proto::ErrorPayload;
use crate::proto::IndexState;
use crate::proto::IndexStatus;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use anyhow::Result;
use anyhow::anyhow;
use builder::IndexBuilder;
use cache::QueryCache;
use git::recent_paths;
use model::IndexSnapshot;
use model::SymbolRecord;
use notify::Config as NotifyConfig;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use search::run_search;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use storage::SnapshotLoad;
use storage::load_snapshot;
use storage::save_snapshot;
use time::OffsetDateTime;
use tokio::fs;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::error;
use tracing::warn;

#[derive(Clone)]
pub struct IndexCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    profile: ProjectProfile,
    snapshot: RwLock<IndexSnapshot>,
    status: RwLock<IndexStatusInternal>,
    cache: QueryCache,
    build_lock: Mutex<()>,
    filter: std::sync::Arc<PathFilter>,
}

#[derive(Clone, Debug)]
struct IndexStatusInternal {
    state: IndexState,
    symbols: usize,
    files: usize,
    updated_at: Option<OffsetDateTime>,
    notice: Option<String>,
}

const RESET_NOTICE: &str = "Index reset after detecting corruption; rebuilding from scratch";
const OPEN_CONTEXT_LINES: u32 = 40;
const OPEN_MAX_BYTES: usize = 16 * 1024;
const SNIPPET_MAX_BYTES: usize = 8 * 1024;

impl IndexCoordinator {
    pub async fn new(profile: ProjectProfile) -> Result<Self> {
        profile.ensure_dirs()?;
        let load_outcome = load_snapshot(&profile.index_path())?;
        let (loaded_snapshot, status) = match load_outcome {
            SnapshotLoad::Loaded(snapshot) => {
                let symbol_count = snapshot.symbols.len();
                let file_count = snapshot.files.len();
                let state = if symbol_count == 0 {
                    IndexState::Building
                } else {
                    IndexState::Ready
                };
                (
                    snapshot,
                    IndexStatusInternal {
                        state,
                        symbols: symbol_count,
                        files: file_count,
                        updated_at: None,
                        notice: None,
                    },
                )
            }
            SnapshotLoad::Missing => (
                IndexSnapshot::default(),
                IndexStatusInternal {
                    state: IndexState::Building,
                    symbols: 0,
                    files: 0,
                    updated_at: None,
                    notice: None,
                },
            ),
            SnapshotLoad::ResetAfterCorruption => (
                IndexSnapshot::default(),
                IndexStatusInternal {
                    state: IndexState::Building,
                    symbols: 0,
                    files: 0,
                    updated_at: None,
                    notice: Some(RESET_NOTICE.to_string()),
                },
            ),
        };
        let filter = Arc::new(PathFilter::new(profile.project_root())?);
        let inner = Arc::new(Inner {
            cache: QueryCache::new(profile.queries_dir()),
            profile,
            snapshot: RwLock::new(loaded_snapshot),
            status: RwLock::new(status),
            build_lock: Mutex::new(()),
            filter,
        });
        let coordinator = Self { inner };
        coordinator.spawn_initial_build();
        coordinator.spawn_watchers();
        Ok(coordinator)
    }

    pub async fn handle_search(&self, request: SearchRequest) -> Result<SearchResponse> {
        let status = self.current_status().await;
        if matches!(status.state, IndexState::Building) {
            let guard = self.inner.snapshot.read().await;
            if guard.symbols.is_empty() {
                return Ok(SearchResponse::indexing(status));
            }
        }
        let refs_limit = request.refs_limit.unwrap_or(12);
        let snapshot = self.inner.snapshot.read().await;
        let outcome = run_search(
            &snapshot,
            &request,
            &self.inner.cache,
            self.inner.profile.project_root(),
            refs_limit,
        )?;
        drop(snapshot);

        let query_id = outcome.cache_entry.as_ref().map(|(id, _)| *id);
        if let Some((id, payload)) = outcome.cache_entry {
            self.inner.cache.store(id, payload)?;
        }

        let mut error = None;
        if outcome.hits.is_empty() {
            if !request.filters.languages.is_empty() {
                let langs = request
                    .filters
                    .languages
                    .iter()
                    .map(|lang| format!("{lang:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                error = Some(ErrorPayload {
                    code: crate::proto::ErrorCode::NotFound,
                    message: format!("No symbols match languages: {langs}"),
                });
            } else if request.filters.recent_only {
                error = Some(ErrorPayload {
                    code: crate::proto::ErrorCode::NotFound,
                    message: "No recently modified symbols match this query".to_string(),
                });
            }
        }

        Ok(SearchResponse {
            query_id,
            hits: outcome.hits,
            index: self.current_status().await,
            stats: Some(outcome.stats),
            error,
        })
    }

    pub async fn handle_open(&self, request: OpenRequest) -> Result<OpenResponse> {
        let (symbol, contents) = self.symbol_with_contents(&request.id).await?;
        let (body, display_start, truncated) = slice_for_range(
            &contents,
            symbol.range.start,
            symbol.range.end,
            OPEN_CONTEXT_LINES,
            OPEN_CONTEXT_LINES,
            OPEN_MAX_BYTES,
        );
        Ok(OpenResponse {
            id: symbol.id.clone(),
            path: symbol.path.clone(),
            language: symbol.language.clone(),
            range: symbol.range.clone(),
            contents: body,
            display_start,
            truncated,
            index: self.current_status().await,
            error: None,
        })
    }

    pub async fn handle_snippet(&self, request: SnippetRequest) -> Result<SnippetResponse> {
        let (symbol, contents) = self.symbol_with_contents(&request.id).await?;
        let context_lines = request.context as u32;
        let (snippet, display_start, truncated) = slice_for_range(
            &contents,
            symbol.range.start,
            symbol.range.end,
            context_lines,
            context_lines,
            SNIPPET_MAX_BYTES,
        );
        Ok(SnippetResponse {
            id: symbol.id.clone(),
            path: symbol.path.clone(),
            language: symbol.language.clone(),
            range: symbol.range.clone(),
            snippet,
            display_start,
            truncated,
            index: self.current_status().await,
            error: None,
        })
    }

    pub async fn rebuild_index(&self) -> Result<IndexStatus> {
        self.rebuild_all().await?;
        Ok(self.current_status().await)
    }

    async fn symbol_with_contents(&self, id: &str) -> Result<(SymbolRecord, String)> {
        let snapshot = self.inner.snapshot.read().await;
        let symbol = snapshot
            .symbol(id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown symbol id {id}"))?;
        drop(snapshot);
        let path = self.inner.profile.project_root().join(&symbol.path);
        let contents = fs::read_to_string(path).await?;
        Ok((symbol, contents))
    }

    async fn rebuild_all(&self) -> Result<()> {
        let _guard = self.inner.build_lock.lock().await;
        self.update_status(IndexState::Building, None).await;
        let snapshot = match self.build_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                self.update_status(IndexState::Failed, None).await;
                return Err(err);
            }
        };

        if let Err(err) = save_snapshot(
            &self.inner.profile.index_path(),
            &self.inner.profile.temp_index_path(),
            &snapshot,
        ) {
            self.update_status(IndexState::Failed, None).await;
            return Err(err);
        }

        let counts = {
            let mut guard = self.inner.snapshot.write().await;
            *guard = snapshot;
            (guard.symbols.len(), guard.files.len())
        };
        self.update_status(IndexState::Ready, Some(counts)).await;
        self.set_notice(None).await;
        Ok(())
    }

    async fn build_snapshot(&self) -> Result<IndexSnapshot> {
        let root = self.inner.profile.project_root().to_path_buf();
        let recent = recent_paths(&root);
        let filter = self.inner.filter.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            IndexBuilder::new(root.as_path(), recent, filter).build()
        })
        .await??;
        Ok(snapshot)
    }

    pub async fn current_status(&self) -> IndexStatus {
        let guard = self.inner.status.read().await;
        IndexStatus {
            state: guard.state.clone(),
            symbols: guard.symbols,
            files: guard.files,
            updated_at: guard.updated_at,
            progress: None,
            schema_version: crate::proto::PROTOCOL_VERSION,
            notice: guard.notice.clone(),
        }
    }

    async fn update_status(&self, state: IndexState, counts: Option<(usize, usize)>) {
        let mut guard = self.inner.status.write().await;
        guard.state = state;
        if let Some((symbols, files)) = counts {
            guard.symbols = symbols;
            guard.files = files;
            guard.updated_at = Some(OffsetDateTime::now_utc());
        }
    }

    async fn set_notice(&self, notice: Option<String>) {
        let mut guard = self.inner.status.write().await;
        guard.notice = notice;
    }

    fn spawn_initial_build(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(err) = this.rebuild_all().await {
                error!("code-finder initial index failed: {err:?}");
            }
        });
    }

    fn spawn_watchers(&self) {
        let profile = self.inner.profile.clone();
        let this = self.clone();
        tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let filter = this.inner.filter.clone();
            std::thread::spawn(move || {
                if let Err(err) = watch_project(profile.project_root().to_path_buf(), filter, tx) {
                    error!("code-finder watcher error: {err:?}");
                }
            });
            while rx.recv().await.is_some() {
                let cloned = this.clone();
                tokio::spawn(async move {
                    sleep(Duration::from_millis(500)).await;
                    if let Err(err) = cloned.rebuild_all().await {
                        warn!("background reindex failed: {err:?}");
                    }
                });
            }
        });
    }
}

fn watch_project(
    root: PathBuf,
    filter: Arc<PathFilter>,
    tx: mpsc::UnboundedSender<()>,
) -> notify::Result<()> {
    let (watch_tx, watch_rx) = std_mpsc::channel();
    let mut watcher = RecommendedWatcher::new(watch_tx, NotifyConfig::default())?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    for res in watch_rx {
        match res {
            Ok(event) => {
                if event_only_ignored(&filter, &event) {
                    continue;
                }
                let _ = tx.send(());
            }
            Err(err) => warn!("code-finder watcher error: {err:?}"),
        }
    }
    Ok(())
}

fn event_only_ignored(filter: &PathFilter, event: &Event) -> bool {
    !event.paths.is_empty()
        && event
            .paths
            .iter()
            .all(|path| filter.is_ignored_path(path, None))
}

fn slice_for_range(
    contents: &str,
    start: u32,
    end: u32,
    context_before: u32,
    context_after: u32,
    max_bytes: usize,
) -> (String, u32, bool) {
    let lines: Vec<&str> = contents.lines().collect();
    if lines.is_empty() {
        return (String::new(), 1, false);
    }
    let total_lines = lines.len() as u32;
    let normalized_start = start.clamp(1, total_lines);
    let normalized_end = end.max(normalized_start).min(total_lines);
    let slice_start = normalized_start.saturating_sub(context_before).max(1);
    let slice_end = (normalized_end + context_after).min(total_lines);
    let segment = &lines[(slice_start - 1) as usize..slice_end as usize];
    let (body, truncated) = collect_lines(segment, max_bytes);
    (body, slice_start, truncated)
}

fn collect_lines(lines: &[&str], max_bytes: usize) -> (String, bool) {
    let mut buf = String::new();
    let mut truncated = false;
    for (idx, line) in lines.iter().enumerate() {
        let separator = if idx == 0 { 0 } else { 1 };
        if buf.len() + separator + line.len() > max_bytes {
            truncated = true;
            break;
        }
        if idx > 0 {
            buf.push('\n');
        }
        buf.push_str(line);
    }
    (buf, truncated)
}
