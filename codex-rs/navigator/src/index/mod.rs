mod builder;
mod cache;
mod classify;
mod codeowners;
mod coverage;
mod filter;
mod git;
mod guardrail;
mod health;
mod language;
pub(crate) mod model;
mod personal;
mod references;
mod search;
mod storage;
mod text;

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
use crate::index::codeowners::OwnerResolver;
use crate::index::coverage::CoverageTracker;
use crate::index::filter::PathFilter;
use crate::index::git::churn_scores;
use crate::index::git::recency_days;
use crate::index::git::recent_paths;
use crate::index::guardrail::GuardrailEmitter;
use crate::index::health::HealthStore;
use crate::index::model::FileEntry;
use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::index::text::TextIngestor;
use crate::project::ProjectProfile;
use crate::proto::ActiveFilters;
use crate::proto::AtlasSnapshot;
use crate::proto::ContextBanner;
use crate::proto::ContextBucket;
use crate::proto::CoverageDiagnostics;
use crate::proto::CoverageGap;
use crate::proto::CoverageReason;
use crate::proto::ErrorPayload;
use crate::proto::FallbackHit;
use crate::proto::FileCategory;
use crate::proto::FilterOp;
use crate::proto::HealthPanel;
use crate::proto::HealthSummary;
use crate::proto::IndexState;
use crate::proto::IndexStatus;
use crate::proto::IngestKind;
use crate::proto::NavHit;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::QueryId;
use crate::proto::Range;
use crate::proto::SearchDiagnostics;
use crate::proto::SearchFilters;
use crate::proto::SearchProfileSample;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;
use crate::proto::SearchStats;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use crate::proto::SymbolKind;
use anyhow::Result;
use anyhow::anyhow;
use cache::QueryCache;
use notify::Config as NotifyConfig;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use search::literal_fallback_allowed;
use search::literal_match_from_contents;
use search::run_search;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use std::time::Instant;
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
use tracing::info;
use tracing::warn;

#[derive(Clone)]
pub struct IndexCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    profile: ProjectProfile,
    snapshot: Arc<RwLock<IndexSnapshot>>,
    status: RwLock<IndexStatusInternal>,
    cache: QueryCache,
    build_lock: Mutex<()>,
    filter: std::sync::Arc<PathFilter>,
    auto_indexing: Arc<AtomicBool>,
    coverage: Arc<CoverageTracker>,
    health: Arc<HealthStore>,
    guardrails: Arc<GuardrailEmitter>,
    text_ingest: TextIngestor,
    self_heal: SelfHealPolicy,
    self_heal_state: Mutex<Option<Instant>>,
    profile_history: Mutex<VecDeque<SearchProfileSample>>,
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
const SELF_HEAL_PENDING_LIMIT: usize = 96;
const SELF_HEAL_ERROR_LIMIT: usize = 4;
const SELF_HEAL_COOLDOWN_SECS: u64 = 900;
const PROFILE_HISTORY_LIMIT: usize = 64;

#[derive(Clone)]
struct SelfHealPolicy {
    enabled: bool,
    pending_limit: usize,
    error_limit: usize,
    cooldown: Duration,
}

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
        let health = Arc::new(HealthStore::new(&profile)?);
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
        let guardrail_webhook = std::env::var("NAVIGATOR_GUARDRAIL_WEBHOOK")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let guardrail_latency = std::env::var("NAVIGATOR_GUARDRAIL_LATENCY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1_500);
        let guardrail_cooldown = std::env::var("NAVIGATOR_GUARDRAIL_COOLDOWN_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));
        let guardrails = Arc::new(GuardrailEmitter::new(
            profile.project_root(),
            guardrail_webhook,
            guardrail_latency,
            guardrail_cooldown,
        ));
        let self_heal_enabled = std::env::var("NAVIGATOR_SELF_HEAL_ENABLED")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .and_then(|value| match value.as_str() {
                "0" | "false" | "off" => Some(false),
                "1" | "true" | "on" => Some(true),
                _ => None,
            })
            .unwrap_or(true);
        let self_heal_pending_limit = std::env::var("NAVIGATOR_SELF_HEAL_PENDING_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(SELF_HEAL_PENDING_LIMIT);
        let self_heal_error_limit = std::env::var("NAVIGATOR_SELF_HEAL_ERROR_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(SELF_HEAL_ERROR_LIMIT);
        let self_heal_cooldown = std::env::var("NAVIGATOR_SELF_HEAL_COOLDOWN_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(SELF_HEAL_COOLDOWN_SECS));
        let self_heal = SelfHealPolicy {
            enabled: self_heal_enabled,
            pending_limit: self_heal_pending_limit,
            error_limit: self_heal_error_limit,
            cooldown: self_heal_cooldown,
        };
        let auto_indexing_flag = Arc::new(AtomicBool::new(auto_indexing));
        let shutdown = CancellationToken::new();
        let snapshot_lock = Arc::new(RwLock::new(loaded_snapshot));
        let text_ingest = TextIngestor::new(snapshot_lock.clone(), shutdown.clone());
        let inner = Arc::new(Inner {
            cache: QueryCache::new(profile.queries_dir()),
            profile,
            snapshot: snapshot_lock,
            status: RwLock::new(status),
            build_lock: Mutex::new(()),
            filter,
            auto_indexing: auto_indexing_flag,
            coverage: Arc::new(CoverageTracker::new(Some(COVERAGE_LIMIT))),
            health,
            guardrails,
            text_ingest,
            self_heal,
            self_heal_state: Mutex::new(None),
            profile_history: Mutex::new(VecDeque::with_capacity(PROFILE_HISTORY_LIMIT)),
            shutdown,
        });
        let coordinator = Self { inner };
        coordinator.spawn_initial_build();
        coordinator.spawn_watchers();
        Ok(coordinator)
    }

    pub async fn handle_search(&self, mut request: SearchRequest) -> Result<SearchResponse> {
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
        if request.inherit_filters {
            rewrite_inherited_filters(&self.inner.cache, &mut request)?;
        }
        let snapshot = self.inner.snapshot.read().await;
        let outcome = run_search(
            &snapshot,
            &request,
            &self.inner.cache,
            self.inner.profile.project_root(),
            refs_limit,
        )?;
        let atlas_hint = build_search_hint(&snapshot, &outcome.hits);
        let active_filters = summarize_active_filters(&request.filters);
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
        if let Err(err) = self.inner.health.record_search(&stats).await {
            warn!("navigator health search metrics failed: {err:?}");
        }
        let guardrails = self.inner.guardrails.clone();
        let stats_clone = stats.clone();
        tokio::spawn(async move {
            guardrails.observe_search_stats(&stats_clone).await;
        });
        self.record_profile_sample(&request, &stats, query_id).await;
        let context_banner = build_context_banner(&outcome.hits);
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
            active_filters,
            context_banner,
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
        let health = self.inner.health.summary(&coverage).await;
        self.maybe_trigger_self_heal(&status, &coverage).await;
        let guardrails = self.inner.guardrails.clone();
        let health_clone = health.clone();
        let coverage_clone = coverage.clone();
        tokio::spawn(async move {
            guardrails
                .observe_health(&health_clone, &coverage_clone)
                .await;
        });
        SearchDiagnostics {
            index_state: status.state.clone(),
            freshness_secs,
            coverage,
            pending_literals,
            health: Some(health),
        }
    }

    async fn maybe_trigger_self_heal(&self, status: &IndexStatus, coverage: &CoverageDiagnostics) {
        if !self.inner.self_heal.enabled {
            return;
        }
        let unhealthy = matches!(status.state, IndexState::Failed)
            || coverage.errors.len() > self.inner.self_heal.error_limit
            || coverage.pending.len() > self.inner.self_heal.pending_limit;
        if !unhealthy {
            return;
        }
        let mut guard = self.inner.self_heal_state.lock().await;
        if guard
            .map(|instant| instant.elapsed() < self.inner.self_heal.cooldown)
            .unwrap_or(false)
        {
            return;
        }
        *guard = Some(Instant::now());
        drop(guard);
        let this = self.clone();
        tokio::spawn(async move {
            info!("navigator self-heal triggered");
            if let Err(err) = this.rebuild_all().await {
                warn!("navigator self-heal rebuild failed: {err:?}");
            } else {
                info!("navigator self-heal rebuild completed");
            }
        });
    }

    pub async fn health_panel(&self) -> HealthPanel {
        let coverage = self.inner.coverage.diagnostics().await;
        self.inner.health.panel(&coverage).await
    }

    pub async fn health_summary(&self) -> HealthSummary {
        let coverage = self.inner.coverage.diagnostics().await;
        self.inner.health.summary(&coverage).await
    }

    pub async fn profile_snapshot(&self, limit: Option<usize>) -> Vec<SearchProfileSample> {
        let cap = limit.unwrap_or(10).clamp(1, PROFILE_HISTORY_LIMIT);
        let guard = self.inner.profile_history.lock().await;
        guard.iter().rev().take(cap).cloned().collect()
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
        let ingest_start = Instant::now();
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
        self.inner.coverage.replace_skipped(skipped.clone()).await;
        self.inner.coverage.clear_pending().await;
        self.update_status(IndexState::Ready, Some(counts)).await;
        self.set_notice(None).await;
        if let Err(err) = self
            .inner
            .health
            .record_ingest(IngestKind::Full, ingest_start.elapsed(), counts.1, &skipped)
            .await
        {
            warn!("navigator health ingest metrics failed: {err:?}");
        }
        Ok(())
    }

    async fn ingest_delta(&self, candidates: Vec<String>) -> Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        let ingest_start = Instant::now();
        let mut indexed_files = 0usize;
        let mut skipped_gaps: Vec<CoverageGap> = Vec::new();
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
        let churn = churn_scores(&root);
        let freshness = recency_days(&root);
        let owners = OwnerResolver::load(&root);
        let filter = self.inner.filter.clone();
        let text_sender = self.inner.text_ingest.sender();
        let builder = IndexBuilder::new(
            root.as_path(),
            recent,
            churn,
            freshness,
            owners,
            filter.clone(),
            Some(text_sender),
        );
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
                Ok(FileOutcome::Indexed(indexed_file)) => {
                    indexed_files += 1;
                    apply_indexed_file(&mut snapshot, indexed_file);
                    self.inner.coverage.record_indexed(&rel).await;
                    changed = true;
                }
                Ok(FileOutcome::IndexedTextOnly {
                    file: indexed_file,
                    reason,
                }) => {
                    indexed_files += 1;
                    apply_indexed_file(&mut snapshot, indexed_file);
                    self.inner
                        .coverage
                        .record_skipped(rel.clone(), coverage_reason_from_skip(reason.clone()))
                        .await;
                    skipped_gaps.push(CoverageGap {
                        path: rel.clone(),
                        reason: coverage_reason_from_skip(reason),
                    });
                    changed = true;
                }
                Ok(FileOutcome::Skipped(reason)) => {
                    drop_file(&mut snapshot, &rel);
                    self.inner
                        .coverage
                        .record_skipped(rel.clone(), coverage_reason_from_skip(reason.clone()))
                        .await;
                    skipped_gaps.push(CoverageGap {
                        path: rel.clone(),
                        reason: coverage_reason_from_skip(reason),
                    });
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
        if let Err(err) = self
            .inner
            .health
            .record_ingest(
                IngestKind::Delta,
                ingest_start.elapsed(),
                indexed_files,
                &skipped_gaps,
            )
            .await
        {
            warn!("navigator health delta metrics failed: {err:?}");
        }
        Ok(())
    }

    async fn collect_fallback_hits(&self, query: &str, max_hits: usize) -> Vec<FallbackHit> {
        collect_fallback_hits_impl(self, query, max_hits).await
    }

    async fn build_snapshot(&self) -> Result<BuildArtifacts> {
        let root = self.inner.profile.project_root().to_path_buf();
        let recent = recent_paths(&root);
        let churn = churn_scores(&root);
        let freshness = recency_days(&root);
        let owners = OwnerResolver::load(&root);
        let filter = self.inner.filter.clone();
        let text_sender = self.inner.text_ingest.sender();
        let snapshot = tokio::task::spawn_blocking(move || {
            IndexBuilder::new(
                root.as_path(),
                recent,
                churn,
                freshness,
                owners,
                filter,
                Some(text_sender),
            )
            .build()
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

    async fn record_profile_sample(
        &self,
        request: &SearchRequest,
        stats: &SearchStats,
        query_id: Option<QueryId>,
    ) {
        let mut guard = self.inner.profile_history.lock().await;
        if guard.len() >= PROFILE_HISTORY_LIMIT {
            guard.pop_front();
        }
        guard.push_back(SearchProfileSample {
            query_id,
            query: normalize_query(request.query.as_deref()),
            took_ms: stats.took_ms,
            candidate_size: stats.candidate_size,
            cache_hit: stats.cache_hit,
            literal_fallback: stats.literal_fallback,
            text_mode: stats.text_mode,
            timestamp: OffsetDateTime::now_utc(),
            stages: stats.stages.clone(),
        });
    }
}

fn normalize_query(query: Option<&str>) -> Option<String> {
    let text = query?.trim();
    if text.is_empty() {
        return None;
    }
    let mut owned = text.to_string();
    const MAX_LEN: usize = 160;
    if owned.len() > MAX_LEN {
        owned.truncate(MAX_LEN.saturating_sub(1));
        owned.push('…');
    }
    Some(owned)
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
        attention: file_entry.attention,
        attention_density: file_entry.attention_density,
        lint_suppressions: file_entry.lint_suppressions,
        lint_density: file_entry.lint_density,
        churn: file_entry.churn,
        freshness_days: file_entry.freshness_days,
        owners: file_entry.owners.clone(),
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
    if let Some(text) = indexed.text {
        snapshot.text.insert(path.clone(), text);
    }
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

fn summarize_active_filters(filters: &SearchFilters) -> Option<ActiveFilters> {
    let has_filters = !filters.languages.is_empty()
        || !filters.categories.is_empty()
        || !filters.path_globs.is_empty()
        || !filters.file_substrings.is_empty()
        || !filters.owners.is_empty()
        || filters.recent_only;
    if !has_filters {
        return None;
    }
    Some(ActiveFilters {
        languages: filters.languages.clone(),
        categories: filters.categories.clone(),
        path_globs: filters.path_globs.clone(),
        file_substrings: filters.file_substrings.clone(),
        owners: filters.owners.clone(),
        recent_only: filters.recent_only,
    })
}

fn build_context_banner(hits: &[NavHit]) -> Option<ContextBanner> {
    if hits.is_empty() {
        return None;
    }
    let mut layer_counts: HashMap<String, usize> = HashMap::new();
    let mut category_counts: HashMap<String, usize> = HashMap::new();
    for hit in hits {
        if let Some(layer) = &hit.layer
            && !layer.is_empty()
        {
            *layer_counts.entry(layer.clone()).or_insert(0) += 1;
        }
        for category in &hit.categories {
            if let Some(label) = context_category_label(category) {
                *category_counts.entry(label.to_string()).or_insert(0) += 1;
            }
        }
    }
    let layers = buckets_from_map(layer_counts, 4);
    let categories = buckets_from_map(category_counts, 3);
    if layers.is_empty() && categories.is_empty() {
        None
    } else {
        Some(ContextBanner { layers, categories })
    }
}

fn context_category_label(category: &FileCategory) -> Option<&'static str> {
    match category {
        FileCategory::Docs => Some("docs"),
        FileCategory::Tests => Some("tests"),
        FileCategory::Deps => Some("deps"),
        FileCategory::Source => None,
    }
}

fn buckets_from_map(map: HashMap<String, usize>, limit: usize) -> Vec<ContextBucket> {
    if map.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut entries: Vec<_> = map.into_iter().collect();
    entries.sort_by(|(name_a, count_a), (name_b, count_b)| {
        count_b.cmp(count_a).then_with(|| {
            name_a
                .to_ascii_lowercase()
                .cmp(&name_b.to_ascii_lowercase())
        })
    });
    entries.truncate(limit);
    entries
        .into_iter()
        .map(|(name, count)| ContextBucket { name, count })
        .collect()
}

fn rewrite_inherited_filters(cache: &QueryCache, request: &mut SearchRequest) -> Result<()> {
    let Some(refine_id) = request.refine else {
        return Err(anyhow!("inherit_filters requires --from query id"));
    };
    let mut chain = Vec::new();
    let mut cursor = Some(refine_id);
    while let Some(id) = cursor {
        let Some(entry) = cache.load(id)? else {
            break;
        };
        cursor = entry.parent;
        chain.push((id, entry));
    }
    if chain.is_empty() {
        return Err(anyhow!(
            "refine id {refine_id} missing from navigator cache"
        ));
    }
    let mut filters = chain[0].1.filters.clone();
    let additions = std::mem::take(&mut request.filters);
    let ops = std::mem::take(&mut request.filter_ops);
    for op in ops {
        apply_filter_op(&mut filters, &op);
    }
    merge_filter_additions(&mut filters, additions);
    let target_refine = select_refine_anchor(&chain, &filters);
    request.refine = Some(target_refine);
    request.filters = filters;
    request.inherit_filters = false;
    Ok(())
}

fn apply_filter_op(filters: &mut SearchFilters, op: &FilterOp) {
    match op {
        FilterOp::RemoveLanguage(lang) => {
            filters.languages.retain(|entry| entry != lang);
        }
        FilterOp::RemoveCategory(cat) => {
            filters.categories.retain(|entry| entry != cat);
        }
        FilterOp::RemovePathGlob(glob) => {
            filters.path_globs.retain(|entry| entry != glob);
        }
        FilterOp::RemoveFileSubstring(value) => {
            filters.file_substrings.retain(|entry| entry != value);
        }
        FilterOp::RemoveOwner(owner) => {
            filters.owners.retain(|entry| entry != owner);
        }
        FilterOp::SetRecentOnly(value) => {
            filters.recent_only = *value;
        }
        FilterOp::ClearFilters => {
            filters.languages.clear();
            filters.categories.clear();
            filters.path_globs.clear();
            filters.file_substrings.clear();
            filters.symbol_exact = None;
            filters.recent_only = false;
        }
    }
}

fn merge_filter_additions(filters: &mut SearchFilters, additions: SearchFilters) {
    for lang in additions.languages {
        if !filters.languages.contains(&lang) {
            filters.languages.push(lang);
        }
    }
    for category in additions.categories {
        if !filters.categories.contains(&category) {
            filters.categories.push(category);
        }
    }
    for glob in additions.path_globs {
        if !filters.path_globs.contains(&glob) {
            filters.path_globs.push(glob);
        }
    }
    for pattern in additions.file_substrings {
        if !filters.file_substrings.contains(&pattern) {
            filters.file_substrings.push(pattern);
        }
    }
    for owner in additions.owners {
        if !filters.owners.contains(&owner) {
            filters.owners.push(owner);
        }
    }
    if let Some(symbol) = additions.symbol_exact {
        filters.symbol_exact = Some(symbol);
    }
    if additions.recent_only {
        filters.recent_only = true;
    }
}

fn select_refine_anchor(
    chain: &[(QueryId, cache::CachedQuery)],
    desired: &SearchFilters,
) -> QueryId {
    for (id, entry) in chain {
        if filters_subset(&entry.filters, desired) {
            return *id;
        }
    }
    chain
        .last()
        .map(|(id, _)| *id)
        .unwrap_or_else(|| chain[0].0)
}

fn filters_subset(current: &SearchFilters, desired: &SearchFilters) -> bool {
    subset(&current.languages, &desired.languages)
        && subset(&current.categories, &desired.categories)
        && subset(&current.path_globs, &desired.path_globs)
        && subset(&current.file_substrings, &desired.file_substrings)
        && subset(&current.owners, &desired.owners)
        && (!current.recent_only || desired.recent_only)
        && match (&current.symbol_exact, &desired.symbol_exact) {
            (Some(lhs), Some(rhs)) => lhs == rhs,
            (Some(_), None) => false,
            (None, _) => true,
        }
}

fn subset<T: PartialEq>(needles: &[T], haystack: &[T]) -> bool {
    needles.iter().all(|needle| haystack.contains(needle))
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
    use crate::index::cache::CachedQuery;
    use crate::index::model::FileFingerprint;
    use crate::proto::CoverageReason;
    use crate::proto::FileCategory;
    use crate::proto::Language;
    use crate::proto::NavHit;
    use crate::proto::SymbolKind;
    use std::collections::HashMap;
    use tempfile::tempdir;

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
            attention: 0,
            attention_density: 0,
            lint_suppressions: 0,
            lint_density: 0,
            churn: 0,
            freshness_days: crate::index::model::DEFAULT_FRESHNESS_DAYS,
            owners: Vec::new(),
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

    #[test]
    fn context_banner_summarizes_layers_and_categories() {
        let hits = vec![
            fake_hit("core", vec![FileCategory::Docs]),
            fake_hit("core", vec![FileCategory::Docs]),
            fake_hit("tui", vec![FileCategory::Tests]),
            fake_hit("tui", vec![FileCategory::Tests]),
            fake_hit("tui", vec![FileCategory::Tests]),
            fake_hit("docs", vec![FileCategory::Docs]),
            fake_hit("infra", vec![FileCategory::Docs]),
        ];
        assert_eq!(hits.len(), 7);
        let banner = build_context_banner(&hits).expect("banner");
        let layer_map: HashMap<_, _> = banner
            .layers
            .iter()
            .map(|bucket| (bucket.name.as_str(), bucket.count))
            .collect();
        assert_eq!(layer_map.get("tui"), Some(&3));
        assert_eq!(layer_map.get("core"), Some(&2));
        assert_eq!(layer_map.get("docs"), Some(&1));
        let expected_docs = hits
            .iter()
            .filter(|hit| hit.categories.contains(&FileCategory::Docs))
            .count();
        let expected_tests = hits
            .iter()
            .filter(|hit| hit.categories.contains(&FileCategory::Tests))
            .count();
        let category_map: HashMap<_, _> = banner
            .categories
            .iter()
            .map(|bucket| (bucket.name.as_str(), bucket.count))
            .collect();
        assert_eq!(category_map.get("docs"), Some(&expected_docs));
        assert_eq!(category_map.get("tests"), Some(&expected_tests));
    }

    fn fake_hit(layer: &str, categories: Vec<FileCategory>) -> NavHit {
        NavHit {
            id: format!("id-{layer}-{}", categories.len()),
            path: format!("{layer}/file.rs"),
            line: 1,
            kind: SymbolKind::Function,
            language: Language::Rust,
            module: None,
            layer: Some(layer.to_string()),
            categories,
            recent: false,
            preview: "fn sample()".to_string(),
            match_count: None,
            score: 1.0,
            references: None,
            help: None,
            context_snippet: None,
            owners: Vec::new(),
            lint_suppressions: 0,
            freshness_days: 1,
            attention_density: 0,
            lint_density: 0,
        }
    }

    #[test]
    fn rewrite_inherited_filters_removes_language_and_promotes_parent() {
        let dir = tempdir().unwrap();
        let cache = QueryCache::new(dir.path().join("cache"));
        let root_id = QueryId::new_v4();
        let mut root_filters = SearchFilters::default();
        cache
            .store(
                root_id,
                CachedQuery {
                    candidate_ids: Vec::new(),
                    query: None,
                    filters: root_filters.clone(),
                    parent: None,
                },
            )
            .unwrap();

        let rust_id = QueryId::new_v4();
        root_filters.languages.push(Language::Rust);
        cache
            .store(
                rust_id,
                CachedQuery {
                    candidate_ids: Vec::new(),
                    query: None,
                    filters: root_filters.clone(),
                    parent: Some(root_id),
                },
            )
            .unwrap();

        let mut request = SearchRequest {
            refine: Some(rust_id),
            inherit_filters: true,
            filter_ops: vec![FilterOp::RemoveLanguage(Language::Rust)],
            ..Default::default()
        };

        rewrite_inherited_filters(&cache, &mut request).expect("rewrite");
        assert_eq!(request.refine, Some(root_id));
        assert!(request.filters.languages.is_empty());
    }

    #[test]
    fn rewrite_inherited_filters_adds_category_without_changing_refine() {
        let dir = tempdir().unwrap();
        let cache = QueryCache::new(dir.path().join("cache"));
        let base_id = QueryId::new_v4();
        let mut base_filters = SearchFilters::default();
        base_filters.languages.push(Language::Rust);
        cache
            .store(
                base_id,
                CachedQuery {
                    candidate_ids: Vec::new(),
                    query: None,
                    filters: base_filters.clone(),
                    parent: None,
                },
            )
            .unwrap();

        let mut request = SearchRequest {
            refine: Some(base_id),
            inherit_filters: true,
            filters: SearchFilters {
                categories: vec![FileCategory::Docs],
                ..SearchFilters::default()
            },
            ..Default::default()
        };

        rewrite_inherited_filters(&cache, &mut request).expect("rewrite");
        assert_eq!(request.refine, Some(base_id));
        assert!(request.filters.languages.contains(&Language::Rust));
        assert!(request.filters.categories.contains(&FileCategory::Docs));
    }
}
