use anyhow::Context;
use anyhow::Result;
use codex_navigator::proto::ActiveFilters;
use codex_navigator::proto::QueryId;
use codex_navigator::proto::SearchResponse;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HISTORY_FILENAME: &str = "history.json";
const MAX_RECENT: usize = 10;

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

    pub fn record_response(&self, response: &SearchResponse) -> Result<()> {
        let Some(query_id) = response.query_id else {
            return Ok(());
        };
        let mut history = self.read()?;
        history.last_query_id = Some(query_id);
        history.recent.retain(|entry| entry.query_id != query_id);
        history.recent.insert(
            0,
            QueryHistoryEntry {
                query_id,
                recorded_at: now_secs(),
                filters: response.active_filters.clone(),
            },
        );
        history.recent.truncate(MAX_RECENT);
        self.write(&history)
    }

    pub fn last_query_id(&self) -> Result<Option<QueryId>> {
        Ok(self.read()?.last_query_id)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<HistoryItem>> {
        let history = self.read()?;
        let mut rows = Vec::new();
        for entry in history.recent.iter().take(limit) {
            rows.push(HistoryItem {
                query_id: entry.query_id,
                recorded_at: entry.recorded_at,
                filters: entry.filters.clone(),
            });
        }
        Ok(rows)
    }

    pub fn entry_at(&self, index: usize) -> Result<Option<HistoryItem>> {
        let history = self.read()?;
        Ok(history.recent.get(index).map(|entry| HistoryItem {
            query_id: entry.query_id,
            recorded_at: entry.recorded_at,
            filters: entry.filters.clone(),
        }))
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryHistoryEntry {
    query_id: QueryId,
    recorded_at: u64,
    #[serde(default)]
    filters: Option<ActiveFilters>,
}

#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub query_id: QueryId,
    pub recorded_at: u64,
    pub filters: Option<ActiveFilters>,
}

fn now_secs() -> u64 {
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
        }
    }

    #[test]
    fn history_round_trip() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let mut first = sample_response(QueryId::new_v4());
        first.active_filters = Some(ActiveFilters {
            languages: Vec::new(),
            categories: Vec::new(),
            path_globs: Vec::new(),
            file_substrings: Vec::new(),
            owners: Vec::new(),
            recent_only: false,
        });
        store.record_response(&first).unwrap();
        let second = sample_response(QueryId::new_v4());
        store.record_response(&second).unwrap();
        let loaded = store.last_query_id().unwrap().expect("history id");
        assert_eq!(loaded, second.query_id.expect("second id"));
        let rows = store.recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].filters.is_some());
    }

    #[test]
    fn history_handles_missing_file() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        assert!(store.last_query_id().unwrap().is_none());
    }
}
