use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::io::Result;
use std::path::PathBuf;

use crate::format_percent;
use crate::format_token_count;
use crate::key_hint;
use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt as _;
use crate::text_formatting::center_truncate_path;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use codex_core::config::AgentContextEntry;
use codex_core::config::AgentToolEntry;
use codex_core::config::AgentsContextImport;
use codex_core::config::AgentsContextImportResult;
use codex_core::config::AgentsSource;
use codex_core::config::import_agents_context;
use codex_core::config::render_agents_context_preview;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use open::that_detached;
use ratatui::buffer::Buffer;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;
use tokio_stream::StreamExt;
use tracing::error;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
struct DirectoryNode {
    name: String,
    path: String,
    directories: BTreeMap<String, DirectoryNode>,
    files: Vec<FileNode>,
}

#[derive(Debug, Clone)]
struct FileNode {
    name: String,
    path: String,
    source: AgentsSource,
}

#[derive(Debug, Clone)]
struct VisibleItem {
    path: String,
    display_name: String,
    depth: usize,
    variant: VisibleVariant,
}

#[derive(Debug, Clone, Copy)]
enum VisibleVariant {
    Directory { collapsed: bool },
    File { source: AgentsSource },
}

#[derive(Debug, Clone, Copy)]
struct TokenSummary {
    tokens: usize,
    percent_of_window: Option<f64>,
    truncated: bool,
    active_entries: usize,
    total_entries: usize,
}

impl TokenSummary {
    const fn empty() -> Self {
        Self {
            tokens: 0,
            percent_of_window: None,
            truncated: false,
            active_entries: 0,
            total_entries: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusMessage {
    Info(String),
    Error(String),
}

enum OverlayState {
    None,
    Add(AddContextOverlay),
}

struct AddContextOverlay {
    source: LineEditor,
    destination: LineEditor,
    focus: AddField,
    target: AgentsSource,
    message: Option<OverlayMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddField {
    Source,
    Destination,
}

#[derive(Debug, Clone)]
enum OverlayMessage {
    Error(String),
}

#[derive(Debug, Clone)]
struct LineEditor {
    content: String,
    cursor: usize,
}

impl OverlayState {
    fn as_add_mut(&mut self) -> Option<&mut AddContextOverlay> {
        match self {
            OverlayState::Add(overlay) => Some(overlay),
            OverlayState::None => None,
        }
    }

    fn as_add(&self) -> Option<&AddContextOverlay> {
        match self {
            OverlayState::Add(overlay) => Some(overlay),
            OverlayState::None => None,
        }
    }
}

impl AddContextOverlay {
    fn new(default_target: AgentsSource) -> Self {
        Self {
            source: LineEditor::new(),
            destination: LineEditor::new(),
            focus: AddField::Source,
            target: default_target,
            message: None,
        }
    }

    fn reset_message(&mut self) {
        self.message = None;
    }

    fn active_editor_mut(&mut self) -> &mut LineEditor {
        match self.focus {
            AddField::Source => &mut self.source,
            AddField::Destination => &mut self.destination,
        }
    }

    fn move_focus_forward(&mut self) {
        self.focus = match self.focus {
            AddField::Source => AddField::Destination,
            AddField::Destination => AddField::Source,
        };
    }

    fn move_focus_backward(&mut self) {
        self.move_focus_forward();
    }

    fn toggle_target(&mut self, project_available: bool) {
        self.target = match (self.target, project_available) {
            (AgentsSource::Global, true) => AgentsSource::Project,
            (AgentsSource::Project, _) => AgentsSource::Global,
            (AgentsSource::Global, false) => AgentsSource::Global,
        };
    }
}

impl LineEditor {
    fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
        }
    }

    fn char_count(&self) -> usize {
        self.content.chars().count()
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.char_count();
    }

    fn insert(&mut self, ch: char) {
        let idx = self.byte_index(self.cursor);
        self.content.insert(idx, ch);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_index(self.cursor);
        let start = self.byte_index(self.cursor - 1);
        self.content.drain(start..end);
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.content.drain(start..end);
    }

    fn clear(&mut self) {
        self.content.clear();
        self.cursor = 0;
    }

    fn byte_index(&self, char_idx: usize) -> usize {
        if char_idx == 0 {
            return 0;
        }
        for (count, (byte_idx, _)) in self.content.char_indices().enumerate() {
            if count == char_idx {
                return byte_idx;
            }
        }
        self.content.len()
    }

    fn as_str(&self) -> &str {
        &self.content
    }
}

struct IncludeAggregation {
    directories: BTreeSet<String>,
    files: BTreeSet<String>,
    has_enabled: bool,
    all_enabled: bool,
}

struct DisabledAggregation {
    directories: BTreeSet<String>,
    files: BTreeSet<String>,
    all_disabled: bool,
    has_any_disabled: bool,
}

pub(crate) enum AgentsContextManagerOutcome {
    Applied {
        include: Vec<String>,
        exclude: Vec<String>,
    },
    Cancelled,
}

pub(crate) struct AgentsContextManagerConfig<'a> {
    pub tools: Vec<AgentToolEntry>,
    pub warning_tokens: usize,
    pub model_context_window: Option<i64>,
    pub global_agents_home: PathBuf,
    pub project_agents_home: Option<PathBuf>,
    pub cwd: PathBuf,
    pub enabled_paths: HashSet<String>,
    pub hidden_paths: HashSet<String>,
    pub include_mode: bool,
    pub existing_include: &'a [String],
    pub existing_exclude: &'a [String],
}

pub(crate) async fn run_agents_context_manager(
    tui: &mut Tui,
    all_entries: Vec<AgentContextEntry>,
    config: AgentsContextManagerConfig<'_>,
) -> Result<AgentsContextManagerOutcome> {
    let mut screen = AgentsContextManager::new(tui.frame_requester(), all_entries, config);

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&screen, frame.area());
    })?;

    let mut events = tui.event_stream().fuse();

    while !screen.is_done() {
        if let Some(event) = events.next().await {
            match event {
                TuiEvent::Key(key) => screen.handle_key(key),
                TuiEvent::Paste(_) => {}
                TuiEvent::Draw => {
                    tui.draw(u16::MAX, |frame| {
                        frame.render_widget_ref(&screen, frame.area());
                    })?;
                }
            }
        } else {
            break;
        }
    }

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&screen, frame.area());
    })?;

    Ok(screen.outcome())
}

struct AgentsContextManager {
    request_frame: FrameRequester,
    all_entries: Vec<AgentContextEntry>,
    tools: Vec<AgentToolEntry>,
    global_agents_home: PathBuf,
    project_agents_home: Option<PathBuf>,
    cwd: PathBuf,
    warning_tokens: usize,
    model_context_window: Option<i64>,
    tree: DirectoryNode,
    visible: Vec<VisibleItem>,
    highlight: usize,
    collapsed_dirs: HashSet<String>,
    disabled_dirs: HashSet<String>,
    disabled_files: HashSet<String>,
    explicit_include: Vec<String>,
    include_mode: bool,
    agents_rules_paths: Vec<String>,
    agents_rules_disabled: bool,
    scroll_offset: Cell<usize>,
    viewport_entries_height: Cell<usize>,
    token_summary: TokenSummary,
    overlay: OverlayState,
    status_message: Option<StatusMessage>,
    finished: bool,
    cancelled: bool,
    hidden_paths: HashSet<String>,
}

impl AgentsContextManager {
    fn new(
        request_frame: FrameRequester,
        all_entries: Vec<AgentContextEntry>,
        config: AgentsContextManagerConfig,
    ) -> Self {
        let AgentsContextManagerConfig {
            tools,
            warning_tokens,
            model_context_window,
            global_agents_home,
            project_agents_home,
            cwd,
            enabled_paths,
            hidden_paths,
            include_mode,
            existing_include,
            existing_exclude,
        } = config;
        let mut tree = DirectoryNode::root();
        for entry in &all_entries {
            tree.insert_entry(entry);
        }
        tree.sort_recursive();

        let agents_rules_paths: Vec<String> = all_entries
            .iter()
            .filter(|entry| entry.relative_path.ends_with("AGENTS.md"))
            .map(|entry| entry.relative_path.clone())
            .collect();

        let collapsed_default = default_collapsed_dirs(&tree);

        let entry_paths: HashSet<String> = all_entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();

        let mut disabled_dirs = HashSet::new();
        let mut disabled_files = HashSet::new();

        for value in existing_exclude {
            if entry_paths.contains(value) {
                disabled_files.insert(value.clone());
            } else {
                disabled_dirs.insert(value.clone());
            }
        }

        for path in &entry_paths {
            if !enabled_paths.contains(path) {
                disabled_files.insert(path.clone());
            }
        }

        let mut manager = Self {
            request_frame,
            all_entries,
            tools,
            global_agents_home,
            project_agents_home,
            cwd,
            warning_tokens,
            model_context_window,
            tree,
            visible: Vec::new(),
            highlight: 0,
            collapsed_dirs: collapsed_default,
            disabled_dirs,
            disabled_files,
            explicit_include: existing_include.to_vec(),
            include_mode,
            agents_rules_paths,
            agents_rules_disabled: false,
            scroll_offset: Cell::new(0),
            viewport_entries_height: Cell::new(0),
            token_summary: TokenSummary::empty(),
            overlay: OverlayState::None,
            status_message: None,
            finished: false,
            cancelled: false,
            hidden_paths,
        };
        manager.agents_rules_disabled = manager
            .agents_rules_paths
            .iter()
            .all(|path| !manager.is_file_enabled(path));
        manager.rebuild_visible();
        manager
    }

    fn is_done(&self) -> bool {
        self.finished || self.cancelled
    }

    fn outcome(self) -> AgentsContextManagerOutcome {
        if self.cancelled {
            AgentsContextManagerOutcome::Cancelled
        } else {
            let exclude: Vec<String> = if self.include_mode {
                Vec::new()
            } else {
                self.collect_disabled_filters(&self.tree)
                    .into_iter()
                    .collect()
            };
            AgentsContextManagerOutcome::Applied {
                include: self.explicit_include,
                exclude,
            }
        }
    }

    fn handle_key(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release {
            return;
        }

        if self.handle_overlay_key(event) {
            return;
        }

        let visible_len = self.visible.len();
        match (event.modifiers, event.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('c'))
            | (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::CONTROL, KeyCode::Char('g')) => {
                self.cancelled = true;
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.finished = true;
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if self.highlight > 0 {
                    self.highlight -= 1;
                    self.ensure_highlight_visible();
                    self.request_frame.schedule_frame();
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if self.highlight + 1 < visible_len {
                    self.highlight += 1;
                    self.ensure_highlight_visible();
                    self.request_frame.schedule_frame();
                }
            }
            (KeyModifiers::NONE, KeyCode::Left) | (KeyModifiers::NONE, KeyCode::Char('h')) => {
                self.collapse_current();
            }
            (KeyModifiers::NONE, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Char('l')) => {
                self.expand_current();
            }
            (KeyModifiers::NONE, KeyCode::Char(' ')) => {
                self.toggle_current();
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                self.open_add_overlay();
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => {
                self.toggle_agents_rules();
            }
            (KeyModifiers::CONTROL, KeyCode::Enter) => {
                self.finished = true;
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.expand_or_collapse_current() {
                    self.open_current_file();
                }
            }
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, event: KeyEvent) -> bool {
        let Some(overlay) = self.overlay.as_add_mut() else {
            return false;
        };

        if event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(
                event.code,
                KeyCode::Char('c') | KeyCode::Char('d') | KeyCode::Char('g')
            )
        {
            self.cancelled = true;
            return true;
        }

        match (event.modifiers, event.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.overlay = OverlayState::None;
                self.status_message = None;
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.confirm_add_overlay();
                true
            }
            (KeyModifiers::SHIFT, KeyCode::Tab) => {
                overlay.reset_message();
                overlay.move_focus_backward();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Tab) => {
                overlay.reset_message();
                overlay.move_focus_forward();
                self.request_frame.schedule_frame();
                true
            }
            (mods, KeyCode::Char('t')) if mods.contains(KeyModifiers::CONTROL) => {
                let project_available = self.project_agents_home.is_some();
                if project_available {
                    overlay.toggle_target(true);
                    overlay.reset_message();
                    self.status_message = None;
                } else {
                    overlay.message = Some(OverlayMessage::Error(
                        "Project .agents directory not detected.".to_string(),
                    ));
                }
                self.request_frame.schedule_frame();
                true
            }
            (mods, KeyCode::Char('u')) if mods.contains(KeyModifiers::CONTROL) => {
                overlay.reset_message();
                overlay.active_editor_mut().clear();
                self.request_frame.schedule_frame();
                true
            }
            (mods, KeyCode::Char(ch)) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                overlay.reset_message();
                overlay.active_editor_mut().insert(ch);
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                overlay.reset_message();
                overlay.active_editor_mut().backspace();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                overlay.reset_message();
                overlay.active_editor_mut().delete();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                overlay.active_editor_mut().move_left();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                overlay.active_editor_mut().move_right();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::Home) => {
                overlay.active_editor_mut().move_home();
                self.request_frame.schedule_frame();
                true
            }
            (KeyModifiers::NONE, KeyCode::End) => {
                overlay.active_editor_mut().move_end();
                self.request_frame.schedule_frame();
                true
            }
            _ => false,
        }
    }

    fn collapse_current(&mut self) {
        if let Some(item) = self.visible.get(self.highlight)
            && let VisibleVariant::Directory { collapsed } = item.variant
            && !collapsed
        {
            self.collapsed_dirs.insert(item.path.clone());
            self.rebuild_visible();
            self.request_frame.schedule_frame();
        }
    }

    fn expand_current(&mut self) {
        if let Some(item) = self.visible.get(self.highlight)
            && let VisibleVariant::Directory { collapsed } = item.variant
            && collapsed
        {
            self.collapsed_dirs.remove(&item.path);
            self.rebuild_visible();
            self.request_frame.schedule_frame();
        }
    }

    fn toggle_current(&mut self) {
        let Some(item) = self.visible.get(self.highlight).cloned() else {
            return;
        };
        match item.variant {
            VisibleVariant::Directory { .. } => {
                if self.disabled_dirs.remove(&item.path) {
                    self.disabled_files
                        .retain(|path| !path_in_directory(path, &item.path));
                } else {
                    self.disabled_dirs.insert(item.path.clone());
                    self.disabled_files
                        .retain(|path| !path_in_directory(path, &item.path));
                }
            }
            VisibleVariant::File { .. } => {
                if !self.disabled_files.remove(&item.path) {
                    self.disabled_files.insert(item.path.clone());
                }
            }
        }
        self.rebuild_visible();
        self.request_frame.schedule_frame();
    }

    fn rebuild_visible(&mut self) {
        let mut visible = Vec::new();
        collect_visible_items(
            &self.tree,
            0,
            &self.collapsed_dirs,
            &self.hidden_paths,
            &mut visible,
        );
        self.visible = visible;
        if self.highlight >= self.visible.len() && !self.visible.is_empty() {
            self.highlight = self.visible.len() - 1;
        }
        self.update_token_summary();
        self.recompute_explicit_include();
        self.ensure_highlight_visible();

        self.request_frame.schedule_frame();
    }

    fn update_token_summary(&mut self) {
        let active_entries: Vec<AgentContextEntry> = self
            .all_entries
            .iter()
            .filter(|entry| self.is_file_enabled(&entry.relative_path))
            .cloned()
            .collect();

        let render = render_agents_context_preview(
            &active_entries,
            &self.tools,
            self.project_agents_home.as_deref(),
        );

        let percent = self.model_context_window.and_then(|window| {
            if window > 0 {
                Some(render.token_count as f64 / window as f64 * 100.0)
            } else {
                None
            }
        });

        self.token_summary = TokenSummary {
            tokens: render.token_count,
            percent_of_window: percent,
            truncated: render.truncated,
            active_entries: active_entries.len(),
            total_entries: self.all_entries.len(),
        };
    }

    fn recompute_explicit_include(&mut self) {
        if !self.include_mode {
            return;
        }
        let aggregation = self.collect_include_paths(&self.tree);
        let mut paths: BTreeSet<String> = aggregation.directories;
        paths.extend(aggregation.files);
        self.explicit_include = paths.into_iter().collect();
    }

    fn collect_disabled_filters(&self, node: &DirectoryNode) -> BTreeSet<String> {
        let aggregation = self.collect_disabled_paths(node);
        let mut merged: BTreeSet<String> = aggregation.directories;
        merged.extend(aggregation.files);
        merged
    }

    fn collect_disabled_paths(&self, node: &DirectoryNode) -> DisabledAggregation {
        if !node.path.is_empty() && self.disabled_dirs.contains(&node.path) {
            let mut directories = BTreeSet::new();
            directories.insert(node.path.clone());
            return DisabledAggregation {
                directories,
                files: BTreeSet::new(),
                all_disabled: true,
                has_any_disabled: true,
            };
        }

        let mut directories: BTreeSet<String> = BTreeSet::new();
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut all_children_disabled = true;
        let mut any_disabled = false;

        for dir in node.directories.values() {
            let child = self.collect_disabled_paths(dir);
            if child.has_any_disabled {
                any_disabled = true;
            }
            if child.all_disabled && child.has_any_disabled {
                directories.insert(dir.path.clone());
            } else {
                directories.extend(child.directories);
                files.extend(child.files);
            }
            if !child.all_disabled {
                all_children_disabled = false;
            }
        }

        for file in &node.files {
            if self.disabled_files.contains(&file.path) {
                files.insert(file.path.clone());
                any_disabled = true;
            } else {
                all_children_disabled = false;
            }
        }

        if !node.path.is_empty() && all_children_disabled && any_disabled {
            let mut directories = BTreeSet::new();
            directories.insert(node.path.clone());
            return DisabledAggregation {
                directories,
                files: BTreeSet::new(),
                all_disabled: true,
                has_any_disabled: true,
            };
        }

        DisabledAggregation {
            directories,
            files,
            all_disabled: all_children_disabled && !node.path.is_empty() && any_disabled,
            has_any_disabled: any_disabled,
        }
    }

    fn collect_include_paths(&self, node: &DirectoryNode) -> IncludeAggregation {
        let mut child_directories: BTreeSet<String> = BTreeSet::new();
        let mut child_files: BTreeSet<String> = BTreeSet::new();
        let mut any_enabled = false;
        let mut all_children_enabled = true;

        for dir in node.directories.values() {
            let child = self.collect_include_paths(dir);
            if child.has_enabled {
                any_enabled = true;
            }
            if !child.all_enabled {
                all_children_enabled = false;
            }
            child_directories.extend(child.directories);
            child_files.extend(child.files);
        }

        for file in &node.files {
            if self.is_file_enabled(&file.path) {
                any_enabled = true;
                child_files.insert(file.path.clone());
            } else {
                all_children_enabled = false;
            }
        }

        let dir_enabled = node.path.is_empty() || self.is_directory_enabled(&node.path);
        if !dir_enabled {
            return IncludeAggregation {
                directories: BTreeSet::new(),
                files: BTreeSet::new(),
                has_enabled: false,
                all_enabled: false,
            };
        }

        if node.path.is_empty() {
            return IncludeAggregation {
                directories: child_directories,
                files: child_files,
                has_enabled: any_enabled,
                all_enabled: all_children_enabled,
            };
        }

        let all_enabled = all_children_enabled && any_enabled;
        let child_count = child_directories.len() + child_files.len();

        if all_enabled && child_count > 1 {
            let mut directories = BTreeSet::new();
            directories.insert(node.path.clone());
            IncludeAggregation {
                directories,
                files: BTreeSet::new(),
                has_enabled: true,
                all_enabled: true,
            }
        } else {
            IncludeAggregation {
                directories: child_directories,
                files: child_files,
                has_enabled: any_enabled,
                all_enabled,
            }
        }
    }

    fn is_directory_enabled(&self, path: &str) -> bool {
        if self
            .disabled_dirs
            .iter()
            .any(|directory| path_in_directory(path, directory))
        {
            return false;
        }
        !self.disabled_dirs.contains(path)
    }

    fn is_file_enabled(&self, path: &str) -> bool {
        if self
            .disabled_dirs
            .iter()
            .any(|directory| path_in_directory(path, directory))
        {
            return false;
        }
        !self.disabled_files.contains(path)
    }

    fn expand_or_collapse_current(&mut self) -> bool {
        if let Some(item) = self.visible.get(self.highlight)
            && let VisibleVariant::Directory { collapsed } = item.variant
        {
            if collapsed {
                self.collapsed_dirs.remove(&item.path);
            } else {
                self.collapsed_dirs.insert(item.path.clone());
            }
            self.rebuild_visible();
            self.request_frame.schedule_frame();
            return true;
        }
        false
    }

    fn open_current_file(&self) {
        let Some(item) = self.visible.get(self.highlight) else {
            return;
        };
        if !matches!(item.variant, VisibleVariant::File { .. }) {
            return;
        }
        let Some(entry) = self
            .all_entries
            .iter()
            .find(|entry| entry.relative_path == item.path)
        else {
            return;
        };
        if let Err(err) = that_detached(&entry.absolute_path) {
            error!(
                path = %entry.absolute_path.display(),
                ?err,
                "failed to open agents context entry"
            );
        }
    }

    fn toggle_agents_rules(&mut self) {
        if self.agents_rules_paths.is_empty() {
            return;
        }
        if self.agents_rules_disabled {
            for path in &self.agents_rules_paths {
                self.disabled_files.remove(path);
            }
            self.agents_rules_disabled = false;
        } else {
            for path in &self.agents_rules_paths {
                self.disabled_files.insert(path.clone());
            }
            self.agents_rules_disabled = true;
        }
        self.rebuild_visible();
        self.request_frame.schedule_frame();
    }

    fn open_add_overlay(&mut self) {
        if self.overlay.as_add().is_some() {
            return;
        }
        let default_target = if self.project_agents_home.is_some() {
            AgentsSource::Project
        } else {
            AgentsSource::Global
        };
        self.overlay = OverlayState::Add(AddContextOverlay::new(default_target));
        self.status_message = None;
        self.request_frame.schedule_frame();
    }

    fn confirm_add_overlay(&mut self) {
        let (source_text, destination_text, target) = {
            let Some(overlay) = self.overlay.as_add_mut() else {
                return;
            };
            overlay.reset_message();
            let source = overlay.source.as_str().trim().to_string();
            let destination = overlay.destination.as_str().trim().to_string();
            (source, destination, overlay.target)
        };

        if source_text.is_empty() {
            if let Some(overlay) = self.overlay.as_add_mut() {
                overlay.message = Some(OverlayMessage::Error(
                    "Source path is required.".to_string(),
                ));
            }
            self.request_frame.schedule_frame();
            return;
        }

        let import = AgentsContextImport {
            source: PathBuf::from(&source_text),
            target,
            destination_dir: if destination_text.is_empty() {
                None
            } else {
                Some(destination_text)
            },
        };

        match import_agents_context(
            &self.global_agents_home,
            self.project_agents_home.as_deref(),
            &self.cwd,
            import,
        ) {
            Ok(result) => {
                if result.added_entries.is_empty() {
                    let message = "No readable files found at the source path.".to_string();
                    if let Some(overlay) = self.overlay.as_add_mut() {
                        overlay.message = Some(OverlayMessage::Error(message.clone()));
                    }
                    self.status_message = Some(StatusMessage::Error(message));
                } else {
                    let count = result.added_entries.len();
                    let scope = match target {
                        AgentsSource::Global => "global",
                        AgentsSource::Project => "project",
                    };
                    let message = match count {
                        1 => format!("Imported 1 file into {scope} agents context."),
                        _ => format!("Imported {count} files into {scope} agents context."),
                    };
                    self.overlay = OverlayState::None;
                    self.apply_import_result(result);
                    self.status_message = Some(StatusMessage::Info(message));
                }
            }
            Err(err) => {
                let message = err.to_string();
                if let Some(overlay) = self.overlay.as_add_mut() {
                    overlay.message = Some(OverlayMessage::Error(message.clone()));
                }
                self.status_message = Some(StatusMessage::Error(message));
            }
        }

        self.request_frame.schedule_frame();
    }

    fn apply_import_result(&mut self, result: AgentsContextImportResult) {
        for entry in &result.added_entries {
            self.tree.insert_entry(entry);
            self.disabled_files.remove(&entry.relative_path);
            if entry.relative_path.ends_with("AGENTS.md")
                && !self.agents_rules_paths.contains(&entry.relative_path)
            {
                self.agents_rules_paths.push(entry.relative_path.clone());
            }
        }
        self.tree.sort_recursive();
        self.all_entries.extend(result.added_entries.clone());
        self.agents_rules_paths.sort();
        self.agents_rules_paths.dedup();
        self.rebuild_visible();
        if let Some(first) = result.added_entries.first() {
            self.focus_on_path(&first.relative_path);
        }
        self.agents_rules_disabled = self
            .agents_rules_paths
            .iter()
            .all(|path| !self.is_file_enabled(path));
    }

    fn focus_on_path(&mut self, target: &str) {
        if let Some((index, _)) = self
            .visible
            .iter()
            .enumerate()
            .find(|(_, item)| item.path == target)
        {
            self.highlight = index;
            self.ensure_highlight_visible();
        }
    }

    fn render_add_overlay(&self, area: Rect, buf: &mut Buffer, overlay: &AddContextOverlay) {
        if area.width < 20 || area.height < 7 {
            return;
        }

        let available_width = area.width.saturating_sub(6);
        let popup_width = if available_width < 30 {
            area.width.saturating_sub(2)
        } else {
            available_width.clamp(30, 80)
        };
        if popup_width == 0 {
            return;
        }

        let editor_width = popup_width.saturating_sub(4);

        let target_label = match overlay.target {
            AgentsSource::Global => "Global",
            AgentsSource::Project => "Project",
        };
        let target_dir = match overlay.target {
            AgentsSource::Global => self
                .global_agents_home
                .join("context")
                .display()
                .to_string(),
            AgentsSource::Project => self
                .project_agents_home
                .as_ref()
                .map(|home| home.join("context").display().to_string())
                .unwrap_or_else(|| "(project .agents not detected)".to_string()),
        };
        let label = "Target: ";
        let hint = "Ctrl+T toggle scope";
        let gap = "   ";
        let label_width = UnicodeWidthStr::width(label) as u16;
        let hint_width = UnicodeWidthStr::width(hint) as u16;
        let gap_width = UnicodeWidthStr::width(gap) as u16;

        let mut column = ColumnRenderable::new();
        let available_for_path = editor_width
            .saturating_sub(label_width)
            .saturating_sub(gap_width)
            .saturating_sub(hint_width);

        if available_for_path >= 8 {
            let truncated_dir = center_truncate_path(&target_dir, available_for_path as usize);
            column.push(Line::from(vec![
                label.dim(),
                Span::from(format!("{target_label} ({truncated_dir})")),
                gap.into(),
                hint.dim(),
            ]));
        } else {
            let truncated_dir = center_truncate_path(
                &target_dir,
                editor_width
                    .saturating_sub(label_width)
                    .saturating_sub(1)
                    .max(1) as usize,
            );
            column.push(Line::from(vec![
                label.dim(),
                Span::from(format!("{target_label} ({truncated_dir})")),
            ]));
            column.push(Line::from(hint.dim()).inset(Insets::tlbr(0, 1, 0, 0)));
        }
        column.push("");
        column.push(
            render_editor_line(
                "Source",
                &overlay.source,
                overlay.focus == AddField::Source,
                editor_width,
            )
            .inset(Insets::tlbr(0, 1, 0, 0)),
        );
        column.push(
            render_editor_line(
                "Destination (optional)",
                &overlay.destination,
                overlay.focus == AddField::Destination,
                editor_width,
            )
            .inset(Insets::tlbr(0, 1, 0, 0)),
        );
        column.push("");
        column.push(
            Line::from("Files must be <= 256 KiB; each import can add up to 512 files.".dim())
                .inset(Insets::tlbr(0, 1, 0, 0)),
        );
        column.push("");
        column.push(
            Line::from("Enter import    Tab switch field    Ctrl+U clear    Esc cancel".dim())
                .inset(Insets::tlbr(0, 1, 0, 0)),
        );
        if let Some(OverlayMessage::Error(text)) = &overlay.message {
            column.push("");
            column.push(Line::from(Span::from(text.clone()).red()).inset(Insets::tlbr(0, 1, 0, 0)));
        }

        let content_height = column
            .desired_height(editor_width)
            .min(area.height.saturating_sub(4));
        let popup_height = content_height
            .saturating_add(2)
            .min(area.height.saturating_sub(2));
        if popup_height < 5 {
            return;
        }

        let popup = Rect {
            x: area.x + (area.width.saturating_sub(popup_width)) / 2,
            y: area.y + (area.height.saturating_sub(popup_height)) / 2,
            width: popup_width,
            height: popup_height,
        };

        Clear.render(popup, buf);
        Block::default()
            .borders(Borders::ALL)
            .title("Add context entries".bold())
            .render_ref(popup, buf);

        let content_area = popup.inner(Margin::new(1, 1));
        column.render(content_area, buf);
    }

    fn ensure_highlight_visible(&self) {
        if self.visible.is_empty() {
            if self.scroll_offset.get() != 0 {
                self.scroll_offset.set(0);
                self.request_frame.schedule_frame();
            }
            return;
        }

        let viewport = self.viewport_entries_height.get();
        if viewport == 0 {
            if self.scroll_offset.get() != 0 {
                self.scroll_offset.set(0);
                self.request_frame.schedule_frame();
            }
            return;
        }
        let max_offset = self.visible.len().saturating_sub(viewport);
        let mut offset = self.scroll_offset.get().min(max_offset);
        if self.highlight < offset {
            offset = self.highlight;
        } else if self.highlight >= offset + viewport {
            offset = self.highlight + 1 - viewport;
        }
        let offset = offset.min(max_offset);
        if offset != self.scroll_offset.get() {
            self.scroll_offset.set(offset);
            self.request_frame.schedule_frame();
        }
    }

    fn header_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from("Agents context entries".bold().cyan()));

        let summary = &self.token_summary;
        let active = summary.active_entries;
        let total = summary.total_entries;
        let tools = self.tools.len();

        let mut spans = Vec::new();
        spans.push("Files".dim());
        spans.push(format!(" {active}/{total}").into());
        spans.push("   ".into());
        spans.push("Tools".dim());
        spans.push(format!(" {tools}").into());
        spans.push("   ".into());
        spans.push("Usage".dim());

        let tokens_label = format!(" ~{}", format_token_count(summary.tokens));
        if summary.tokens >= self.warning_tokens {
            spans.push(tokens_label.magenta().bold());
        } else {
            spans.push(tokens_label.into());
        }

        if let Some(percent) = summary.percent_of_window {
            spans.push("  (".dim());
            spans.push(format_percent(percent).into());
            spans.push(" of window)".dim());
        }

        lines.push(Line::from(spans));

        if summary.tokens >= self.warning_tokens {
            let threshold = format_token_count(self.warning_tokens);
            lines.push(Line::from(
                format!("Warning: exceeds ~{threshold} token threshold.").magenta(),
            ));
        }

        if summary.truncated {
            lines.push(Line::from(
                "Context truncated to fit the 1 MiB limit.".magenta(),
            ));
        }

        if let Some(message) = &self.status_message {
            let span = match message {
                StatusMessage::Info(text) => Span::from(text.clone()).cyan(),
                StatusMessage::Error(text) => Span::from(text.clone()).red().bold(),
            };
            lines.push(Line::from(vec![span]));
        }

        lines.push(Line::from(""));
        lines
    }

    fn footer_lines(&self) -> [Line<'static>; 2] {
        let hint_spans = vec![
            "  ".into(),
            key_hint::plain(KeyCode::Enter).into(),
            " open / expand  ".dim(),
            key_hint::plain(KeyCode::Esc).into(),
            " apply & exit  ".dim(),
            key_hint::ctrl(KeyCode::Char('c')).into(),
            "/".into(),
            key_hint::ctrl(KeyCode::Char('d')).into(),
            " cancel  ".dim(),
            key_hint::plain(KeyCode::Char(' ')).into(),
            " toggle  ".dim(),
            key_hint::plain(KeyCode::Char('a')).into(),
            " add files  ".dim(),
            key_hint::plain(KeyCode::Char('r')).into(),
            " ignore AGENTS.md  ".dim(),
            key_hint::plain(KeyCode::Left).into(),
            " collapse  ".dim(),
            key_hint::plain(KeyCode::Right).into(),
            " expand".dim(),
        ];
        [Line::from(""), Line::from(hint_spans)]
    }

    fn entry_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.visible.len());
        for (index, item) in self.visible.iter().enumerate() {
            let highlighted = index == self.highlight;
            let line = match item.variant {
                VisibleVariant::Directory { collapsed } => {
                    let enabled = self.is_directory_enabled(&item.path);
                    format_directory_row(
                        &item.display_name,
                        item.depth,
                        enabled,
                        highlighted,
                        collapsed,
                    )
                }
                VisibleVariant::File { source } => {
                    let enabled = self.is_file_enabled(&item.path);
                    format_file_row(&item.display_name, item.depth, source, enabled, highlighted)
                }
            };
            lines.push(line);
        }
        lines
    }
}

impl WidgetRef for &AgentsContextManager {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        if let OverlayState::Add(ref overlay) = self.overlay {
            Clear.render(area, buf);
            self.render_add_overlay(area, buf, overlay);
            return;
        }

        Clear.render(area, buf);
        let total_height = area.height as usize;
        if total_height == 0 {
            return;
        }
        let header = self.header_lines();
        let footer = self.footer_lines();
        let entries = self.entry_lines();

        let header_height = header.len().min(total_height);
        let footer_height = footer
            .len()
            .min(total_height.saturating_sub(header_height).max(0));
        let entries_capacity = total_height
            .saturating_sub(header_height)
            .saturating_sub(footer_height)
            .max(1);

        self.viewport_entries_height
            .set(entries_capacity.min(entries.len().max(1)));
        self.ensure_highlight_visible();

        let offset = self.scroll_offset.get().min(
            entries
                .len()
                .saturating_sub(self.viewport_entries_height.get()),
        );
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(total_height);

        for line in header.into_iter().take(total_height) {
            if lines.len() == total_height {
                break;
            }
            lines.push(line);
        }

        let available_for_entries = total_height.saturating_sub(lines.len());
        let entry_take = available_for_entries.min(self.viewport_entries_height.get());
        for line in entries.iter().skip(offset).take(entry_take) {
            if lines.len() == total_height {
                break;
            }
            lines.push(line.clone());
        }

        if lines.len() < total_height {
            for line in footer.into_iter() {
                if lines.len() == total_height {
                    break;
                }
                lines.push(line);
            }
        }

        while lines.len() < total_height {
            lines.push(Line::default());
        }

        Paragraph::new(lines).render_ref(area, buf);

        if let OverlayState::Add(ref overlay) = self.overlay {
            self.render_add_overlay(area, buf, overlay);
        }
    }
}

impl DirectoryNode {
    fn root() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            directories: BTreeMap::new(),
            files: Vec::new(),
        }
    }

    fn insert_entry(&mut self, entry: &AgentContextEntry) {
        let parts: Vec<&str> = entry.relative_path.split('/').collect();
        let mut current = self;
        for (idx, part) in parts.iter().enumerate() {
            let is_last = idx == parts.len() - 1;
            if is_last {
                current.files.push(FileNode {
                    name: part.to_string(),
                    path: entry.relative_path.clone(),
                    source: entry.source,
                });
            } else {
                let child_path = if current.path.is_empty() {
                    part.to_string()
                } else {
                    format!("{}/{}", current.path, part)
                };
                current = current
                    .directories
                    .entry(part.to_string())
                    .or_insert_with(|| DirectoryNode {
                        name: part.to_string(),
                        path: child_path,
                        directories: BTreeMap::new(),
                        files: Vec::new(),
                    });
            }
        }
    }

    fn sort_recursive(&mut self) {
        for dir in self.directories.values_mut() {
            dir.sort_recursive();
        }
        self.files.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

fn collect_visible_items(
    node: &DirectoryNode,
    depth: usize,
    collapsed_dirs: &HashSet<String>,
    hidden_paths: &HashSet<String>,
    output: &mut Vec<VisibleItem>,
) -> bool {
    let mut has_visible = false;

    for dir in node.directories.values() {
        let collapsed = collapsed_dirs.contains(&dir.path);
        let mut child_buffer: Vec<VisibleItem> = Vec::new();
        let child_visible = if collapsed {
            has_visible_descendants(dir, hidden_paths)
        } else {
            collect_visible_items(
                dir,
                depth + 1,
                collapsed_dirs,
                hidden_paths,
                &mut child_buffer,
            )
        };

        if !child_visible {
            continue;
        }

        has_visible = true;
        output.push(VisibleItem {
            path: dir.path.clone(),
            display_name: dir.name.clone(),
            depth,
            variant: VisibleVariant::Directory { collapsed },
        });

        if !collapsed {
            output.extend(child_buffer);
        }
    }

    for file in &node.files {
        if hidden_paths.contains(&file.path) {
            continue;
        }
        has_visible = true;
        output.push(VisibleItem {
            path: file.path.clone(),
            display_name: file.name.clone(),
            depth,
            variant: VisibleVariant::File {
                source: file.source,
            },
        });
    }

    has_visible
}

fn has_visible_descendants(node: &DirectoryNode, hidden_paths: &HashSet<String>) -> bool {
    if node
        .files
        .iter()
        .any(|file| !hidden_paths.contains(&file.path))
    {
        return true;
    }

    node.directories
        .values()
        .any(|dir| has_visible_descendants(dir, hidden_paths))
}

fn render_editor_line(
    label: &str,
    editor: &LineEditor,
    focused: bool,
    width: u16,
) -> Line<'static> {
    let label_text = format!("{label}: ");
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::from(label_text.clone()).dim());

    let available = width.saturating_sub(label_text.len() as u16) as usize;
    if available == 0 {
        spans.push(match focused {
            true => Span::from("▏").cyan(),
            false => Span::from("▏").dim(),
        });
        return Line::from(spans);
    }

    let (before, after, left_trimmed, right_trimmed) = compute_editor_segments(editor, available);

    if left_trimmed {
        spans.push("…".dim());
    }

    if !before.is_empty() {
        let span = if focused {
            Span::from(before).bold()
        } else {
            Span::from(before)
        };
        spans.push(span);
    }

    spans.push(match focused {
        true => Span::from("▏").cyan(),
        false => Span::from("▏").dim(),
    });

    if !after.is_empty() {
        let span = if focused {
            Span::from(after).bold()
        } else {
            Span::from(after)
        };
        spans.push(span);
    }

    if right_trimmed {
        spans.push("…".dim());
    }

    Line::from(spans)
}

fn compute_editor_segments(editor: &LineEditor, available: usize) -> (String, String, bool, bool) {
    if available <= 1 {
        return (String::new(), String::new(), false, false);
    }
    let caret_space = 1;
    let max_visible = available - caret_space;
    let chars: Vec<char> = editor.as_str().chars().collect();
    let cursor = editor.cursor.min(chars.len());

    let mut start = cursor.saturating_sub(max_visible);
    let mut end = (start + max_visible).min(chars.len());

    if end < cursor {
        end = cursor;
    }
    if end - start > max_visible {
        start = end - max_visible;
    }

    let before: String = chars[start..cursor].iter().collect();
    let after: String = chars[cursor..end].iter().collect();
    let left_trimmed = start > 0;
    let right_trimmed = end < chars.len();
    (before, after, left_trimmed, right_trimmed)
}

fn format_directory_row(
    display_name: &str,
    depth: usize,
    enabled: bool,
    highlighted: bool,
    collapsed: bool,
) -> Line<'static> {
    let marker = if highlighted {
        "›".cyan()
    } else {
        "  ".into()
    };
    let checkbox = if enabled { "[x]".into() } else { "[ ]".dim() };
    let indent = "  ".repeat(depth);
    let collapse_indicator = if collapsed {
        "▸".into()
    } else {
        "▾".into()
    };
    let name = format!("{display_name}/");
    Line::from(vec![
        marker,
        " ".into(),
        checkbox,
        "  ".into(),
        indent.into(),
        collapse_indicator,
        " ".into(),
        name.bold(),
    ])
}

fn format_file_row(
    display_name: &str,
    depth: usize,
    source: AgentsSource,
    enabled: bool,
    highlighted: bool,
) -> Line<'static> {
    let marker = if highlighted {
        "›".cyan()
    } else {
        "  ".into()
    };
    let checkbox = if enabled { "[x]".into() } else { "[ ]".dim() };
    let indent = "  ".repeat(depth + 1);
    let name = display_name.to_string();
    let source_label = match source {
        AgentsSource::Global => "(global)".dim(),
        AgentsSource::Project => "(project)".dim(),
    };
    Line::from(vec![
        marker,
        " ".into(),
        checkbox,
        "  ".into(),
        indent.into(),
        name.into(),
        "  ".into(),
        source_label,
    ])
}

fn path_in_directory(path: &str, directory: &str) -> bool {
    if directory.is_empty() {
        return true;
    }
    path == directory
        || path
            .strip_prefix(directory)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .is_some()
}

fn default_collapsed_dirs(tree: &DirectoryNode) -> HashSet<String> {
    let mut collapsed = HashSet::new();
    collect_default_collapsed(tree, &mut collapsed);
    collapsed
}

fn collect_default_collapsed(node: &DirectoryNode, output: &mut HashSet<String>) {
    for dir in node.directories.values() {
        if !dir.path.is_empty() {
            output.insert(dir.path.clone());
        }
        collect_default_collapsed(dir, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn path_in_directory_matches_nested() {
        assert!(path_in_directory("notes/a.md", "notes"));
        assert!(path_in_directory("notes/sub/a.md", "notes"));
        assert!(path_in_directory("notes/sub/a.md", "notes/sub"));
        assert!(!path_in_directory("other/a.md", "notes"));
    }

    fn entry(path: &str) -> AgentContextEntry {
        AgentContextEntry {
            source: AgentsSource::Global,
            relative_path: path.to_string(),
            absolute_path: PathBuf::from(path),
            content: format!("Content for {path}"),
            truncated: false,
        }
    }

    fn manager_with_include(
        entries: Vec<AgentContextEntry>,
        enabled_paths: HashSet<String>,
        include: &[String],
        hidden: &[&str],
    ) -> AgentsContextManager {
        let hidden_paths: HashSet<String> = hidden
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let config = AgentsContextManagerConfig {
            tools: Vec::new(),
            warning_tokens: 50_000,
            model_context_window: None,
            global_agents_home: PathBuf::from("/tmp/global"),
            project_agents_home: None,
            cwd: PathBuf::from("/workspace"),
            enabled_paths,
            hidden_paths,
            include_mode: !include.is_empty(),
            existing_include: include,
            existing_exclude: &[],
        };
        AgentsContextManager::new(FrameRequester::test_dummy(), entries, config)
    }

    fn highlight_path(manager: &mut AgentsContextManager, target: &str) {
        if let Some(idx) = manager.visible.iter().position(|item| item.path == target) {
            manager.highlight = idx;
        } else {
            panic!("path {target} not found");
        }
    }

    fn expand_all(manager: &mut AgentsContextManager) {
        manager.collapsed_dirs.clear();
        manager.rebuild_visible();
    }

    #[test]
    fn enabling_file_extends_include_filters() {
        let entries = vec![entry("notes/guide.md"), entry("notes/tips.md")];
        let mut enabled = HashSet::new();
        enabled.insert("notes/guide.md".to_string());
        let include = vec!["notes/guide.md".to_string()];
        let mut manager = manager_with_include(entries, enabled, &include, &[]);

        expand_all(&mut manager);
        highlight_path(&mut manager, "notes/tips.md");
        manager.toggle_current();

        if let AgentsContextManagerOutcome::Applied { include, exclude } = manager.outcome() {
            assert!(exclude.is_empty());
            assert_eq!(include, vec!["notes".to_string()]);
        } else {
            panic!("manager unexpectedly cancelled");
        }
    }

    #[test]
    fn disabling_file_removes_from_include_filters() {
        let entries = vec![entry("notes/guide.md"), entry("notes/tips.md")];
        let mut enabled = HashSet::new();
        enabled.insert("notes/guide.md".to_string());
        enabled.insert("notes/tips.md".to_string());
        let include = vec!["notes/guide.md".to_string(), "notes/tips.md".to_string()];
        let mut manager = manager_with_include(entries, enabled, &include, &[]);

        expand_all(&mut manager);
        highlight_path(&mut manager, "notes/guide.md");
        manager.toggle_current();

        if let AgentsContextManagerOutcome::Applied { include, exclude } = manager.outcome() {
            assert_eq!(include, vec!["notes/tips.md".to_string()]);
            assert!(exclude.is_empty());
        } else {
            panic!("manager unexpectedly cancelled");
        }
    }

    #[test]
    fn disabling_entire_directory_compacts_exclude_filters() {
        let entries = vec![
            entry("notes/guide.md"),
            entry("notes/tips.md"),
            entry("notes/deep/reference.md"),
        ];
        let mut enabled = HashSet::new();
        for entry in &entries {
            enabled.insert(entry.relative_path.clone());
        }
        let config = AgentsContextManagerConfig {
            tools: Vec::new(),
            warning_tokens: 50_000,
            model_context_window: None,
            global_agents_home: PathBuf::from("/tmp/global"),
            project_agents_home: None,
            cwd: PathBuf::from("/workspace"),
            enabled_paths: enabled,
            hidden_paths: HashSet::new(),
            include_mode: false,
            existing_include: &[],
            existing_exclude: &[],
        };
        let mut manager = AgentsContextManager::new(FrameRequester::test_dummy(), entries, config);

        expand_all(&mut manager);
        for path in ["notes/guide.md", "notes/tips.md", "notes/deep/reference.md"] {
            highlight_path(&mut manager, path);
            manager.toggle_current();
        }

        match manager.outcome() {
            AgentsContextManagerOutcome::Applied { include, exclude } => {
                assert!(include.is_empty());
                assert_eq!(exclude, vec!["notes".to_string()]);
            }
            AgentsContextManagerOutcome::Cancelled => panic!("manager unexpectedly cancelled"),
        }
    }

    #[test]
    fn toggle_agents_rules_disables_and_enables_files() {
        let entries = vec![entry("AGENTS.md"), entry("notes/guide.md")];
        let mut enabled = HashSet::new();
        enabled.insert("AGENTS.md".to_string());
        enabled.insert("notes/guide.md".to_string());
        let mut manager = manager_with_include(entries, enabled, &[], &[]);

        assert!(manager.is_file_enabled("AGENTS.md"));
        assert!(!manager.agents_rules_disabled);

        manager.toggle_agents_rules();
        assert!(manager.agents_rules_disabled);
        assert!(!manager.is_file_enabled("AGENTS.md"));

        manager.toggle_agents_rules();
        assert!(!manager.agents_rules_disabled);
        assert!(manager.is_file_enabled("AGENTS.md"));
    }

    #[test]
    fn include_mode_retains_nested_file_path() {
        let entries = vec![entry("notes/guide.md"), entry("notes/deep/tips.md")];
        let manager_config = AgentsContextManagerConfig {
            tools: Vec::new(),
            warning_tokens: 50_000,
            model_context_window: None,
            global_agents_home: PathBuf::from("/tmp/global"),
            project_agents_home: None,
            cwd: PathBuf::from("/workspace"),
            enabled_paths: HashSet::new(),
            hidden_paths: HashSet::new(),
            include_mode: true,
            existing_include: &[],
            existing_exclude: &[],
        };
        let mut manager =
            AgentsContextManager::new(FrameRequester::test_dummy(), entries, manager_config);

        expand_all(&mut manager);
        highlight_path(&mut manager, "notes/deep/tips.md");
        manager.toggle_current();

        match manager.outcome() {
            AgentsContextManagerOutcome::Applied { include, exclude } => {
                assert!(exclude.is_empty());
                assert_eq!(include, vec!["notes/deep/tips.md".to_string()]);
            }
            AgentsContextManagerOutcome::Cancelled => panic!("manager unexpectedly cancelled"),
        }
    }

    #[test]
    fn include_mode_preserves_single_file_path() {
        let entries = vec![entry("notes/guide.md")];
        let manager_config = AgentsContextManagerConfig {
            tools: Vec::new(),
            warning_tokens: 50_000,
            model_context_window: None,
            global_agents_home: PathBuf::from("/tmp/global"),
            project_agents_home: None,
            cwd: PathBuf::from("/workspace"),
            enabled_paths: HashSet::new(),
            hidden_paths: HashSet::new(),
            include_mode: true,
            existing_include: &[],
            existing_exclude: &[],
        };
        let mut manager =
            AgentsContextManager::new(FrameRequester::test_dummy(), entries, manager_config);

        expand_all(&mut manager);
        highlight_path(&mut manager, "notes/guide.md");
        manager.toggle_current();

        match manager.outcome() {
            AgentsContextManagerOutcome::Applied { include, exclude } => {
                assert!(exclude.is_empty());
                assert_eq!(include, vec!["notes/guide.md".to_string()]);
            }
            AgentsContextManagerOutcome::Cancelled => panic!("manager unexpectedly cancelled"),
        }
    }

    #[test]
    fn hidden_entries_are_not_rendered() {
        let entries = vec![entry("notes/guide.md"), entry("notes/tips.md")];
        let mut enabled = HashSet::new();
        enabled.insert("notes/guide.md".to_string());
        let mut manager = manager_with_include(entries, enabled, &[], &["notes/guide.md"]);

        expand_all(&mut manager);

        assert!(manager.visible.iter().any(|item| item.path == "notes"));
        assert!(
            manager
                .visible
                .iter()
                .any(|item| item.path == "notes/tips.md")
        );
        assert!(
            manager
                .visible
                .iter()
                .all(|item| item.path != "notes/guide.md")
        );
    }

    #[test]
    fn include_mode_collapses_nested_directory_with_multiple_files() {
        let entries = vec![
            entry("notes/deep/tips.md"),
            entry("notes/deep/guide.md"),
            entry("notes/summary.md"),
        ];
        let manager_config = AgentsContextManagerConfig {
            tools: Vec::new(),
            warning_tokens: 50_000,
            model_context_window: None,
            global_agents_home: PathBuf::from("/tmp/global"),
            project_agents_home: None,
            cwd: PathBuf::from("/workspace"),
            enabled_paths: HashSet::new(),
            hidden_paths: HashSet::new(),
            include_mode: true,
            existing_include: &[],
            existing_exclude: &[],
        };
        let mut manager =
            AgentsContextManager::new(FrameRequester::test_dummy(), entries, manager_config);

        expand_all(&mut manager);
        highlight_path(&mut manager, "notes/deep/guide.md");
        manager.toggle_current();
        highlight_path(&mut manager, "notes/deep/tips.md");
        manager.toggle_current();

        match manager.outcome() {
            AgentsContextManagerOutcome::Applied { include, exclude } => {
                assert!(exclude.is_empty());
                assert_eq!(include, vec!["notes/deep".to_string()]);
            }
            AgentsContextManagerOutcome::Cancelled => {
                panic!("manager unexpectedly cancelled")
            }
        }
    }
}
