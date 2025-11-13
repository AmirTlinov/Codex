use crate::planner::NavigatorSearchArgs;
use crate::planner::StoredSearchArgs;
use crate::proto::ActiveFilters;
use crate::proto::FacetSuggestion;
use crate::proto::NavHit;
use crate::proto::QueryId;
use crate::proto::SearchResponse;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub const HISTORY_FILENAME: &str = "history.json";
pub const MAX_RECENT: usize = 10;
pub const MAX_PINNED: usize = 5;
const MAX_STORED_HITS: usize = 4;

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
        recorded_query: Option<RecordedQuery>,
        hits: Vec<HistoryHit>,
    ) -> Result<()> {
        let Some(query_id) = response.query_id else {
            return Ok(());
        };
        let mut history = self.read()?;
        history.last_query_id = Some(query_id);
        let entry = QueryHistoryEntry {
            query_id,
            recorded_at: now_secs(),
            filters: response.active_filters.clone(),
            hits,
            recorded_query,
            facet_suggestions: response.facet_suggestions.clone(),
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
        Ok(history
            .recent
            .iter()
            .take(limit)
            .map(|entry| HistoryItem::from_entry(entry, false))
            .collect())
    }

    pub fn entry_at(&self, index: usize) -> Result<Option<HistoryItem>> {
        let history = self.read()?;
        Ok(history
            .recent
            .get(index)
            .map(|entry| HistoryItem::from_entry(entry, false)))
    }

    pub fn pinned(&self) -> Result<Vec<HistoryItem>> {
        let history = self.read()?;
        Ok(history
            .pinned
            .iter()
            .map(|entry| HistoryItem::from_entry(entry, true))
            .collect())
    }

    pub fn pinned_entry_at(&self, index: usize) -> Result<Option<HistoryItem>> {
        let history = self.read()?;
        Ok(history
            .pinned
            .get(index)
            .map(|entry| HistoryItem::from_entry(entry, true)))
    }

    pub fn history_item(&self, index: usize, pinned: bool) -> Result<Option<HistoryItem>> {
        if pinned {
            self.pinned_entry_at(index)
        } else {
            self.entry_at(index)
        }
    }

    pub fn pin_recent(&self, index: usize) -> Result<HistoryItem> {
        let mut history = self.read()?;
        let Some(entry) = history.recent.get(index).cloned() else {
            anyhow::bail!("history index {index} not available; run `codex navigator` first");
        };
        if entry.recorded_query.is_none() {
            anyhow::bail!(
                "history entry {index} cannot be pinned because it lacks replay metadata"
            );
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

    pub fn replay_recent(&self, index: usize) -> Result<Option<RecordedQuery>> {
        let history = self.read()?;
        Ok(history
            .recent
            .get(index)
            .and_then(|entry| entry.recorded_query.clone()))
    }

    pub fn replay_pinned(&self, index: usize) -> Result<Option<RecordedQuery>> {
        let history = self.read()?;
        Ok(history
            .pinned
            .get(index)
            .and_then(|entry| entry.recorded_query.clone()))
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
    recorded_query: Option<RecordedQuery>,
    #[serde(default)]
    facet_suggestions: Vec<FacetSuggestion>,
}

#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub query_id: QueryId,
    pub recorded_at: u64,
    pub filters: Option<ActiveFilters>,
    pub hits: Vec<HistoryHit>,
    pub is_pinned: bool,
    pub recorded_query: Option<RecordedQuery>,
    pub facet_suggestions: Vec<FacetSuggestion>,
}

impl HistoryItem {
    fn from_entry(entry: &QueryHistoryEntry, is_pinned: bool) -> Self {
        Self {
            query_id: entry.query_id,
            recorded_at: entry.recorded_at,
            filters: entry.filters.clone(),
            hits: entry.hits.clone(),
            is_pinned,
            recorded_query: entry.recorded_query.clone(),
            facet_suggestions: entry.facet_suggestions.clone(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedQuery {
    pub args: StoredSearchArgs,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub refs_mode: Option<String>,
    #[serde(default)]
    pub show_refs: Option<bool>,
    #[serde(default)]
    pub diagnostics_only: Option<bool>,
    #[serde(default)]
    pub focus_mode: Option<String>,
}

impl RecordedQuery {
    pub fn from_args(args: &NavigatorSearchArgs) -> Self {
        Self {
            args: StoredSearchArgs::from(args),
            output_format: None,
            refs_mode: None,
            show_refs: None,
            diagnostics_only: None,
            focus_mode: None,
        }
    }

    pub fn into_args(self) -> NavigatorSearchArgs {
        self.args.into_args()
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn capture_history_hits(hits: &[NavHit]) -> Vec<HistoryHit> {
    hits.iter()
        .take(MAX_STORED_HITS)
        .map(|hit| HistoryHit {
            path: hit.path.clone(),
            line: hit.line,
            layer: hit.layer.clone(),
            preview: hit.preview.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::FacetSuggestionKind;
    use crate::proto::IndexState;
    use crate::proto::IndexStatus;
    use crate::proto::Language;
    use crate::proto::PROTOCOL_VERSION;
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
            facet_suggestions: Vec::new(),
        }
    }

    #[test]
    fn history_round_trip() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let mut args = NavigatorSearchArgs::default();
        args.query = Some("fn sample".to_string());
        let recorded = RecordedQuery::from_args(&args);
        let mut response = sample_response(QueryId::new_v4());
        response.active_filters = Some(ActiveFilters {
            languages: vec![Language::Rust],
            categories: Vec::new(),
            path_globs: Vec::new(),
            file_substrings: Vec::new(),
            owners: Vec::new(),
            recent_only: false,
        });
        response.facet_suggestions = vec![crate::proto::FacetSuggestion {
            label: "lang=rust".to_string(),
            command: "codex navigator facet --lang rust".to_string(),
            kind: FacetSuggestionKind::Language,
            value: Some("rust".to_string()),
        }];
        store
            .record_entry(
                &response,
                Some(recorded),
                vec![HistoryHit {
                    path: "src/lib.rs".to_string(),
                    line: 1,
                    layer: Some("core".to_string()),
                    preview: "fn demo()".to_string(),
                }],
            )
            .unwrap();
        let second = sample_response(QueryId::new_v4());
        store
            .record_entry(&second, None, Vec::new())
            .expect("record second");
        let loaded = store.last_query_id().unwrap().expect("history id");
        assert_eq!(loaded, second.query_id.expect("second id"));
        let rows = store.recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].filters.is_some());
        assert!(rows[0].recorded_query.is_none());
        assert_eq!(rows[1].hits.len(), 1);
        assert_eq!(rows[1].facet_suggestions.len(), 1);
        assert!(rows[1].recorded_query.is_some());
    }

    #[test]
    fn pin_and_unpin_entries() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let mut args = NavigatorSearchArgs::default();
        args.query = Some("struct Foo".to_string());
        let first = sample_response(QueryId::new_v4());
        store
            .record_entry(&first, Some(RecordedQuery::from_args(&args)), Vec::new())
            .unwrap();
        store.pin_recent(0).unwrap();
        let pinned = store.pinned().unwrap();
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].is_pinned);
        assert!(pinned[0].recorded_query.is_some());
        store.unpin(0).unwrap();
        assert!(store.pinned().unwrap().is_empty());
    }

}
