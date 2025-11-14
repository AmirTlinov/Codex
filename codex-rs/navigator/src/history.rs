use crate::atlas_hint_label;
use crate::planner::NavigatorSearchArgs;
use crate::planner::StoredSearchArgs;
use crate::planner::category_label;
use crate::planner::language_label;
use crate::proto::ActiveFilters;
use crate::proto::AtlasHint;
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
            atlas_hint: response.atlas_hint.clone(),
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
    #[serde(default)]
    atlas_hint: Option<AtlasHint>,
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
    pub atlas_hint: Option<AtlasHint>,
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
            atlas_hint: entry.atlas_hint.clone(),
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

pub fn summarize_history_query(item: &HistoryItem) -> Option<String> {
    let recorded = item.recorded_query.as_ref()?;
    let query = recorded.args.query.as_ref()?;
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    const LIMIT: usize = 80;
    if trimmed.len() <= LIMIT {
        Some(trimmed.to_string())
    } else {
        let mut owned = trimmed[..LIMIT - 1].to_string();
        owned.push('…');
        Some(owned)
    }
}

pub fn history_item_matches(item: &HistoryItem, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let lowered = needle.to_ascii_lowercase();
    if let Some(recorded) = item.recorded_query.as_ref()
        && recorded
            .args
            .query
            .as_deref()
            .is_some_and(|text| text.to_ascii_lowercase().contains(&lowered))
    {
        return true;
    }
    if let Some(filters) = item.filters.as_ref()
        && filters_match(filters, &lowered)
    {
        return true;
    }
    if let Some(hint) = item.atlas_hint.as_ref()
        && atlas_hint_label(hint)
            .to_ascii_lowercase()
            .contains(&lowered)
    {
        return true;
    }
    if item.facet_suggestions.iter().any(|suggestion| {
        suggestion.label.to_ascii_lowercase().contains(&lowered)
            || suggestion.command.to_ascii_lowercase().contains(&lowered)
    }) {
        return true;
    }
    if item.hits.iter().any(|hit| hit_matches(hit, &lowered)) {
        return true;
    }
    if let Some(recorded) = item.recorded_query.as_ref() {
        if recorded
            .args
            .path_globs
            .iter()
            .any(|glob| glob.to_ascii_lowercase().contains(&lowered))
        {
            return true;
        }
        if recorded
            .args
            .file_substrings
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(&lowered))
        {
            return true;
        }
        if recorded
            .args
            .owners
            .iter()
            .any(|owner| owner.to_ascii_lowercase().contains(&lowered))
        {
            return true;
        }
    }
    if lowered == "repeat" && item.recorded_query.is_some() {
        return true;
    }
    item.query_id
        .to_string()
        .to_ascii_lowercase()
        .contains(&lowered)
}

fn filters_match(filters: &ActiveFilters, needle: &str) -> bool {
    filters
        .languages
        .iter()
        .map(language_label)
        .any(|label| label.contains(needle))
        || filters
            .categories
            .iter()
            .map(category_label)
            .any(|label| label.contains(needle))
        || filters
            .owners
            .iter()
            .any(|owner| owner.to_ascii_lowercase().contains(needle))
        || filters
            .path_globs
            .iter()
            .any(|glob| glob.to_ascii_lowercase().contains(needle))
        || filters
            .file_substrings
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(needle))
        || (filters.recent_only && needle == "recent")
}

fn hit_matches(hit: &HistoryHit, needle: &str) -> bool {
    hit.path.to_ascii_lowercase().contains(needle)
        || hit.preview.to_ascii_lowercase().contains(needle)
        || hit
            .layer
            .as_ref()
            .is_some_and(|layer| layer.to_ascii_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::AtlasHint;
    use crate::proto::AtlasHintSummary;
    use crate::proto::AtlasNodeKind;
    use crate::proto::FacetSuggestionKind;
    use crate::proto::IndexState;
    use crate::proto::IndexStatus;
    use crate::proto::Language;
    use crate::proto::PROTOCOL_VERSION;
    use tempfile::tempdir;
    use uuid::Uuid;

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

    fn sample_atlas_hint() -> AtlasHint {
        AtlasHint {
            target: Some("core/planner".to_string()),
            matched: true,
            breadcrumb: vec!["core".to_string(), "planner".to_string()],
            focus: AtlasHintSummary {
                name: "planner".to_string(),
                kind: AtlasNodeKind::Module,
                file_count: 4,
                symbol_count: 32,
                loc: 900,
                recent_files: 2,
                doc_files: 1,
                test_files: 1,
                dep_files: 0,
            },
            top_children: vec![AtlasHintSummary {
                name: "auto_facet".to_string(),
                kind: AtlasNodeKind::Module,
                file_count: 2,
                symbol_count: 11,
                loc: 400,
                recent_files: 1,
                doc_files: 0,
                test_files: 0,
                dep_files: 0,
            }],
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
        response.atlas_hint = Some(sample_atlas_hint());
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
        assert!(rows[1].atlas_hint.is_some());
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

    #[test]
    fn history_item_matches_atlas_hint_label() {
        let item = HistoryItem {
            query_id: Uuid::new_v4(),
            recorded_at: 0,
            filters: None,
            hits: Vec::new(),
            is_pinned: false,
            recorded_query: None,
            facet_suggestions: Vec::new(),
            atlas_hint: Some(sample_atlas_hint()),
        };
        assert!(history_item_matches(&item, "planner"));
        assert!(history_item_matches(&item, "auto_facet"));
        assert!(!history_item_matches(&item, "missing-term"));
    }
}
