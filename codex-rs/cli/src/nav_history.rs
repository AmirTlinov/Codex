use crate::code_nav::FocusMode;
use crate::code_nav::OutputFormat;
use crate::code_nav::RefsMode;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
pub use codex_navigator::history::HistoryHit;
pub use codex_navigator::history::HistoryItem;
pub use codex_navigator::history::QueryHistoryStore;
pub use codex_navigator::history::RecordedQuery;
use codex_navigator::history::now_secs;
use codex_navigator::planner::NavigatorSearchArgs;
use codex_navigator::planner::StoredSearchArgs;
use codex_navigator::proto::ActiveFilters;
use codex_navigator::proto::QueryId;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

const FACET_PRESETS_FILENAME: &str = "facet_presets.json";
const MAX_PRESETS: usize = 24;

#[derive(Debug, Clone)]
pub struct FacetPresetStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetPreset {
    pub name: String,
    #[serde(default)]
    pub filters: ActiveFilters,
    pub saved_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FacetPresetBook {
    #[serde(default)]
    presets: Vec<FacetPreset>,
}

impl FacetPresetStore {
    pub fn new(queries_dir: PathBuf) -> Self {
        Self {
            path: queries_dir.join(FACET_PRESETS_FILENAME),
        }
    }

    pub fn list(&self) -> Result<Vec<FacetPreset>> {
        Ok(self.read()?.presets)
    }

    pub fn get(&self, name: &str) -> Result<Option<FacetPreset>> {
        let target = normalize_name(name)?;
        let book = self.read()?;
        Ok(book
            .presets
            .into_iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(&target)))
    }

    pub fn save(&self, name: &str, filters: ActiveFilters) -> Result<FacetPreset> {
        if is_filters_empty(&filters) {
            return Err(anyhow!("cannot save preset without active filters"));
        }
        let normalized = normalize_name(name)?;
        let mut book = self.read()?;
        let saved_at = now_secs();
        if let Some(index) = book
            .presets
            .iter()
            .position(|preset| preset.name.eq_ignore_ascii_case(&normalized))
        {
            {
                let existing = &mut book.presets[index];
                existing.name = normalized.clone();
                existing.filters = filters;
                existing.saved_at = saved_at;
            }
            let entry = book.presets[index].clone();
            self.write(&book)?;
            return Ok(entry);
        }
        let entry = FacetPreset {
            name: normalized,
            filters,
            saved_at,
        };
        book.presets.insert(0, entry.clone());
        if book.presets.len() > MAX_PRESETS {
            book.presets.truncate(MAX_PRESETS);
        }
        self.write(&book)?;
        Ok(entry)
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        let normalized = normalize_name(name)?;
        let mut book = self.read()?;
        let before = book.presets.len();
        book.presets
            .retain(|preset| !preset.name.eq_ignore_ascii_case(&normalized));
        let removed = before != book.presets.len();
        if removed {
            self.write(&book)?;
        }
        Ok(removed)
    }

    fn read(&self) -> Result<FacetPresetBook> {
        match fs::read(&self.path) {
            Ok(data) => serde_json::from_slice(&data).context("parse facet preset store"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(FacetPresetBook::default())
            }
            Err(err) => Err(err).context("read facet preset store"),
        }
    }

    fn write(&self, book: &FacetPresetBook) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("create facet preset dir")?;
        }
        let data = serde_json::to_vec_pretty(book).context("serialize facet presets")?;
        fs::write(&self.path, data).context("write facet preset store")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntryView {
    pub index: usize,
    pub query_id: QueryId,
    pub recorded_secs_ago: u64,
    pub is_pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<ActiveFilters>,
    #[serde(default)]
    pub filter_chips: Vec<String>,
    #[serde(default)]
    pub hits: Vec<HistoryHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestion_commands: Vec<SuggestionCommandView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionCommandView {
    pub index: usize,
    pub label: String,
    pub command: String,
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

pub fn recorded_query_from_replay(replay: &HistoryReplay) -> RecordedQuery {
    RecordedQuery {
        args: StoredSearchArgs::from(&replay.args),
        output_format: Some(output_format_label(replay.output_format).to_string()),
        refs_mode: Some(refs_mode_label(replay.refs_mode).to_string()),
        show_refs: Some(replay.show_refs),
        diagnostics_only: Some(replay.diagnostics_only),
        focus_mode: Some(focus_mode_label(replay.focus_mode).to_string()),
    }
}

impl TryFrom<&RecordedQuery> for HistoryReplay {
    type Error = anyhow::Error;

    fn try_from(recorded: &RecordedQuery) -> Result<Self> {
        Ok(HistoryReplay {
            args: recorded.args.clone().into_args(),
            output_format: parse_output_format(recorded.output_format.as_deref()),
            refs_mode: parse_refs_mode(recorded.refs_mode.as_deref()),
            show_refs: recorded.show_refs.unwrap_or(false),
            diagnostics_only: recorded.diagnostics_only.unwrap_or(false),
            focus_mode: parse_focus_mode(recorded.focus_mode.as_deref()),
        })
    }
}

pub fn history_replay_from_item(item: &HistoryItem) -> Option<HistoryReplay> {
    item.recorded_query
        .as_ref()
        .and_then(|recorded| HistoryReplay::try_from(recorded).ok())
}

fn output_format_label(value: OutputFormat) -> &'static str {
    match value {
        OutputFormat::Json => "Json",
        OutputFormat::Ndjson => "Ndjson",
        OutputFormat::Text => "Text",
    }
}

fn refs_mode_label(value: RefsMode) -> &'static str {
    match value {
        RefsMode::All => "All",
        RefsMode::Definitions => "Definitions",
        RefsMode::Usages => "Usages",
    }
}

fn focus_mode_label(value: FocusMode) -> &'static str {
    match value {
        FocusMode::Auto => "Auto",
        FocusMode::All => "All",
        FocusMode::Code => "Code",
        FocusMode::Docs => "Docs",
        FocusMode::Tests => "Tests",
        FocusMode::Deps => "Deps",
    }
}

fn parse_output_format(value: Option<&str>) -> OutputFormat {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("text") => OutputFormat::Text,
        Some("ndjson") => OutputFormat::Ndjson,
        _ => OutputFormat::Json,
    }
}

fn parse_refs_mode(value: Option<&str>) -> RefsMode {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("definitions") => RefsMode::Definitions,
        Some("usages") => RefsMode::Usages,
        _ => RefsMode::All,
    }
}

fn parse_focus_mode(value: Option<&str>) -> FocusMode {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("all") => FocusMode::All,
        Some("code") => FocusMode::Code,
        Some("docs") => FocusMode::Docs,
        Some("tests") => FocusMode::Tests,
        Some("deps") => FocusMode::Deps,
        _ => FocusMode::Auto,
    }
}

fn normalize_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        Err(anyhow!("preset name cannot be empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn is_filters_empty(filters: &ActiveFilters) -> bool {
    filters.languages.is_empty()
        && filters.categories.is_empty()
        && filters.path_globs.is_empty()
        && filters.file_substrings.is_empty()
        && filters.owners.is_empty()
        && !filters.recent_only
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_navigator::proto::FacetSuggestion;
    use codex_navigator::proto::FacetSuggestionKind;
    use codex_navigator::proto::IndexState;
    use codex_navigator::proto::IndexStatus;
    use codex_navigator::proto::Language;
    use codex_navigator::proto::PROTOCOL_VERSION;
    use codex_navigator::proto::SearchProfile;
    use codex_navigator::proto::SearchResponse;
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
        first.facet_suggestions = vec![FacetSuggestion {
            label: "lang=rust".to_string(),
            command: "codex navigator facet --lang rust".to_string(),
            kind: FacetSuggestionKind::Language,
            value: Some("rust".to_string()),
        }];
        store
            .record_entry(&first, Some(recorded_query_from_replay(&replay)), hits)
            .unwrap();
        let second = sample_response(QueryId::new_v4());
        store.record_entry(&second, None, Vec::new()).unwrap();
        let loaded = store.last_query_id().unwrap().expect("history id");
        assert_eq!(loaded, second.query_id.expect("second id"));
        let rows = store.recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].filters.is_some());
        assert!(rows[0].recorded_query.is_none());
        assert_eq!(rows[1].hits.len(), 1);
        assert_eq!(rows[1].facet_suggestions.len(), 1);
        let replay = history_replay_from_item(&rows[1]).expect("replay metadata");
        assert_eq!(replay.focus_mode, FocusMode::Docs);
    }

    #[test]
    fn pin_and_replay_entries() {
        let dir = tempdir().unwrap();
        let store = QueryHistoryStore::new(dir.path().to_path_buf());
        let replay = sample_replay();
        let response = sample_response(QueryId::new_v4());
        store
            .record_entry(
                &response,
                Some(recorded_query_from_replay(&replay)),
                Vec::new(),
            )
            .unwrap();
        store.pin_recent(0).unwrap();
        let pinned = store.pinned().unwrap();
        assert_eq!(pinned.len(), 1);
        assert!(pinned[0].is_pinned);
        let recorded = store.replay_pinned(0).unwrap().expect("replay metadata");
        let replayed = HistoryReplay::try_from(&recorded).expect("convert replay");
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

    #[test]
    fn facet_preset_store_round_trip() {
        let dir = tempdir().unwrap();
        let store = FacetPresetStore::new(dir.path().to_path_buf());
        assert!(store.list().unwrap().is_empty());
        let mut filters = ActiveFilters::default();
        filters.languages.push(Language::Rust);
        filters.recent_only = true;
        let saved = store.save("rust focus", filters.clone()).unwrap();
        assert_eq!(saved.name, "rust focus");
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].filters.languages.len(), 1);
        assert!(store.get("rust focus").unwrap().is_some());
        assert!(store.remove("rust focus").unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn facet_preset_store_rejects_empty_filters() {
        let dir = tempdir().unwrap();
        let store = FacetPresetStore::new(dir.path().to_path_buf());
        let err = store.save("empty", ActiveFilters::default()).unwrap_err();
        assert!(err.to_string().contains("active filters"));
    }
}
