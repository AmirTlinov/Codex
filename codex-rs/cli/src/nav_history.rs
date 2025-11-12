use crate::code_nav::FocusMode;
use crate::code_nav::OutputFormat;
use crate::code_nav::RefsMode;
use anyhow::Context;
use anyhow::Result;
use codex_navigator::planner::NavigatorSearchArgs;
use codex_navigator::proto::ActiveFilters;
use codex_navigator::proto::InputFormat;
use codex_navigator::proto::QueryId;
use codex_navigator::proto::SearchProfile;
use codex_navigator::proto::SearchResponse;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HISTORY_FILENAME: &str = "history.json";
const MAX_RECENT: usize = 10;
const MAX_PINNED: usize = 5;

#[derive(Debug, Clone)]
pub struct QueryHistoryStore {
    path: PathBuf,
}

impl QueryHistoryStore {
    pub fn new(queries_dir: PathBuf) -> Self {
        Self {
            path: queries_dir.join(HISTORY_FILENAME),
        }
    }

    pub fn record_entry(
        &self,
        response: &SearchResponse,
        replay: Option<&HistoryReplay>,
        hits: Vec<HistoryHit>,
    ) -> Result<()> {
        let Some(query_id) = response.query_id else {
            return Ok(());
        };
        let mut history = self.read()?;
        history.last_query_id = Some(query_id);
        let stored_replay = replay.map(RecordedQuery::from_replay);
        let entry = QueryHistoryEntry {
            query_id,
            recorded_at: now_secs(),
            filters: response.active_filters.clone(),
            hits,
            replay: stored_replay,
        };
        history
            .recent
            .retain(|existing| existing.query_id != query_id);
        history.recent.insert(0, entry.clone());
        history.recent.truncate(MAX_RECENT);
        for pinned in history.pinned.iter_mut() {
            if pinned.query_id == query_id {
                *pinned = entry.clone();
            }
        }
        self.write(&history)
    }

    pub fn last_query_id(&self) -> Result<Option<QueryId>> {
        Ok(self.read()?.last_query_id)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<HistoryItem>> {
        let history = self.read()?;
        let pinned_ids: HashSet<_> = history.pinned.iter().map(|entry| entry.query_id).collect();
        Ok(history
            .recent
            .iter()
            .take(limit)
            .map(|entry| HistoryItem::from_entry(entry, pinned_ids.contains(&entry.query_id)))
            .collect())
    }

    pub fn entry_at(&self, index: usize) -> Result<Option<HistoryItem>> {
        let history = self.read()?;
        let pinned_ids: HashSet<_> = history.pinned.iter().map(|entry| entry.query_id).collect();
        Ok(history
            .recent
            .get(index)
            .map(|entry| HistoryItem::from_entry(entry, pinned_ids.contains(&entry.query_id))))
    }

    pub fn pinned(&self) -> Result<Vec<HistoryItem>> {
        let history = self.read()?;
        Ok(history
            .pinned
            .iter()
            .map(|entry| HistoryItem::from_entry(entry, true))
            .collect())
    }

    pub fn pin_recent(&self, index: usize) -> Result<HistoryItem> {
        let mut history = self.read()?;
        let Some(entry) = history.recent.get(index).cloned() else {
            return Err(anyhow::anyhow!(
                "history index {index} not available; run `codex navigator` first"
            ));
        };
        if entry.replay.is_none() {
            return Err(anyhow::anyhow!(
                "history entry {index} cannot be pinned because it lacks replay metadata"
            ));
        }
        if history.pinned.iter().any(|p| p.query_id == entry.query_id) {
            return Ok(HistoryItem::from_entry(&entry, true));
        }
        history.pinned.insert(0, entry.clone());
        history.pinned.truncate(MAX_PINNED);
        self.write(&history)?;
        Ok(HistoryItem::from_entry(&entry, true))
    }

    pub fn unpin(&self, index: usize) -> Result<Option<HistoryItem>> {
        let mut history = self.read()?;
        if index >= history.pinned.len() {
            return Ok(None);
        }
        let entry = history.pinned.remove(index);
        self.write(&history)?;
        Ok(Some(HistoryItem::from_entry(&entry, false)))
    }

    pub fn replay_recent(&self, index: usize) -> Result<Option<HistoryReplay>> {
        let history = self.read()?;
        Ok(history
            .recent
            .get(index)
            .and_then(|entry| entry.replay.clone())
            .map(RecordedQuery::into_replay))
    }

    pub fn replay_pinned(&self, index: usize) -> Result<Option<HistoryReplay>> {
        let history = self.read()?;
        Ok(history
            .pinned
            .get(index)
            .and_then(|entry| entry.replay.clone())
            .map(RecordedQuery::into_replay))
    }

    fn read(&self) -> Result<QueryHistory> {
        match fs::read(&self.path) {
            Ok(data) => serde_json::from_slice(&data).context("parse navigator query history"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(QueryHistory::default()),
            Err(err) => Err(err).context("read navigator query history"),
        }
    }

    fn write(&self, history: &QueryHistory) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("create navigator history dir")?;
        }
        let data =
            serde_json::to_vec_pretty(history).context("serialize navigator query history")?;
        fs::write(&self.path, data).context("write navigator query history")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct QueryHistory {
    last_query_id: Option<QueryId>,
    #[serde(default)]
    recent: Vec<QueryHistoryEntry>,
    #[serde(default)]
    pinned: Vec<QueryHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryHistoryEntry {
    query_id: QueryId,
    recorded_at: u64,
    #[serde(default)]
    filters: Option<ActiveFilters>,
    #[serde(default)]
    hits: Vec<HistoryHit>,
    #[serde(default)]
    replay: Option<RecordedQuery>,
}

#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub query_id: QueryId,
    pub recorded_at: u64,
    pub filters: Option<ActiveFilters>,
    pub hits: Vec<HistoryHit>,
    pub is_pinned: bool,
    pub replay: Option<HistoryReplay>,
}

impl HistoryItem {
    fn from_entry(entry: &QueryHistoryEntry, is_pinned: bool) -> Self {
        Self {
            query_id: entry.query_id,
            recorded_at: entry.recorded_at,
            filters: entry.filters.clone(),
            hits: entry.hits.clone(),
            is_pinned,
            replay: entry.replay.clone().map(RecordedQuery::into_replay),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryHit {
    pub path: String,
    pub line: u32,
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct HistoryReplay {
    pub args: NavigatorSearchArgs,
    pub output_format: OutputFormat,
    pub refs_mode: RefsMode,
    pub show_refs: bool,
    pub diagnostics_only: bool,
    pub focus_mode: FocusMode,
}

impl HistoryReplay {
    pub fn new(
        args: NavigatorSearchArgs,
        output_format: OutputFormat,
        refs_mode: RefsMode,
        show_refs: bool,
        diagnostics_only: bool,
        focus_mode: FocusMode,
    ) -> Self {
        Self {
            args,
            output_format,
            refs_mode,
            show_refs,
            diagnostics_only,
            focus_mode,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordedQuery {
    args: StoredSearchArgs,
    output_format: OutputFormat,
    refs_mode: RefsMode,
    show_refs: bool,
    diagnostics_only: bool,
    focus_mode: FocusMode,
}

impl RecordedQuery {
    fn from_replay(replay: &HistoryReplay) -> Self {
        Self {
            args: StoredSearchArgs::from(&replay.args),
            output_format: replay.output_format,
            refs_mode: replay.refs_mode,
            show_refs: replay.show_refs,
            diagnostics_only: replay.diagnostics_only,
            focus_mode: replay.focus_mode,
        }
    }

    fn into_replay(self) -> HistoryReplay {
        HistoryReplay {
            args: self.args.into_args(),
            output_format: self.output_format,
            refs_mode: self.refs_mode,
            show_refs: self.show_refs,
            diagnostics_only: self.diagnostics_only,
            focus_mode: self.focus_mode,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredSearchArgs {
    query: Option<String>,
    limit: Option<usize>,
    kinds: Vec<String>,
    languages: Vec<String>,
    categories: Vec<String>,
    path_globs: Vec<String>,
    file_substrings: Vec<String>,
    owners: Vec<String>,
    symbol_exact: Option<String>,
    recent_only: Option<bool>,
    only_tests: Option<bool>,
    only_docs: Option<bool>,
    only_deps: Option<bool>,
    with_refs: Option<bool>,
    refs_limit: Option<usize>,
    refs_role: Option<String>,
    help_symbol: Option<String>,
    refine: Option<String>,
    wait_for_index: Option<bool>,
    profiles: Vec<SearchProfile>,
    remove_languages: Vec<String>,
    remove_categories: Vec<String>,
    remove_path_globs: Vec<String>,
    remove_file_substrings: Vec<String>,
    remove_owners: Vec<String>,
    clear_filters: bool,
    disable_recent_only: bool,
    inherit_filters: bool,
    input_format: InputFormat,
}

impl From<&NavigatorSearchArgs> for StoredSearchArgs {
    fn from(args: &NavigatorSearchArgs) -> Self {
        Self {
            query: args.query.clone(),
            limit: args.limit,
            kinds: args.kinds.clone(),
            languages: args.languages.clone(),
            categories: args.categories.clone(),
            path_globs: args.path_globs.clone(),
            file_substrings: args.file_substrings.clone(),
            owners: args.owners.clone(),
            symbol_exact: args.symbol_exact.clone(),
            recent_only: args.recent_only,
            only_tests: args.only_tests,
            only_docs: args.only_docs,
            only_deps: args.only_deps,
            with_refs: args.with_refs,
            refs_limit: args.refs_limit,
            refs_role: args.refs_role.clone(),
            help_symbol: args.help_symbol.clone(),
            refine: args.refine.clone(),
            wait_for_index: args.wait_for_index,
            profiles: args.profiles.clone(),
            remove_languages: args.remove_languages.clone(),
            remove_categories: args.remove_categories.clone(),
            remove_path_globs: args.remove_path_globs.clone(),
            remove_file_substrings: args.remove_file_substrings.clone(),
            remove_owners: args.remove_owners.clone(),
            clear_filters: args.clear_filters,
            disable_recent_only: args.disable_recent_only,
            inherit_filters: args.inherit_filters,
            input_format: args.input_format,
        }
    }
}

impl StoredSearchArgs {
    fn into_args(self) -> NavigatorSearchArgs {
        let mut args = NavigatorSearchArgs::default();
        args.query = self.query;
        args.limit = self.limit;
        args.kinds = self.kinds;
        args.languages = self.languages;
        args.categories = self.categories;
        args.path_globs = self.path_globs;
        args.file_substrings = self.file_substrings;
        args.owners = self.owners;
        args.symbol_exact = self.symbol_exact;
        args.recent_only = self.recent_only;
        args.only_tests = self.only_tests;
        args.only_docs = self.only_docs;
        args.only_deps = self.only_deps;
        args.with_refs = self.with_refs;
        args.refs_limit = self.refs_limit;
        args.refs_role = self.refs_role;
        args.help_symbol = self.help_symbol;
        args.refine = self.refine;
        args.wait_for_index = self.wait_for_index;
        args.profiles = self.profiles;
        args.remove_languages = self.remove_languages;
        args.remove_categories = self.remove_categories;
        args.remove_path_globs = self.remove_path_globs;
        args.remove_file_substrings = self.remove_file_substrings;
        args.remove_owners = self.remove_owners;
        args.clear_filters = self.clear_filters;
        args.disable_recent_only = self.disable_recent_only;
        args.inherit_filters = self.inherit_filters;
        args.input_format = self.input_format;
        args
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_navigator::proto::IndexState;
    use codex_navigator::proto::IndexStatus;
    use codex_navigator::proto::PROTOCOL_VERSION;
    use tempfile::tempdir;

    fn sample_response(id: QueryId) -> SearchResponse {
        SearchResponse {
            query_id: Some(id),
            hits: Vec::new(),
            index: IndexStatus {
                state: IndexState::Ready,
                symbols: 0,
                files: 0,
                updated_at: None,
                progress: None,
                schema_version: PROTOCOL_VERSION,
                notice: None,
                auto_indexing: true,
                coverage: None,
            },
            stats: None,
            hints: Vec::new(),
            error: None,
            diagnostics: None,
            fallback_hits: Vec::new(),
            atlas_hint: None,
            active_filters: None,
            context_banner: None,
        }
    }

    fn sample_replay() -> HistoryReplay {
        let mut args = NavigatorSearchArgs::default();
        args.query = Some("fn sample".to_string());
        args.limit = Some(10);
        args.languages = vec!["rust".to_string()];
        args.only_tests = Some(true);
        args.profiles = vec![SearchProfile::Focused];
        HistoryReplay::new(
            args,
            OutputFormat::Text,
            RefsMode::All,
            true,
            false,
            FocusMode::Docs,
        )
    }

    #[test]
    fn history_round_trip() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let replay = sample_replay();
        let mut first = sample_response(QueryId::new_v4());
        first.active_filters = Some(ActiveFilters {
            languages: Vec::new(),
            categories: Vec::new(),
            path_globs: Vec::new(),
            file_substrings: Vec::new(),
            owners: Vec::new(),
            recent_only: false,
        });
        let hits = vec![HistoryHit {
            path: "src/lib.rs".to_string(),
            line: 1,
            layer: Some("core".to_string()),
            preview: "fn demo()".to_string(),
        }];
        store.record_entry(&first, Some(&replay), hits).unwrap();
        let second = sample_response(QueryId::new_v4());
        store.record_entry(&second, None, Vec::new()).unwrap();
        let loaded = store.last_query_id().unwrap().expect("history id");
        assert_eq!(loaded, second.query_id.expect("second id"));
        let rows = store.recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].filters.is_some());
        assert!(rows[0].replay.is_none());
        assert_eq!(rows[1].hits.len(), 1);
        assert_eq!(
            rows[1].replay.as_ref().expect("replay metadata").focus_mode,
            FocusMode::Docs
        );
    }

    #[test]
    fn pin_and_replay_entries() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let replay = sample_replay();
        let response = sample_response(QueryId::new_v4());
        store
            .record_entry(&response, Some(&replay), Vec::new())
            .unwrap();
        store.pin_recent(0).unwrap();
        let pinned = store.pinned().unwrap();
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].is_pinned);
        let replayed = store.replay_pinned(0).unwrap().expect("replay");
        assert_eq!(replayed.output_format, OutputFormat::Text);
        assert_eq!(replayed.args.query.as_deref(), Some("fn sample"));
        assert_eq!(replayed.focus_mode, FocusMode::Docs);
        store.unpin(0).unwrap();
        assert!(store.pinned().unwrap().is_empty());
    }

    #[test]
    fn history_handles_missing_file() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        assert!(store.last_query_id().unwrap().is_none());
    }
}
