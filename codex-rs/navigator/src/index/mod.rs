mod builder;
mod cache;
mod classify;
mod coverage;
mod filter;
mod git;
mod language;
pub(crate) mod model;
mod references;
mod search;
mod storage;

use crate::atlas::build_search_hint;
use crate::atlas::rebuild_atlas;
use crate::index::builder::BuildArtifacts;
use crate::index::builder::FileOutcome;
use crate::index::builder::IndexBuilder;
use crate::index::builder::IndexedFile;
use crate::index::builder::MAX_FILE_BYTES;
use crate::index::builder::SkipReason;
use crate::index::builder::SkippedFile;
use crate::index::builder::relative_path;
use crate::index::coverage::CoverageTracker;
use crate::index::filter::PathFilter;
use crate::project::ProjectProfile;
use crate::proto::AtlasSnapshot;
use crate::proto::CoverageGap;
use crate::proto::CoverageReason;
use crate::proto::ErrorPayload;
use crate::proto::FallbackHit;
use crate::proto::IndexState;
use crate::proto::IndexStatus;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::Range;
use crate::proto::SearchDiagnostics;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use crate::proto::SymbolKind;
use anyhow::Result;
use anyhow::anyhow;
use cache::QueryCache;
use git::recent_paths;
use model::FileEntry;
use model::IndexSnapshot;
use model::SymbolRecord;
use notify::Config as NotifyConfig;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use search::literal_fallback_allowed;
use search::literal_match_from_contents;
use search::run_search;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use storage::SnapshotLoad;
use storage::load_snapshot;
use storage::save_snapshot;
use time::OffsetDateTime;
use tokio::fs;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::time::Sleep;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
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
    auto_indexing: Arc<AtomicBool>,
    coverage: Arc<CoverageTracker>,
    shutdown: CancellationToken,
}

#[derive(Clone, Debug)]
struct IndexStatusInternal {
    state: IndexState,
    symbols: usize,
    files: usize,
    updated_at: Option<OffsetDateTime>,
    notice: Option<String>,
    auto_indexing: bool,
}

const RESET_NOTICE: &str = "Index reset after detecting corruption; rebuilding from scratch";
const OPEN_CONTEXT_LINES: u32 = 40;
const OPEN_MAX_BYTES: usize = 16 * 1024;
const SNIPPET_MAX_BYTES: usize = 8 * 1024;
const COVERAGE_LIMIT: usize = 32;
const FALLBACK_MAX_FILE_BYTES: usize = 512 * 1024;
const LITERAL_PENDING_SAMPLE: usize = 8;

impl IndexCoordinator {
    pub fn cancel_background(&self) {
        self.inner.shutdown.cancel();
    }

    pub async fn set_auto_indexing(&self, enabled: bool) {
        self.inner.auto_indexing.store(enabled, Ordering::Relaxed);
        {
            let mut guard = self.inner.status.write().await;
            guard.auto_indexing = enabled;
        }
        if enabled {
            self.set_notice(None).await;
            if let Err(err) = self.rebuild_all().await {
                warn!("navigator rebuild after enabling auto indexing failed: {err:?}");
            }
        } else {
            self.set_notice(Some(
                "Auto indexing disabled. Use /index-code or /indexing to rebuild manually."
                    .to_string(),
            ))
            .await;
        }
    }

    pub fn project_root(&self) -> &Path {
        self.inner.profile.project_root()
    }

    pub async fn new(profile: ProjectProfile, auto_indexing: bool) -> Result<Self> {
        profile.ensure_dirs()?;
        let load_outcome = load_snapshot(&profile.index_path())?;
        let (loaded_snapshot, status) = match load_outcome {
            SnapshotLoad::Loaded(snapshot) => {
                let snapshot = *snapshot;
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
                        auto_indexing,
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
                    auto_indexing,
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
                    auto_indexing,
                },
            ),
        };
        let filter = Arc::new(PathFilter::new(profile.project_root())?);
        let auto_indexing_flag = Arc::new(AtomicBool::new(auto_indexing));
        let shutdown = CancellationToken::new();
        let inner = Arc::new(Inner {
            cache: QueryCache::new(profile.queries_dir()),
            profile,
            snapshot: RwLock::new(loaded_snapshot),
            status: RwLock::new(status),
            build_lock: Mutex::new(()),
            filter,
            auto_indexing: auto_indexing_flag,
            coverage: Arc::new(CoverageTracker::new(Some(COVERAGE_LIMIT))),
            shutdown,
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
                let mut response = SearchResponse::indexing(status);
                response.diagnostics = Some(self.diagnostics().await);
                return Ok(response);
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
        let atlas_hint = build_search_hint(&snapshot, &outcome.hits);
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

        let fallback_hits = if literal_fallback_allowed(&request)
            && let Some(query) = request
                .query
                .as_ref()
                .map(|q| q.trim())
                .filter(|q| !q.is_empty())
        {
            let remaining = request.limit.saturating_sub(outcome.hits.len());
            if remaining > 0 {
                self.collect_fallback_hits(query, remaining).await
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let diagnostics = self.diagnostics().await;
        let mut stats = outcome.stats;
        if !diagnostics.coverage.pending.is_empty() {
            let pending_paths: Vec<String> = diagnostics
                .coverage
                .pending
                .iter()
                .map(|gap| gap.path.clone())
                .take(LITERAL_PENDING_SAMPLE)
                .collect();
            if !pending_paths.is_empty() {
                stats.literal_pending_paths = Some(pending_paths);
            }
        }
        Ok(SearchResponse {
            query_id,
            hits: outcome.hits,
            index: self.current_status().await,
            stats: Some(stats),
            hints: outcome.hints,
            error,
            diagnostics: Some(diagnostics),
            fallback_hits,
            atlas_hint,
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
            diagnostics: Some(self.diagnostics().await),
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
            diagnostics: Some(self.diagnostics().await),
        })
    }

    pub async fn diagnostics(&self) -> SearchDiagnostics {
        let status = self.current_status().await;
        let freshness_secs = status.updated_at.map(|ts| {
            let diff = OffsetDateTime::now_utc() - ts;
            diff.whole_seconds().max(0) as u64
        });
        let coverage = self.inner.coverage.diagnostics().await;
        let pending_literals = coverage
            .pending
            .iter()
            .map(|gap| gap.path.clone())
            .take(8)
            .collect();
        SearchDiagnostics {
            index_state: status.state.clone(),
            freshness_secs,
            coverage,
            pending_literals,
        }
    }

    pub async fn rebuild_index(&self) -> Result<IndexStatus> {
        self.rebuild_all().await?;
        Ok(self.current_status().await)
    }

    async fn symbol_with_contents(&self, id: &str) -> Result<(SymbolRecord, String)> {
        if let Some(literal) = parse_literal_symbol_id(id) {
            return self.literal_symbol_with_contents(literal).await;
        }

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

    async fn literal_symbol_with_contents(
        &self,
        literal: LiteralSymbolId,
    ) -> Result<(SymbolRecord, String)> {
        let snapshot = self.inner.snapshot.read().await;
        let file_entry = snapshot
            .files
            .get(&literal.path)
            .cloned()
            .ok_or_else(|| anyhow!("unknown literal path {}", literal.path))?;
        drop(snapshot);
        let abs = self.inner.profile.project_root().join(&literal.path);
        let contents = fs::read_to_string(abs).await?;
        let symbol = build_literal_symbol(&literal, &file_entry);
        Ok((symbol, contents))
    }

    async fn rebuild_all(&self) -> Result<()> {
        let _guard = self.inner.build_lock.lock().await;
        self.update_status(IndexState::Building, None).await;
        let artifacts = match self.build_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                self.update_status(IndexState::Failed, None).await;
                return Err(err);
            }
        };

        if let Err(err) = save_snapshot(
            &self.inner.profile.index_path(),
            &self.inner.profile.temp_index_path(),
            &artifacts.snapshot,
        ) {
            self.update_status(IndexState::Failed, None).await;
            return Err(err);
        }

        let counts = {
            let mut guard = self.inner.snapshot.write().await;
            *guard = artifacts.snapshot;
            (guard.symbols.len(), guard.files.len())
        };
        let skipped = skipped_to_gaps(artifacts.skipped);
        self.inner.coverage.replace_skipped(skipped).await;
        self.inner.coverage.clear_pending().await;
        self.update_status(IndexState::Ready, Some(counts)).await;
        self.set_notice(None).await;
        Ok(())
    }

    async fn ingest_delta(&self, candidates: Vec<String>) -> Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        let mut dedup = HashSet::new();
        let mut pending = Vec::new();
        for path in candidates {
            if dedup.insert(path.clone()) {
                pending.push(path);
            }
        }
        if pending.is_empty() {
            return Ok(());
        }

        let root = self.inner.profile.project_root().to_path_buf();
        let recent = recent_paths(&root);
        let filter = self.inner.filter.clone();
        let builder = IndexBuilder::new(root.as_path(), recent, filter.clone());
        let _guard = self.inner.build_lock.lock().await;
        let mut snapshot = self.inner.snapshot.write().await;
        let mut changed = false;
        for rel in pending {
            if filter.is_ignored_rel(&rel) {
                self.inner
                    .coverage
                    .record_skipped(rel.clone(), CoverageReason::Ignored)
                    .await;
                drop_file(&mut snapshot, &rel);
                continue;
            }
            match builder.index_path(&rel) {
                Ok(FileOutcome::Indexed(indexed)) => {
                    apply_indexed_file(&mut snapshot, indexed);
                    self.inner.coverage.record_indexed(&rel).await;
                    changed = true;
                }
                Ok(FileOutcome::IndexedTextOnly {
                    file: indexed,
                    reason,
                }) => {
                    apply_indexed_file(&mut snapshot, indexed);
                    self.inner
                        .coverage
                        .record_skipped(rel.clone(), coverage_reason_from_skip(reason))
                        .await;
                    changed = true;
                }
                Ok(FileOutcome::Skipped(reason)) => {
                    drop_file(&mut snapshot, &rel);
                    self.inner
                        .coverage
                        .record_skipped(rel.clone(), coverage_reason_from_skip(reason))
                        .await;
                }
                Err(err) => {
                    self.inner
                        .coverage
                        .record_error(
                            rel.clone(),
                            CoverageReason::ReadError {
                                message: err.to_string(),
                            },
                        )
                        .await;
                }
            }
        }
        if changed {
            rebuild_atlas(&mut snapshot, self.project_root());
        }
        drop(snapshot);
        if changed {
            let counts = self.snapshot_counts().await;
            self.update_status(IndexState::Ready, Some(counts)).await;
        }
        Ok(())
    }

    async fn collect_fallback_hits(&self, query: &str, max_hits: usize) -> Vec<FallbackHit> {
        collect_fallback_hits_impl(self, query, max_hits).await
    }

    async fn build_snapshot(&self) -> Result<BuildArtifacts> {
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
        let (state, symbols, files, updated_at, notice, auto_indexing) = {
            let guard = self.inner.status.read().await;
            (
                guard.state.clone(),
                guard.symbols,
                guard.files,
                guard.updated_at,
                guard.notice.clone(),
                guard.auto_indexing,
            )
        };
        let coverage = self.inner.coverage.diagnostics().await;
        IndexStatus {
            state,
            symbols,
            files,
            updated_at,
            progress: None,
            schema_version: crate::proto::PROTOCOL_VERSION,
            notice,
            auto_indexing,
            coverage: Some(coverage),
        }
    }

    pub async fn atlas_snapshot(&self) -> AtlasSnapshot {
        let snapshot = self.inner.snapshot.read().await;
        snapshot.atlas.clone()
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

    async fn snapshot_counts(&self) -> (usize, usize) {
        let guard = self.inner.snapshot.read().await;
        (guard.symbols.len(), guard.files.len())
    }

    fn spawn_initial_build(&self) {
        let this = self.clone();
        let shutdown = self.inner.shutdown.clone();
        tokio::spawn(async move {
            if shutdown.is_cancelled() {
                return;
            }
            if let Err(err) = this.rebuild_all().await
                && !shutdown.is_cancelled()
            {
                error!("navigator initial index failed: {err:?}");
            }
        });
    }

    fn spawn_watchers(&self) {
        let profile = self.inner.profile.clone();
        let this = self.clone();
        let shutdown = self.inner.shutdown.clone();
        tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let filter = this.inner.filter.clone();
            let thread_shutdown = shutdown.clone();
            std::thread::spawn(move || {
                if let Err(err) = watch_project(
                    profile.project_root().to_path_buf(),
                    filter,
                    tx,
                    thread_shutdown.clone(),
                ) && !thread_shutdown.is_cancelled()
                {
                    error!("navigator watcher error: {err:?}");
                }
            });
            let mut pending_paths: HashSet<String> = HashSet::new();
            let mut flush_timer: Option<Pin<Box<Sleep>>> = None;
            let mut force_full = false;
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    maybe = rx.recv() => {
                        let Some(event) = maybe else { break; };
                        match event {
                            WatchEvent::Paths(paths) => {
                                for path in paths {
                                    pending_paths.insert(path.clone());
                                    this.inner.coverage.record_pending(path).await;
                                }
                            }
                            WatchEvent::Rescan => {
                                force_full = true;
                                pending_paths.clear();
                            }
                        }
                        if flush_timer.is_none() {
                            flush_timer = Some(Box::pin(sleep(Duration::from_millis(250))));
                        }
                    }
                    _ = async {
                        if let Some(timer) = &mut flush_timer {
                            timer.await;
                        }
                    }, if flush_timer.is_some() => {
                        flush_timer = None;
                        if shutdown.is_cancelled() {
                            pending_paths.clear();
                            force_full = false;
                            continue;
                        }
                        if !this.inner.auto_indexing.load(Ordering::Relaxed) {
                            pending_paths.clear();
                            force_full = false;
                            continue;
                        }
                        if force_full {
                            if let Err(err) = this.rebuild_all().await
                                && !shutdown.is_cancelled()
                            {
                                warn!("background reindex failed: {err:?}");
                            }
                            force_full = false;
                            continue;
                        }
                        if pending_paths.is_empty() {
                            continue;
                        }
                        let batch: Vec<String> = pending_paths.drain().collect();
                        if let Err(err) = this.ingest_delta(batch).await
                            && !shutdown.is_cancelled()
                        {
                            warn!("incremental ingest failed: {err:?}");
                        }
                    }
                }
            }
        });
    }
}

enum WatchEvent {
    Paths(Vec<String>),
    Rescan,
}

fn watch_project(
    root: PathBuf,
    filter: Arc<PathFilter>,
    tx: mpsc::UnboundedSender<WatchEvent>,
    shutdown: CancellationToken,
) -> notify::Result<()> {
    let (watch_tx, watch_rx) = std_mpsc::channel();
    let mut watcher = RecommendedWatcher::new(watch_tx, NotifyConfig::default())?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        match watch_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(event)) => {
                if event_only_ignored(&filter, &event) {
                    continue;
                }
                let rels = event
                    .paths
                    .iter()
                    .filter_map(|path| relative_path(&root, path))
                    .collect::<Vec<_>>();
                let message = if rels.is_empty() {
                    WatchEvent::Rescan
                } else {
                    WatchEvent::Paths(rels)
                };
                let _ = tx.send(message);
            }
            Ok(Err(err)) => warn!("navigator watcher error: {err:?}"),
            Err(RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiteralSymbolId {
    path: String,
    line: u32,
}

fn parse_literal_symbol_id(id: &str) -> Option<LiteralSymbolId> {
    let rest = id.strip_prefix("literal::")?;
    let (path, line_str) = rest.rsplit_once('#')?;
    if path.is_empty() {
        return None;
    }
    let line: u32 = line_str.parse().ok()?;
    if line == 0 {
        return None;
    }
    Some(LiteralSymbolId {
        path: path.to_string(),
        line,
    })
}

fn build_literal_symbol(literal: &LiteralSymbolId, file_entry: &FileEntry) -> SymbolRecord {
    let id = format!("literal::{}#{}", literal.path, literal.line);
    SymbolRecord {
        id: id.clone(),
        identifier: id,
        kind: SymbolKind::Document,
        language: file_entry.language.clone(),
        path: literal.path.clone(),
        range: Range {
            start: literal.line,
            end: literal.line,
        },
        module: None,
        layer: None,
        categories: file_entry.categories.clone(),
        recent: file_entry.recent,
        preview: String::new(),
        doc_summary: None,
        dependencies: Vec::new(),
    }
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

async fn collect_fallback_hits_impl(
    coordinator: &IndexCoordinator,
    query: &str,
    max_hits: usize,
) -> Vec<FallbackHit> {
    if max_hits == 0 {
        return Vec::new();
    }
    let diagnostics = coordinator.inner.coverage.diagnostics().await;
    if diagnostics.pending.is_empty() {
        return Vec::new();
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let root = coordinator.inner.profile.project_root().to_path_buf();
    let needle = trimmed.to_ascii_lowercase();
    let mut hits = Vec::new();
    for gap in diagnostics.pending {
        if hits.len() >= max_hits {
            break;
        }
        let path = root.join(&gap.path);
        let Ok(contents) = fs::read_to_string(&path).await else {
            continue;
        };
        if contents.len() > FALLBACK_MAX_FILE_BYTES {
            continue;
        }
        if let Some(hit) =
            fallback_hit_from_contents(gap.path.clone(), &contents, trimmed, gap.reason.clone())
        {
            hits.push(hit);
            continue;
        }
        if let Some((line, preview)) = find_fallback_line(&contents, &needle) {
            hits.push(FallbackHit {
                path: gap.path,
                line,
                preview,
                reason: gap.reason,
                context_snippet: None,
            });
        }
    }
    hits
}

fn fallback_hit_from_contents(
    path: String,
    contents: &str,
    query: &str,
    reason: CoverageReason,
) -> Option<FallbackHit> {
    literal_match_from_contents(contents, query).map(|literal| FallbackHit {
        path,
        line: literal.line,
        preview: literal.preview,
        reason,
        context_snippet: Some(literal.snippet),
    })
}

fn find_fallback_line(contents: &str, needle: &str) -> Option<(u32, String)> {
    for (idx, line) in contents.lines().enumerate() {
        if line.to_ascii_lowercase().contains(needle) {
            let mut preview = line.trim().to_string();
            if preview.len() > 160 {
                preview.truncate(160);
            }
            return Some(((idx + 1) as u32, preview));
        }
    }
    None
}

fn apply_indexed_file(snapshot: &mut IndexSnapshot, indexed: IndexedFile) {
    let path = indexed.file.path.clone();
    drop_file(snapshot, &path);
    for symbol in indexed.symbols {
        snapshot.symbols.insert(symbol.id.clone(), symbol);
    }
    for token in indexed.file.tokens.iter() {
        snapshot
            .token_to_files
            .entry(token.clone())
            .or_default()
            .insert(path.clone());
    }
    for trigram in indexed.file.trigrams.iter() {
        snapshot
            .trigram_to_files
            .entry(*trigram)
            .or_default()
            .insert(path.clone());
    }
    snapshot.text.insert(path.clone(), indexed.text);
    snapshot.files.insert(path, indexed.file);
}

fn drop_file(snapshot: &mut IndexSnapshot, path: &str) {
    if let Some(entry) = snapshot.files.remove(path) {
        for symbol_id in entry.symbol_ids {
            snapshot.symbols.remove(&symbol_id);
        }
        for token in entry.tokens {
            if let Some(files) = snapshot.token_to_files.get_mut(&token) {
                files.remove(path);
                if files.is_empty() {
                    snapshot.token_to_files.remove(&token);
                }
            }
        }
        for trigram in entry.trigrams {
            if let Some(files) = snapshot.trigram_to_files.get_mut(&trigram) {
                files.remove(path);
                if files.is_empty() {
                    snapshot.trigram_to_files.remove(&trigram);
                }
            }
        }
    }
    snapshot.text.remove(path);
}

fn coverage_reason_from_skip(reason: SkipReason) -> CoverageReason {
    match reason {
        SkipReason::Oversize { bytes } => CoverageReason::Oversize {
            bytes,
            limit: MAX_FILE_BYTES as u64,
        },
        SkipReason::NonUtf8 => CoverageReason::NonUtf8,
        SkipReason::NoSymbols => CoverageReason::NoSymbols,
        SkipReason::ReadError(message) => CoverageReason::ReadError { message },
    }
}

fn skipped_to_gaps(skipped: Vec<SkippedFile>) -> Vec<CoverageGap> {
    skipped
        .into_iter()
        .map(|file| CoverageGap {
            path: file.path,
            reason: coverage_reason_from_skip(file.reason),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::FileFingerprint;
    use crate::proto::CoverageReason;
    use crate::proto::FileCategory;
    use crate::proto::Language;

    #[test]
    fn parse_literal_symbol_id_valid_input() {
        let literal = parse_literal_symbol_id("literal::src/lib.rs#42").expect("literal id");
        assert_eq!(literal.path, "src/lib.rs");
        assert_eq!(literal.line, 42);
    }

    #[test]
    fn parse_literal_symbol_id_rejects_invalid_forms() {
        assert!(parse_literal_symbol_id("literal::src/lib.rs").is_none());
        assert!(parse_literal_symbol_id("literal::#10").is_none());
        assert!(parse_literal_symbol_id("literal::foo#0").is_none());
        assert!(parse_literal_symbol_id("nav_123").is_none());
    }

    #[test]
    fn build_literal_symbol_preserves_file_metadata() {
        let literal = LiteralSymbolId {
            path: "src/lib.rs".to_string(),
            line: 7,
        };
        let entry = FileEntry {
            path: literal.path.clone(),
            language: Language::Rust,
            categories: vec![FileCategory::Source],
            recent: true,
            symbol_ids: Vec::new(),
            tokens: Vec::new(),
            trigrams: Vec::new(),
            line_count: 0,
            fingerprint: FileFingerprint {
                mtime: Some(0),
                size: 0,
                digest: [0; 16],
            },
        };
        let symbol = build_literal_symbol(&literal, &entry);
        assert_eq!(symbol.id, "literal::src/lib.rs#7");
        assert_eq!(symbol.language, Language::Rust);
        assert_eq!(symbol.range.start, 7);
        assert_eq!(symbol.range.end, 7);
        assert!(symbol.categories.contains(&FileCategory::Source));
        assert!(symbol.preview.is_empty());
    }

    #[test]
    fn fallback_hit_includes_snippet() {
        let reason = CoverageReason::PendingIngest;
        let contents = "line one\nneedle present here\nline three";
        let hit = fallback_hit_from_contents(
            "src/sample.txt".to_string(),
            contents,
            "needle present",
            reason.clone(),
        )
        .expect("fallback hit");
        assert_eq!(hit.path, "src/sample.txt");
        assert_eq!(hit.line, 2);
        assert!(hit.preview.contains("needle"));
        assert!(hit.context_snippet.is_some(), "snippet missing");
        assert_eq!(hit.reason, reason);
    }
}
