use std::cell::Cell as CountCell;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ScrollState;
use codex_core::UnifiedExecOutputWindow;
use codex_core::UnifiedExecSessionOutput;
use codex_core::protocol::UnifiedExecSessionState;
use codex_core::protocol::UnifiedExecSessionStatus;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Cell;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Row;
use ratatui::widgets::StatefulWidget;
use ratatui::widgets::Table;
use ratatui::widgets::TableState;
use ratatui::widgets::Wrap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessStatus {
    Running,
    Exited,
}

impl ProcessStatus {
    fn label(self) -> &'static str {
        match self {
            ProcessStatus::Running => "Running",
            ProcessStatus::Exited => "Exited",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            ProcessStatus::Running => "●",
            ProcessStatus::Exited => "○",
        }
    }

    fn style(self) -> Style {
        match self {
            ProcessStatus::Running => Style::default().green(),
            ProcessStatus::Exited => Style::default().dim(),
        }
    }
}

impl From<UnifiedExecSessionStatus> for ProcessStatus {
    fn from(value: UnifiedExecSessionStatus) -> Self {
        match value {
            UnifiedExecSessionStatus::Running => ProcessStatus::Running,
            UnifiedExecSessionStatus::Exited => ProcessStatus::Exited,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessManagerEntry {
    pub session_id: i32,
    pub status: ProcessStatus,
    pub command: Vec<String>,
    pub started_at: SystemTime,
    pub last_output_at: Option<SystemTime>,
    pub preview: String,
    pub preview_truncated: bool,
}

impl ProcessManagerEntry {
    pub fn from_snapshot(snapshot: codex_core::UnifiedExecSessionSnapshot) -> Self {
        let state: UnifiedExecSessionState = snapshot.into();
        Self::from_state(state)
    }

    pub fn from_state(state: UnifiedExecSessionState) -> Self {
        let status = ProcessStatus::from(state.status);
        Self {
            session_id: state.session_id,
            status,
            command: state.command,
            started_at: unix_epoch_plus(state.started_at_ms),
            last_output_at: state.last_output_at_ms.map(unix_epoch_plus),
            preview: state.output_preview,
            preview_truncated: state.output_truncated,
        }
    }
}

pub(crate) fn entry_and_data_from_output(
    output: UnifiedExecSessionOutput,
) -> (ProcessManagerEntry, ProcessOutputData) {
    let UnifiedExecSessionOutput {
        session_id,
        command,
        started_at,
        last_output_at,
        status,
        content,
        truncated,
        truncated_suffix,
        expandable_prefix,
        expandable_suffix,
        range_start,
        range_end,
        total_bytes,
        window_bytes,
    } = output;

    let status = ProcessStatus::from(status);
    let (preview, preview_truncated) = clip_preview(&content);

    let entry = ProcessManagerEntry {
        session_id,
        status,
        command,
        started_at,
        last_output_at,
        preview,
        preview_truncated: preview_truncated
            || truncated
            || truncated_suffix
            || expandable_prefix
            || expandable_suffix,
    };

    let data = ProcessOutputData {
        content,
        range_start,
        range_end,
        total_bytes,
        truncated_prefix: truncated,
        truncated_suffix,
        expandable_prefix,
        expandable_suffix,
        window_bytes,
    };

    (entry, data)
}

const FALLBACK_PREVIEW_MAX_CHARS: usize = 4 * 1024;

fn clip_preview(raw: &str) -> (String, bool) {
    if raw.len() <= FALLBACK_PREVIEW_MAX_CHARS {
        return (raw.to_string(), false);
    }

    let mut buf = String::with_capacity(FALLBACK_PREVIEW_MAX_CHARS + 32);
    for ch in raw
        .chars()
        .take(FALLBACK_PREVIEW_MAX_CHARS.saturating_sub(1))
    {
        buf.push(ch);
    }
    buf.push('…');
    buf.push_str("\n(log truncated)");
    (buf, true)
}

fn unix_epoch_plus(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn summarize_command(command: &[String]) -> String {
    if command.is_empty() {
        return "(detached)".to_string();
    }
    command.join(" ")
}

fn format_elapsed(time: SystemTime) -> String {
    match SystemTime::now().duration_since(time) {
        Ok(duration) => format!("{} ago", format_duration_short(duration)),
        Err(_) => "just now".to_string(),
    }
}

fn format_optional_elapsed(time: Option<SystemTime>) -> String {
    match time {
        Some(value) => format_elapsed(value),
        None => "—".to_string(),
    }
}

fn format_duration_short(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        return "<1s".to_string();
    }
    if duration < Duration::from_secs(1) {
        return format!("{}ms", duration.as_millis());
    }
    if duration < Duration::from_secs(60) {
        return format!("{}s", duration.as_secs());
    }
    if duration < Duration::from_secs(3_600) {
        let minutes = duration.as_secs() / 60;
        let seconds = duration.as_secs() % 60;
        if seconds == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m{seconds:02}s");
    }
    if duration < Duration::from_secs(86_400) {
        let hours = duration.as_secs() / 3_600;
        let minutes = (duration.as_secs() % 3_600) / 60;
        if minutes == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h{minutes:02}m");
    }
    let days = duration.as_secs() / 86_400;
    let hours = (duration.as_secs() % 86_400) / 3_600;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{hours:02}h")
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes}{}", UNITS[idx])
    } else if value >= 10.0 {
        format!("{value:.0}{}", UNITS[idx])
    } else {
        format!("{value:.1}{}", UNITS[idx])
    }
}

pub(crate) struct ProcessManagerInit {
    pub app_event_tx: AppEventSender,
    pub entries: Vec<ProcessManagerEntry>,
}

pub(crate) struct ProcessManagerView {
    app_event_tx: AppEventSender,
    entries: Vec<ProcessManagerEntry>,
    scroll: ScrollState,
    visible_rows: CountCell<usize>,
    close_requested: bool,
}

impl ProcessManagerView {
    pub(crate) fn new(init: ProcessManagerInit) -> Self {
        let mut scroll = ScrollState::new();
        scroll.clamp_selection(init.entries.len());
        Self {
            app_event_tx: init.app_event_tx,
            entries: init.entries,
            scroll,
            visible_rows: CountCell::new(1),
            close_requested: false,
        }
    }

    pub(crate) fn set_entries(&mut self, entries: Vec<ProcessManagerEntry>) {
        self.entries = entries;
        self.scroll.clamp_selection(self.entries.len());
        self.scroll
            .ensure_visible(self.entries.len(), self.visible_rows.get().max(1));
    }

    fn selected_entry(&self) -> Option<&ProcessManagerEntry> {
        self.scroll
            .selected_idx
            .and_then(|idx| self.entries.get(idx))
    }

    fn visible_rows(&self) -> usize {
        self.visible_rows.get().max(1)
    }

    fn move_selection_up(&mut self) {
        self.scroll.move_up_wrap(self.entries.len());
        self.scroll
            .ensure_visible(self.entries.len(), self.visible_rows());
    }

    fn move_selection_down(&mut self) {
        self.scroll.move_down_wrap(self.entries.len());
        self.scroll
            .ensure_visible(self.entries.len(), self.visible_rows());
    }

    fn page_up(&mut self) {
        let steps = self.visible_rows().saturating_sub(1);
        for _ in 0..steps {
            self.scroll.move_up_wrap(self.entries.len());
        }
        self.scroll
            .ensure_visible(self.entries.len(), self.visible_rows());
    }

    fn page_down(&mut self) {
        let steps = self.visible_rows().saturating_sub(1);
        for _ in 0..steps {
            self.scroll.move_down_wrap(self.entries.len());
        }
        self.scroll
            .ensure_visible(self.entries.len(), self.visible_rows());
    }

    fn send_app_event(&self, event: AppEvent) {
        self.app_event_tx.send(event);
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 3 {
            let message = Paragraph::new(Line::from("Not enough space"));
            message.render(area, buf);
            return;
        }

        let mut rows: Vec<Row> = self
            .entries
            .iter()
            .map(|entry| {
                let status_text = format!("{} {}", entry.status.icon(), entry.status.label());
                Row::new(vec![
                    Cell::from(entry.session_id.to_string()),
                    Cell::from(status_text).style(entry.status.style()),
                    Cell::from(summarize_command(&entry.command)),
                    Cell::from(format_elapsed(entry.started_at)),
                    Cell::from(format_optional_elapsed(entry.last_output_at)),
                ])
            })
            .collect();

        if rows.is_empty() {
            rows.push(
                Row::new(vec![
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from("(no active sessions)").style(Style::default().dim()),
                    Cell::from(""),
                    Cell::from(""),
                ])
                .style(Style::default()),
            );
        }

        let mut state = TableState::default();
        state.select(self.scroll.selected_idx);
        *state.offset_mut() = self.scroll.scroll_top;

        let header = Row::new(vec![
            "Session".bold(),
            "Status".bold(),
            "Command".bold(),
            "Started".bold(),
            "Last output".bold(),
        ]);
        let table = Table::new(
            rows,
            [
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Percentage(50),
                Constraint::Length(16),
                Constraint::Length(16),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Unified Exec Sessions"),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        StatefulWidget::render(table, area, buf, &mut state);

        let visible_rows = area.height.saturating_sub(3) as usize;
        self.visible_rows.set(visible_rows.max(1));
    }

    fn render_preview(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        let details = self.selected_entry().map(|entry| {
            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(format!(
                "Command: {}",
                summarize_command(&entry.command)
            )));
            lines.push(Line::from(format!("Status: {}", entry.status.label())));
            lines.push(Line::from(format!(
                "Started: {}",
                format_elapsed(entry.started_at)
            )));
            lines.push(Line::from(format!(
                "Last output: {}",
                format_optional_elapsed(entry.last_output_at)
            )));
            if entry.preview.trim().is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from("No output captured yet."));
            } else {
                lines.push(Line::from(""));
                for line in entry.preview.lines() {
                    lines.push(Line::from(line.to_owned()));
                }
                if entry.preview_truncated {
                    lines.push(Line::from(""));
                    lines.push(Line::from(
                        "Output truncated — press o to open the full log."
                            .cyan()
                            .italic(),
                    ));
                }
            }
            Text::from(lines)
        });

        let details =
            details.unwrap_or_else(|| Text::from("Select a session to view recent output."));

        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Output preview"),
            )
            .render(area, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let hint = "↑/↓ navigate  •  Open log (o)  •  Input (i)  •  Kill (k)  •  Remove (d)  •  Refresh (r)  •  Esc close";
        Paragraph::new(Line::from(hint.dim())).render(area, buf);
    }
}

impl BottomPaneView for ProcessManagerView {
    fn handle_key_event(&mut self, key_event: crossterm::event::KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.close_requested = true;
            }
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.close_requested = true;
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.move_selection_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.move_selection_down(),
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => self.page_up(),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => self.page_down(),
            KeyEvent {
                code: KeyCode::Char('r'),
                ..
            } => {
                self.send_app_event(AppEvent::OpenProcessManager);
            }
            KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } => {
                if let Some(entry) = self.selected_entry() {
                    self.send_app_event(AppEvent::KillUnifiedExecSession {
                        session_id: entry.session_id,
                    });
                }
            }
            KeyEvent {
                code: KeyCode::Char('d'),
                ..
            } => {
                if let Some(entry) = self.selected_entry() {
                    self.send_app_event(AppEvent::RemoveUnifiedExecSession {
                        session_id: entry.session_id,
                    });
                }
            }
            KeyEvent {
                code: KeyCode::Char('i'),
                ..
            } => {
                if let Some(entry) = self.selected_entry() {
                    self.send_app_event(AppEvent::OpenUnifiedExecInputPrompt {
                        session_id: entry.session_id,
                    });
                }
            }
            KeyEvent {
                code: KeyCode::Char('o'),
                ..
            } => {
                if let Some(entry) = self.selected_entry() {
                    self.send_app_event(AppEvent::OpenUnifiedExecOutput {
                        session_id: entry.session_id,
                    });
                }
            }
            _ => {}
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close_requested = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.close_requested
    }
}

impl crate::render::renderable::Renderable for ProcessManagerView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 9 || area.width < 10 {
            Paragraph::new("Unified exec manager").render(area, buf);
            return;
        }

        let preview_height = 7;
        let footer_height = 1;
        let list_height = area
            .height
            .saturating_sub(preview_height + footer_height)
            .max(3);
        let layout = Layout::vertical([
            Constraint::Length(list_height),
            Constraint::Length(preview_height),
            Constraint::Length(footer_height),
        ])
        .split(area);

        self.render_list(layout[0], buf);
        self.render_preview(layout[1], buf);
        self.render_footer(layout[2], buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        let list_rows = (self.entries.len() as u16).saturating_add(3).min(12);
        list_rows + 4
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessOutputData {
    pub content: String,
    pub range_start: u64,
    pub range_end: u64,
    pub total_bytes: u64,
    pub truncated_prefix: bool,
    pub truncated_suffix: bool,
    pub expandable_prefix: bool,
    pub expandable_suffix: bool,
    pub window_bytes: usize,
}

pub(crate) struct ProcessOutputInit {
    pub entry: ProcessManagerEntry,
    pub data: ProcessOutputData,
    pub app_event_tx: AppEventSender,
}

pub(crate) struct ProcessOutputView {
    entry: ProcessManagerEntry,
    data: ProcessOutputData,
    lines: Vec<String>,
    offset: CountCell<usize>,
    visible_rows: CountCell<usize>,
    close_requested: bool,
    app_event_tx: AppEventSender,
}

const PROCESS_OUTPUT_WINDOW_MAX_BYTES: usize = 2 * 1024 * 1024;
const PROCESS_OUTPUT_EXPANSION_STEP: usize = 64 * 1024;

impl ProcessOutputView {
    const FOOTER_ROWS: u16 = 1;

    pub(crate) fn new(mut init: ProcessOutputInit) -> Self {
        init.data.window_bytes = init.data.window_bytes.max(1);
        let mut view = Self {
            entry: init.entry,
            data: init.data,
            lines: Vec::new(),
            offset: CountCell::new(0),
            visible_rows: CountCell::new(1),
            close_requested: false,
            app_event_tx: init.app_event_tx,
        };
        view.rebuild_lines();
        view
    }

    pub(crate) fn session_id(&self) -> i32 {
        self.entry.session_id
    }

    pub(crate) fn update_entry(&mut self, entry: ProcessManagerEntry) {
        self.entry = entry;
    }

    pub(crate) fn set_output_data(&mut self, mut data: ProcessOutputData) {
        let was_at_bottom = self.is_at_bottom();
        data.window_bytes = data.window_bytes.max(1);
        self.data = data;
        self.rebuild_lines();
        if was_at_bottom {
            self.jump_to_bottom();
        } else {
            self.clamp_offset();
        }
    }

    fn rebuild_lines(&mut self) {
        let content = self.data.content.replace("\r\n", "\n");
        self.lines = build_output_lines(&content);
        self.data.content = content;
    }

    fn visible_rows(&self) -> usize {
        self.visible_rows.get().max(1)
    }

    fn total_rows(&self) -> usize {
        self.lines.len()
    }

    fn current_offset(&self) -> usize {
        self.offset.get()
    }

    fn set_offset(&self, value: usize) {
        self.offset.set(value);
    }

    fn summary_line_count(&self) -> usize {
        let mut count = 5; // command, status, started, last output, window
        if self.data.total_bytes == 0 {
            count += 1;
        }
        if self.data.expandable_prefix || self.data.truncated_prefix {
            count += 1;
        }
        if self.data.truncated_suffix {
            count += 1;
        }
        // window cap hint
        count += 1;
        count
    }

    fn summary_height(&self) -> u16 {
        self.summary_line_count() as u16 + 2
    }

    fn clamp_offset(&self) {
        let total = self.total_rows();
        if total == 0 {
            self.set_offset(0);
            return;
        }
        let max_offset = total.saturating_sub(self.visible_rows());
        if self.current_offset() > max_offset {
            self.set_offset(max_offset);
        }
    }

    fn is_at_bottom(&self) -> bool {
        if self.total_rows() == 0 {
            true
        } else {
            let last_visible = self.current_offset() + self.visible_rows();
            last_visible >= self.total_rows()
        }
    }

    fn scroll_up(&mut self) {
        if self.current_offset() > 0 {
            self.set_offset(self.current_offset() - 1);
        }
    }

    fn scroll_down(&mut self) {
        if self.current_offset() + self.visible_rows() < self.total_rows() {
            self.set_offset(self.current_offset() + 1);
        }
    }

    fn page_up(&mut self) {
        let step = self.visible_rows().saturating_sub(1);
        self.set_offset(self.current_offset().saturating_sub(step));
    }

    fn page_down(&mut self) {
        let step = self.visible_rows().saturating_sub(1);
        let max_offset = self.total_rows().saturating_sub(self.visible_rows());
        self.set_offset((self.current_offset() + step).min(max_offset));
    }

    fn jump_to_top(&mut self) {
        self.set_offset(0);
    }

    fn jump_to_bottom(&mut self) {
        self.set_offset(self.total_rows().saturating_sub(self.visible_rows()));
    }

    fn window_for_expand_backwards(&self) -> Option<UnifiedExecOutputWindow> {
        if !self.data.expandable_prefix {
            return None;
        }
        let anchor_end = self.data.range_end;
        let new_window_bytes = self
            .data
            .window_bytes
            .saturating_add(PROCESS_OUTPUT_EXPANSION_STEP)
            .min(PROCESS_OUTPUT_WINDOW_MAX_BYTES);
        if anchor_end == 0 {
            return None;
        }
        if new_window_bytes == self.data.window_bytes && self.data.range_start == 0 {
            return None;
        }
        let new_start = anchor_end.saturating_sub(new_window_bytes as u64);
        if new_start == self.data.range_start && new_window_bytes == self.data.window_bytes {
            return None;
        }
        Some(UnifiedExecOutputWindow::Range {
            start: new_start,
            max_bytes: new_window_bytes,
        })
    }

    fn window_for_shift_forward(&self) -> Option<UnifiedExecOutputWindow> {
        if !self.data.expandable_suffix {
            return None;
        }
        if self.data.window_bytes == 0 {
            return None;
        }
        let max_start = self
            .data
            .total_bytes
            .saturating_sub(self.data.window_bytes as u64);
        let mut new_start = self
            .data
            .range_start
            .saturating_add(PROCESS_OUTPUT_EXPANSION_STEP as u64);
        if new_start > max_start {
            new_start = max_start;
        }
        if new_start == self.data.range_start {
            return None;
        }
        Some(UnifiedExecOutputWindow::Range {
            start: new_start,
            max_bytes: self.data.window_bytes,
        })
    }

    fn request_expand_backwards(&self) {
        if let Some(window) = self.window_for_expand_backwards() {
            self.app_event_tx
                .send(AppEvent::LoadUnifiedExecOutputWindow {
                    session_id: self.entry.session_id,
                    window,
                });
        }
    }

    fn request_shift_forward(&self) {
        if let Some(window) = self.window_for_shift_forward() {
            self.app_event_tx
                .send(AppEvent::LoadUnifiedExecOutputWindow {
                    session_id: self.entry.session_id,
                    window,
                });
        }
    }

    fn request_export(&self) {
        self.app_event_tx
            .send(AppEvent::OpenUnifiedExecExportPrompt {
                session_id: self.entry.session_id,
            });
    }

    fn render_summary(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(format!(
            "Command: {}",
            summarize_command(&self.entry.command)
        )));
        let status_line_text = format!("Status: {}", self.entry.status.label());
        let status_line = match self.entry.status {
            ProcessStatus::Running => status_line_text.green(),
            ProcessStatus::Exited => status_line_text.dim(),
        };
        lines.push(Line::from(status_line));
        lines.push(Line::from(format!(
            "Started: {}",
            format_elapsed(self.entry.started_at)
        )));
        lines.push(Line::from(format!(
            "Last output: {}",
            format_optional_elapsed(self.entry.last_output_at)
        )));

        if self.data.total_bytes == 0 {
            lines.push(Line::from("No output captured yet.".dim()));
        } else {
            let window_line = format!(
                "Window: {} – {} of {} ({} window)",
                format_bytes(self.data.range_start),
                format_bytes(self.data.range_end),
                format_bytes(self.data.total_bytes),
                format_bytes(self.data.window_bytes as u64)
            );
            lines.push(Line::from(window_line));
        }

        if self.data.expandable_prefix {
            lines.push(Line::from(
                "Older output available — Alt+PgUp extends the view backwards.".cyan(),
            ));
        } else if self.data.truncated_prefix {
            lines.push(Line::from(
                "Older output truncated by the in-memory buffer."
                    .magenta()
                    .italic(),
            ));
        }

        if self.data.truncated_suffix {
            if self.data.expandable_suffix {
                lines.push(Line::from(
                    "Newer output hidden — Alt+PgDn moves the window forward.".cyan(),
                ));
            } else {
                lines.push(Line::from(
                    "Newer output truncated. Press r to return to the live tail."
                        .magenta()
                        .italic(),
                ));
            }
        }

        lines.push(
            format!(
                "Window cap: {} — use e to export the full log.",
                format_bytes(PROCESS_OUTPUT_WINDOW_MAX_BYTES as u64)
            )
            .dim()
            .into(),
        );

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Session {}", self.entry.session_id)),
            )
            .render(area, buf);
    }

    fn render_output(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            self.visible_rows.set(1);
            return;
        }
        let height = area.height as usize;
        self.visible_rows.set(height.max(1));
        self.clamp_offset();
        let start = self.current_offset();
        let end = (start + self.visible_rows()).min(self.total_rows());
        let lines: Vec<Line<'static>> = self.lines[start..end]
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect();

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Output"))
            .render(area, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let hint = "↑/↓ scroll  •  PgUp/PgDn page  •  g/G top/bottom  •  Alt+PgUp older  •  Alt+PgDn newer  •  r tail  •  e export  •  Esc close";
        Paragraph::new(Line::from(hint.dim())).render(area, buf);
    }
}

impl BottomPaneView for ProcessOutputView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => self.close_requested = true,
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.close_requested = true;
            }
            KeyEvent {
                code: KeyCode::PageUp,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::ALT) => {
                self.request_expand_backwards();
            }
            KeyEvent {
                code: KeyCode::PageDown,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::ALT) => {
                self.request_shift_forward();
            }
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers,
                ..
            } if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
                self.request_export();
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.scroll_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.scroll_down(),
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => self.page_up(),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => self.page_down(),
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers,
                ..
            } if modifiers.is_empty() => self.jump_to_top(),
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => self.jump_to_bottom(),
            KeyEvent {
                code: KeyCode::Char('r'),
                ..
            } => {
                self.app_event_tx.send(AppEvent::RefreshUnifiedExecOutput {
                    session_id: self.entry.session_id,
                });
            }
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.close_requested
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close_requested = true;
        CancellationEvent::Handled
    }
}

impl crate::render::renderable::Renderable for ProcessOutputView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let summary_height = self.summary_height();
        let layout = Layout::vertical([
            Constraint::Length(summary_height),
            Constraint::Min(3),
            Constraint::Length(Self::FOOTER_ROWS),
        ])
        .split(area);

        self.render_summary(layout[0], buf);
        self.render_output(layout[1], buf);
        self.render_footer(layout[2], buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.summary_height().saturating_add(Self::FOOTER_ROWS + 6)
    }
}

fn build_output_lines(output: &str) -> Vec<String> {
    if output.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = output
        .split('\n')
        .map(std::string::ToString::to_string)
        .collect();
    if output.ends_with('\n') {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::render::renderable::Renderable;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::unbounded_channel;

    fn buffer_lines(buf: &Buffer) -> Vec<String> {
        let area = buf.area();
        (0..area.height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..area.width {
                    let symbol = buf
                        .cell((x, y))
                        .map(ratatui::buffer::Cell::symbol)
                        .unwrap_or(" ");
                    line.push_str(symbol);
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn make_view_with_channel(
        entries: Vec<ProcessManagerEntry>,
    ) -> (ProcessManagerView, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        let view = ProcessManagerView::new(ProcessManagerInit {
            app_event_tx: sender,
            entries,
        });
        (view, rx)
    }

    fn make_view(entries: Vec<ProcessManagerEntry>) -> ProcessManagerView {
        make_view_with_channel(entries).0
    }

    fn make_output_view_with_data(
        entry: ProcessManagerEntry,
        data: ProcessOutputData,
    ) -> (ProcessOutputView, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        let init = ProcessOutputInit {
            app_event_tx: sender,
            entry,
            data,
        };
        (ProcessOutputView::new(init), rx)
    }

    fn make_output_view(
        entry: ProcessManagerEntry,
        output: &str,
        truncated: bool,
    ) -> (ProcessOutputView, UnboundedReceiver<AppEvent>) {
        let data = ProcessOutputData {
            content: output.to_string(),
            range_start: 0,
            range_end: output.len() as u64,
            total_bytes: output.len() as u64,
            truncated_prefix: truncated,
            truncated_suffix: false,
            expandable_prefix: truncated,
            expandable_suffix: false,
            window_bytes: output.len().max(1),
        };
        make_output_view_with_data(entry, data)
    }

    fn render_snapshot(view: &ProcessManagerView) -> String {
        let width = 80;
        let preview_height = 7;
        let footer_height = 1;
        let required_for_rows = view.entries.len() as u16 + 3 + preview_height + footer_height;
        let height = view.desired_height(width).max(12).max(required_for_rows);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        buffer_lines(&buf).join("\n")
    }

    fn state_with_preview(status: UnifiedExecSessionStatus) -> ProcessManagerEntry {
        let now = SystemTime::now();
        let started = now;
        let last_output = Some(now);
        let state = UnifiedExecSessionState {
            session_id: 7,
            command: vec!["sleep".into(), "10".into()],
            status,
            started_at_ms: system_time_to_millis(started),
            last_output_at_ms: last_output.map(system_time_to_millis),
            output_preview: "hello stdout".into(),
            output_truncated: false,
        };
        ProcessManagerEntry::from_state(state)
    }

    fn system_time_to_millis(time: SystemTime) -> u64 {
        time.duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_millis() as u64
    }

    fn snapshot_entry(
        session_id: i32,
        status: UnifiedExecSessionStatus,
        started_ago: Duration,
        last_output_ago: Option<Duration>,
        command: &[&str],
        preview: &str,
    ) -> ProcessManagerEntry {
        let now = SystemTime::now();
        let started = now - started_ago;
        let last_output = last_output_ago.map(|ago| now - ago);
        let state = UnifiedExecSessionState {
            session_id,
            command: command.iter().map(|part| (*part).to_string()).collect(),
            status,
            started_at_ms: system_time_to_millis(started),
            last_output_at_ms: last_output.map(system_time_to_millis),
            output_preview: preview.to_string(),
            output_truncated: false,
        };
        ProcessManagerEntry::from_state(state)
    }

    #[test]
    fn render_lists_command_and_timestamps() {
        let view = make_view(vec![state_with_preview(UnifiedExecSessionStatus::Running)]);
        let area = Rect::new(0, 0, 80, 18);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let joined = buffer_lines(&buf).join("\n");
        assert!(
            joined.contains("Command"),
            "header should include Command column"
        );
        assert!(
            joined.contains("Started"),
            "header should include Started column"
        );
        assert!(
            joined.contains("Last output"),
            "header should include Last output column"
        );
        assert!(joined.contains("sleep 10"), "command should be displayed");
    }

    #[test]
    fn render_preview_shows_metadata() {
        let view = make_view(vec![state_with_preview(UnifiedExecSessionStatus::Running)]);
        let area = Rect::new(0, 0, 80, 18);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let joined = buffer_lines(&buf).join("\n");
        assert!(joined.contains("Command: sleep 10"));
        assert!(joined.contains("Status:"));
        assert!(joined.contains("Last output:"));
    }

    #[test]
    fn navigation_and_actions_target_selected_session() {
        let mut first = state_with_preview(UnifiedExecSessionStatus::Running);
        first.session_id = 1;
        let mut second = state_with_preview(UnifiedExecSessionStatus::Running);
        second.session_id = 2;
        let (mut view, mut rx) = make_view_with_channel(vec![first, second]);

        // Move selection to the second entry.
        view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(view.scroll.selected_idx, Some(1));

        view.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let event = rx.try_recv().expect("input prompt event");
        match event {
            AppEvent::OpenUnifiedExecInputPrompt { session_id } => assert_eq!(session_id, 2),
            other => panic!("unexpected event: {other:?}"),
        }

        view.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        let event = rx.try_recv().expect("kill event");
        match event {
            AppEvent::KillUnifiedExecSession { session_id } => assert_eq!(session_id, 2),
            other => panic!("unexpected event: {other:?}"),
        }

        view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let event = rx.try_recv().expect("remove event");
        match event {
            AppEvent::RemoveUnifiedExecSession { session_id } => assert_eq!(session_id, 2),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn refresh_and_close_shortcuts_work() {
        let (mut view, mut rx) =
            make_view_with_channel(vec![state_with_preview(UnifiedExecSessionStatus::Running)]);

        view.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        match rx.try_recv() {
            Ok(AppEvent::OpenProcessManager) => {}
            Ok(other) => panic!("expected refresh event, got {other:?}"),
            Err(err) => panic!("missing refresh event: {err:?}"),
        }
        assert!(!view.is_complete(), "refresh should not close the view");

        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(view.is_complete(), "Esc should request closing the view");
    }

    #[test]
    fn snapshot_process_manager_populated() {
        let entries = vec![
            snapshot_entry(
                7,
                UnifiedExecSessionStatus::Running,
                Duration::from_secs(90),
                Some(Duration::from_secs(2)),
                &["bash", "-lc", "long_running_task"],
                "Compilation at 73%...\nNo errors so far.",
            ),
            snapshot_entry(
                3,
                UnifiedExecSessionStatus::Exited,
                Duration::from_secs(3_600),
                Some(Duration::from_secs(900)),
                &["cargo", "test"],
                "test suite::apply_patch ... ok",
            ),
        ];
        let view = make_view(entries);
        assert_snapshot!("process_manager_populated", render_snapshot(&view));
    }

    #[test]
    fn snapshot_process_manager_empty() {
        let view = make_view(Vec::new());
        assert_snapshot!("process_manager_empty", render_snapshot(&view));
    }

    #[test]
    fn snapshot_process_output_view() {
        let entry = snapshot_entry(
            7,
            UnifiedExecSessionStatus::Running,
            Duration::from_secs(120),
            Some(Duration::from_secs(5)),
            &["npm", "run", "build"],
            "building…",
        );
        let (view, _rx) = make_output_view(entry, "Compiling...\nDone!\n", false);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        assert_snapshot!("process_output_view_basic", buffer_lines(&buf).join("\n"));
    }

    #[test]
    fn snapshot_process_output_view_truncated() {
        let entry = snapshot_entry(
            3,
            UnifiedExecSessionStatus::Exited,
            Duration::from_secs(3600),
            Some(Duration::from_secs(60)),
            &["cargo", "test"],
            "Finished test suite.",
        );
        let log = (0..50)
            .map(|i| format!("log line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (view, _rx) = make_output_view(entry, &log, true);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        assert_snapshot!(
            "process_output_view_truncated",
            buffer_lines(&buf).join("\n")
        );
    }

    #[test]
    fn process_output_refresh_emits_event() {
        let entry = snapshot_entry(
            11,
            UnifiedExecSessionStatus::Running,
            Duration::from_secs(30),
            Some(Duration::from_secs(2)),
            &["bash", "-lc", "tail -f log"],
            "tailing",
        );
        let (mut view, mut rx) = make_output_view(entry, "line1\nline2", false);
        view.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        match rx.try_recv() {
            Ok(AppEvent::RefreshUnifiedExecOutput { session_id }) => {
                assert_eq!(session_id, 11);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn process_output_alt_page_up_requests_window() {
        let entry = snapshot_entry(
            42,
            UnifiedExecSessionStatus::Running,
            Duration::from_secs(10),
            Some(Duration::from_secs(1)),
            &["bash", "-lc", "watch"],
            "watching",
        );
        let data = ProcessOutputData {
            content: "recent\nlines\n".to_string(),
            range_start: 128,
            range_end: 256,
            total_bytes: 512,
            truncated_prefix: true,
            truncated_suffix: false,
            expandable_prefix: true,
            expandable_suffix: false,
            window_bytes: 128,
        };
        let (mut view, mut rx) = make_output_view_with_data(entry, data);
        view.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT));
        match rx.try_recv() {
            Ok(AppEvent::LoadUnifiedExecOutputWindow { session_id, window }) => {
                assert_eq!(session_id, 42);
                match window {
                    UnifiedExecOutputWindow::Range { start, max_bytes } => {
                        assert!(start < 128, "expected window to extend backwards");
                        assert!(max_bytes > 128, "expected window size to increase");
                    }
                    other => panic!("expected range window, got {other:?}"),
                }
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn process_output_alt_page_down_requests_forward_window() {
        let entry = snapshot_entry(
            7,
            UnifiedExecSessionStatus::Exited,
            Duration::from_secs(120),
            Some(Duration::from_secs(60)),
            &["bash", "-lc", "tail"],
            "tail",
        );
        let data = ProcessOutputData {
            content: "older\nchunk\n".to_string(),
            range_start: 0,
            range_end: 64,
            total_bytes: 256,
            truncated_prefix: false,
            truncated_suffix: true,
            expandable_prefix: false,
            expandable_suffix: true,
            window_bytes: 64,
        };
        let (mut view, mut rx) = make_output_view_with_data(entry, data);
        view.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::ALT));
        match rx.try_recv() {
            Ok(AppEvent::LoadUnifiedExecOutputWindow { session_id, window }) => {
                assert_eq!(session_id, 7);
                match window {
                    UnifiedExecOutputWindow::Range { start, max_bytes } => {
                        assert!(start > 0, "expected window to shift forward");
                        assert_eq!(max_bytes, 64, "window size should stay constant");
                    }
                    other => panic!("expected range window, got {other:?}"),
                }
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn process_output_export_opens_prompt() {
        let entry = snapshot_entry(
            5,
            UnifiedExecSessionStatus::Running,
            Duration::from_secs(5),
            Some(Duration::from_secs(1)),
            &["python", "script.py"],
            "running",
        );
        let (mut view, mut rx) = make_output_view(entry, "log", false);
        view.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        match rx.try_recv() {
            Ok(AppEvent::OpenUnifiedExecExportPrompt { session_id }) => {
                assert_eq!(session_id, 5);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
