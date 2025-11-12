use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::background_process::BackgroundProcess;
use crate::background_process::BackgroundProcessStore;
use crate::hint_bar::HintBar;
use crate::render::scroll_view::Bookmark;
use crate::render::scroll_view::ScrollView;
use crate::render::virtual_list::VirtualListAdapter;
use crate::render::virtual_list::VirtualListState;
use crate::text_formatting::truncate_text;
use codex_core::protocol::BackgroundShellStatus;
use codex_core::protocol::Op;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::tui;
use crate::tui::TuiEvent;

const SUMMARY_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const DETAIL_POLL_INTERVAL: Duration = Duration::from_secs(1);
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_millis(400);
const LIST_HINT_LINES: u16 = 2;
const DETAIL_HINT_LINES: u16 = 2;
const MAX_COMMAND_LABEL_GRAPHEMES: usize = 80;
const SUMMARY_LIMIT: usize = 50;

pub(crate) struct BackgroundShellScreen {
    store: Rc<RefCell<BackgroundProcessStore>>,
    app_event_tx: AppEventSender,
    processes: Vec<BackgroundProcess>,
    selected: usize,
    list_state: VirtualListState,
    detail: DetailViewState,
    mode: ScreenMode,
    list_prompt: Option<ListPrompt>,
    status: Option<StatusBanner>,
    next_summary_refresh: Instant,
    next_poll_refresh: Instant,
    last_list_area: Option<Rect>,
    last_log_area: Option<Rect>,
    is_done: bool,
    show_completed: bool,
}

impl BackgroundShellScreen {
    pub fn new(store: Rc<RefCell<BackgroundProcessStore>>, app_event_tx: AppEventSender) -> Self {
        Self {
            store,
            app_event_tx,
            processes: Vec::new(),
            selected: 0,
            list_state: VirtualListState::new(),
            detail: DetailViewState::new(),
            mode: ScreenMode::List,
            list_prompt: None,
            status: None,
            next_summary_refresh: Instant::now(),
            next_poll_refresh: Instant::now(),
            last_list_area: None,
            last_log_area: None,
            is_done: false,
            show_completed: false,
        }
    }

    pub fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> std::io::Result<()> {
        match event {
            TuiEvent::Key(key) if key.kind == KeyEventKind::Press => {
                self.handle_key_event(tui, key);
                tui.frame_requester().schedule_frame();
                Ok(())
            }
            TuiEvent::Key(_) => Ok(()),
            TuiEvent::Paste(_) => Ok(()),
            TuiEvent::Draw => {
                self.maybe_refresh();
                self.capture_processes();
                let result = self.render(tui);
                self.schedule_next_draw(tui);
                result
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.is_done
    }

    fn render(&mut self, tui: &mut tui::Tui) -> std::io::Result<()> {
        tui.draw(u16::MAX, |frame| {
            let area = frame.area();
            Clear.render(area, frame.buffer);
            match self.mode {
                ScreenMode::List => self.render_list(area, frame.buffer),
                ScreenMode::Detail => self.render_detail(area, frame.buffer),
            }
        })
    }

    fn schedule_next_draw(&self, tui: &mut tui::Tui) {
        let should_refresh = matches!(self.mode, ScreenMode::Detail)
            || self
                .processes
                .iter()
                .any(|proc| matches!(proc.status, BackgroundShellStatus::Running));
        if should_refresh {
            tui.frame_requester()
                .schedule_frame_in(AUTO_REFRESH_INTERVAL);
        }
    }

    fn render_list(&mut self, area: Rect, buf: &mut Buffer) {
        let (content_area, hint_area) = split_area_for_hints(area, LIST_HINT_LINES);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Line::from(" Background shells ").bold().cyan());
        let inner = block.inner(content_area);
        block.render(content_area, buf);
        if inner.height == 0 {
            return;
        }
        if self.processes.is_empty() {
            Paragraph::new(Line::from("No background shells yet".dim()))
                .alignment(ratatui::layout::Alignment::Center)
                .render(inner, buf);
        } else {
            let layout = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(inner);

            Paragraph::new(self.build_stats_line())
                .wrap(Wrap { trim: true })
                .render(layout[0], buf);

            self.last_list_area = Some(layout[1]);
            let adapter = ProcessListAdapter {
                processes: &self.processes,
            };
            self.list_state
                .render(&adapter, self.selected_index(), layout[1], buf);
        }

        if hint_area.height > 0 {
            let mut hint_lines = Vec::new();
            if let Some(prompt) = &self.list_prompt {
                hint_lines.push(prompt.line());
            } else if let Some(status) = &self.status {
                hint_lines.push(Line::from(vec![
                    Span::from(status.text.clone()).fg(Color::Yellow),
                ]));
            } else {
                hint_lines.push(self.default_list_hint_line());
            }
            if hint_lines.len() < LIST_HINT_LINES as usize {
                hint_lines.push(toggle_hint_line(self.show_completed));
            }
            HintBar::new(hint_lines).render(hint_area, buf);
        }
    }

    fn render_detail(&mut self, area: Rect, buf: &mut Buffer) {
        let (content_area, hint_area) = split_area_for_hints(area, DETAIL_HINT_LINES);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Line::from(" Shell details ").bold().cyan());
        let inner = block.inner(content_area);
        block.render(content_area, buf);
        if inner.height == 0 {
            return;
        }
        if let Some(process) = self.selected_process() {
            let summary_height = inner.height.min(6);
            let layout = Layout::vertical([
                Constraint::Length(summary_height),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(inner);

            self.render_detail_summary(layout[0], buf, process);
            self.last_log_area = Some(layout[1]);
            self.detail.scroll.render(layout[1], buf);

            if hint_area.height > 0 {
                let mut hint_lines = Vec::new();
                hint_lines.push(self.detail.scroll.info_line());
                let mut hint_line = if let Some(input) = &self.detail.input_mode {
                    input.line()
                } else {
                    Line::from(vec![Span::from(
                    "Esc back · Enter list · ↑/↓ scroll · PgUp/PgDn page · / search · n/N next match · b bookmark cycle · t tail · k kill",
                )
                .fg(Color::White)])
                };
                append_toggle_suffix(&mut hint_line, self.show_completed);
                hint_lines.push(hint_line);
                HintBar::new(hint_lines).render(hint_area, buf);
            }
        } else {
            Paragraph::new(Line::from("No process selected".dim()))
                .alignment(ratatui::layout::Alignment::Center)
                .render(inner, buf);
            if hint_area.height > 0 {
                HintBar::new(vec![toggle_hint_line(self.show_completed)]).render(hint_area, buf);
            }
        }
    }

    fn render_detail_summary(&self, area: Rect, buf: &mut Buffer, process: &BackgroundProcess) {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            "Status: ".into(),
            status_span(&process.status, process.exit_code),
        ]));
        if let Some(runtime) = process.runtime() {
            lines.push(Line::from(format!("Runtime: {}", format_duration(runtime))));
        }
        lines.push(Line::from(format!(
            "Command: {}",
            process.command_display()
        )));
        lines.push(Line::from(format!("Short ID: {}", process.label())));
        if let Some(bookmark) = &process.bookmark {
            lines.push(Line::from(format!("Bookmark: #{bookmark}")));
        }
        if let Some(description) = &process.description {
            lines.push(Line::from(format!("Description: {description}")));
        }
        if process.log_truncated() {
            lines.push(Line::from(vec![
                Span::from("Stdout truncated; press t to stream live output").fg(Color::Yellow),
            ]));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn handle_key_event(&mut self, tui: &mut tui::Tui, key: KeyEvent) {
        if self.is_done {
            return;
        }
        if self.mode == ScreenMode::List
            && let Some(mut prompt) = self.list_prompt.take()
        {
            let done = self.handle_list_prompt_key(&mut prompt, key);
            if !done {
                self.list_prompt = Some(prompt);
            }
            return;
        }
        if self.mode == ScreenMode::Detail && self.detail.handle_input_key(key) {
            return;
        }

        match self.mode {
            ScreenMode::List => self.handle_list_key(key, tui),
            ScreenMode::Detail => self.handle_detail_key(key, tui),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, tui: &mut tui::Tui) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.is_done = true,
            KeyCode::Enter => self.enter_detail(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.page_selection(-1),
            KeyCode::PageDown => self.page_selection(1),
            KeyCode::Char('r') => self.refresh_summary(),
            KeyCode::Char('t') | KeyCode::Char('p') => self.poll_selected(),
            KeyCode::Char('k') => self.kill_selected(),
            KeyCode::Char('a') => self.toggle_show_completed(),
            KeyCode::Char('x') => {
                self.store.borrow_mut().purge_finished();
                self.capture_processes();
            }
            KeyCode::Char('#') => self.list_prompt = Some(ListPrompt::bookmark()),
            KeyCode::Char('.') => self.list_prompt = Some(ListPrompt::id()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.is_done = true;
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ignore – already on background screen.
                let _ = tui.notify("Background manager already open");
            }
            _ => {}
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent, _tui: &mut tui::Tui) {
        match key.code {
            KeyCode::Esc => self.exit_detail(),
            KeyCode::Enter => self.exit_detail(),
            KeyCode::Char('q') => self.is_done = true,
            KeyCode::Up => self.detail.scroll.scroll_by(-1),
            KeyCode::Down => self.detail.scroll.scroll_by(1),
            KeyCode::PageUp => self.page_logs(-1),
            KeyCode::PageDown => self.page_logs(1),
            KeyCode::Home => self.detail.scroll.jump_to_start(),
            KeyCode::End => self.detail.scroll.jump_to_end(),
            KeyCode::Char('/') => self.detail.begin_search(),
            KeyCode::Char('a') => self.toggle_show_completed(),
            KeyCode::Char('n') => {
                if !self.detail.scroll.next_match() {
                    self.set_status("No search matches".to_string());
                }
            }
            KeyCode::Char('N') => {
                if !self.detail.scroll.prev_match() {
                    self.set_status("No search matches".to_string());
                }
            }
            KeyCode::Char('b') => {
                if !self.detail.scroll.cycle_bookmark(true) {
                    self.set_status("No bookmarks".to_string());
                }
            }
            KeyCode::Char('B') => {
                if !self.detail.scroll.cycle_bookmark(false) {
                    self.set_status("No bookmarks".to_string());
                }
            }
            KeyCode::Char('t') | KeyCode::Char('p') => self.poll_selected(),
            KeyCode::Char('k') => self.kill_selected(),
            _ => {}
        }
    }

    fn handle_list_prompt_key(&mut self, prompt: &mut ListPrompt, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => true,
            KeyCode::Enter => {
                let text = prompt.buffer.trim();
                if text.is_empty() {
                    return true;
                }
                let found = match prompt.mode {
                    ListPromptMode::Bookmark => self.jump_to_bookmark(text),
                    ListPromptMode::Id => self.jump_to_id(text),
                };
                if !found {
                    self.set_status(format!("No match for `{text}`"));
                }
                true
            }
            KeyCode::Backspace => {
                prompt.buffer.pop();
                false
            }
            KeyCode::Char(c) => {
                prompt.buffer.push(c);
                false
            }
            _ => false,
        }
    }

    fn toggle_show_completed(&mut self) {
        self.show_completed = !self.show_completed;
        let status = if self.show_completed {
            "Showing running + finished shells"
        } else {
            "Showing running shells only"
        };
        self.set_status(status.to_string());
        self.capture_processes();
    }

    fn page_selection(&mut self, delta: isize) {
        if self.processes.is_empty() {
            return;
        }
        let area = self.last_list_area.unwrap_or(Rect::new(0, 0, 0, 10));
        let adapter = ProcessListAdapter {
            processes: &self.processes,
        };
        let step = self.list_state.page_step(&adapter, area) as isize;
        let new_index = (self.selected_index() as isize + step * delta)
            .clamp(0, self.processes.len().saturating_sub(1) as isize)
            as usize;
        self.selected = new_index;
    }

    fn page_logs(&mut self, delta: isize) {
        let height = self
            .last_log_area
            .map(|area| area.height as usize)
            .unwrap_or(5);
        self.detail.scroll.page_by(height, delta);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.processes.is_empty() {
            return;
        }
        let len = self.processes.len();
        let current = self.selected_index() as isize;
        let next = (current + delta).rem_euclid(len as isize);
        self.selected = next as usize;
    }

    fn enter_detail(&mut self) {
        if self.processes.is_empty() {
            return;
        }
        self.mode = ScreenMode::Detail;
        self.sync_detail_view();
        self.poll_selected();
    }

    fn exit_detail(&mut self) {
        self.mode = ScreenMode::List;
        self.detail.reset();
    }

    fn jump_to_bookmark(&mut self, alias: &str) -> bool {
        if let Some((idx, _)) = self.processes.iter().enumerate().find(|(_, proc)| {
            proc.bookmark
                .as_deref()
                .is_some_and(|b| b.eq_ignore_ascii_case(alias))
        }) {
            self.selected = idx;
            true
        } else {
            false
        }
    }

    fn jump_to_id(&mut self, id: &str) -> bool {
        if let Some((idx, _)) = self.processes.iter().enumerate().find(|(_, proc)| {
            proc.shell_id.eq_ignore_ascii_case(id)
                || proc
                    .label()
                    .to_ascii_lowercase()
                    .contains(&id.to_ascii_lowercase())
        }) {
            self.selected = idx;
            true
        } else {
            false
        }
    }

    pub(crate) fn refresh_summary(&self) {
        self.app_event_tx
            .send(AppEvent::CodexOp(Op::BackgroundShellSummary {
                limit: Some(SUMMARY_LIMIT),
            }));
    }

    fn poll_selected(&self) {
        if let Some(proc) = self.selected_process() {
            self.app_event_tx
                .send(AppEvent::CodexOp(Op::PollBackgroundShell {
                    shell_id: proc.shell_id.clone(),
                }));
        }
    }

    fn kill_selected(&mut self) {
        if let Some(proc) = self.selected_process() {
            self.app_event_tx
                .send(AppEvent::CodexOp(Op::KillBackgroundShell {
                    shell_id: proc.shell_id.clone(),
                }));
            self.set_status(format!("Killing {}", proc.command_display()));
        }
    }

    fn maybe_refresh(&mut self) {
        let now = Instant::now();
        if now >= self.next_summary_refresh {
            self.refresh_summary();
            self.next_summary_refresh = now + SUMMARY_REFRESH_INTERVAL;
        }
        if self.mode == ScreenMode::Detail && now >= self.next_poll_refresh {
            self.poll_selected();
            self.next_poll_refresh = now + DETAIL_POLL_INTERVAL;
        }
        if let Some(status) = &self.status
            && status.expires_at <= now
        {
            self.status = None;
        }
    }

    fn capture_processes(&mut self) {
        let snapshot = self.store.borrow().snapshot();
        let mut running = Vec::new();
        let mut finished = Vec::new();
        for proc in snapshot {
            if matches!(proc.status, BackgroundShellStatus::Running) {
                running.push(proc);
            } else {
                finished.push(proc);
            }
        }
        if self.show_completed {
            running.extend(finished);
        }
        self.processes = running;
        if self.processes.is_empty() {
            self.selected = 0;
            self.detail.reset();
            if self.mode == ScreenMode::Detail {
                self.exit_detail();
            }
            return;
        }
        self.selected = self.selected.min(self.processes.len().saturating_sub(1));
        if self.mode == ScreenMode::Detail {
            self.sync_detail_view();
        }
    }

    fn sync_detail_view(&mut self) {
        if self.mode != ScreenMode::Detail {
            return;
        }
        if let Some(process) = self.selected_process().cloned() {
            if self.detail.shell_id.as_deref() != Some(process.shell_id.as_str()) {
                self.detail.shell_id = Some(process.shell_id.clone());
                self.detail.scroll.jump_to_end();
            }
            let lines = process.log_snapshot(200);
            self.detail.scroll.set_lines(lines.clone());
            let mut bookmarks = vec![Bookmark {
                name: "top".into(),
                line: 0,
            }];
            if !lines.is_empty() {
                bookmarks.push(Bookmark {
                    name: "latest".into(),
                    line: lines.len().saturating_sub(1),
                });
            }
            if let Some(alias) = &process.bookmark {
                bookmarks.push(Bookmark {
                    name: format!("#{alias}"),
                    line: lines.len().saturating_sub(1),
                });
            }
            self.detail.scroll.set_bookmarks(bookmarks);
        }
    }

    fn selected_index(&self) -> usize {
        self.selected.min(self.processes.len().saturating_sub(1))
    }

    fn selected_process(&self) -> Option<&BackgroundProcess> {
        self.processes.get(self.selected_index())
    }

    fn build_stats_line(&self) -> Line<'static> {
        let mut running = 0usize;
        let mut failed = 0usize;
        for process in &self.processes {
            match process.status {
                BackgroundShellStatus::Running => running += 1,
                BackgroundShellStatus::Failed => failed += 1,
                BackgroundShellStatus::Completed => {}
            }
        }
        let total = self.processes.len();
        let mut parts = Vec::new();
        if running == 1 {
            parts.push("1 shell running".to_string());
        } else {
            parts.push(format!("{running} shells running"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if total > running + failed {
            let done = total - running - failed;
            parts.push(format!("{done} completed"));
        }
        if let Some(proc) = self.selected_process() {
            parts.push(format!(
                "Selected: {}",
                truncate_text(&proc.command_display(), 40)
            ));
        }
        let text = parts.join(" · ");
        Line::from(vec![Span::from(text).fg(Color::White)])
    }

    fn default_list_hint_line(&self) -> Line<'static> {
        Line::from(vec![Span::from(
            "Enter view · Esc close · ↑/↓ move · PgUp/PgDn page · # bookmark search · . ID search · r refresh · t tail · k kill · x hide done",
        )
        .fg(Color::White)])
    }

    fn set_status(&mut self, text: String) {
        self.status = Some(StatusBanner {
            text,
            expires_at: Instant::now() + Duration::from_secs(4),
        });
    }
}

struct DetailViewState {
    scroll: ScrollView,
    shell_id: Option<String>,
    input_mode: Option<DetailInputMode>,
}

impl DetailViewState {
    fn new() -> Self {
        Self {
            scroll: ScrollView::new(),
            shell_id: None,
            input_mode: None,
        }
    }

    fn reset(&mut self) {
        self.input_mode = None;
    }

    fn begin_search(&mut self) {
        self.input_mode = Some(DetailInputMode::Search {
            buffer: String::new(),
        });
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        match self.input_mode.as_mut() {
            Some(DetailInputMode::Search { buffer }) => match key.code {
                KeyCode::Esc => {
                    self.input_mode = None;
                    true
                }
                KeyCode::Enter => {
                    let query = if buffer.trim().is_empty() {
                        None
                    } else {
                        Some(buffer.trim().to_string())
                    };
                    self.scroll.set_filter(query);
                    self.input_mode = None;
                    true
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    true
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    true
                }
                _ => true,
            },
            None => false,
        }
    }
}

enum DetailInputMode {
    Search { buffer: String },
}

impl DetailInputMode {
    fn line(&self) -> Line<'static> {
        match self {
            DetailInputMode::Search { buffer } => Line::from(vec![
                Span::from(format!("Search: {buffer}_")).fg(Color::Yellow),
            ]),
        }
    }
}

struct StatusBanner {
    text: String,
    expires_at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScreenMode {
    List,
    Detail,
}

fn split_area_for_hints(area: Rect, hint_lines: u16) -> (Rect, Rect) {
    if hint_lines == 0 || area.height <= hint_lines {
        return (area, Rect::new(area.x, area.y + area.height, area.width, 0));
    }
    let content_height = area.height - hint_lines;
    let content = Rect::new(area.x, area.y, area.width, content_height);
    let hint = Rect::new(area.x, area.y + content_height, area.width, hint_lines);
    (content, hint)
}

fn toggle_hint_suffix(show_completed: bool) -> Vec<Span<'static>> {
    let action = if show_completed {
        "hide completed processes"
    } else {
        "show completed processes"
    };
    vec![Span::from("a ").bold(), Span::from(action).fg(Color::White)]
}

fn toggle_hint_line(show_completed: bool) -> Line<'static> {
    Line::from(toggle_hint_suffix(show_completed))
}

fn append_toggle_suffix(line: &mut Line<'static>, show_completed: bool) {
    line.spans.push(" · ".dim());
    line.spans.extend(toggle_hint_suffix(show_completed));
}

struct ListPrompt {
    mode: ListPromptMode,
    buffer: String,
}

enum ListPromptMode {
    Bookmark,
    Id,
}

impl ListPrompt {
    fn bookmark() -> Self {
        Self {
            mode: ListPromptMode::Bookmark,
            buffer: String::new(),
        }
    }

    fn id() -> Self {
        Self {
            mode: ListPromptMode::Id,
            buffer: String::new(),
        }
    }

    fn line(&self) -> Line<'static> {
        let label = match self.mode {
            ListPromptMode::Bookmark => "Jump to bookmark",
            ListPromptMode::Id => "Jump to ID",
        };
        Line::from(vec![
            Span::from(format!("{label}: {}_", self.buffer)).fg(Color::Cyan),
        ])
    }
}

struct ProcessListAdapter<'a> {
    processes: &'a [BackgroundProcess],
}

impl<'a> VirtualListAdapter for ProcessListAdapter<'a> {
    fn len(&self) -> usize {
        self.processes.len()
    }

    fn item_height(&self, _index: usize, _width: u16) -> u16 {
        2
    }

    fn render_item(&self, index: usize, area: Rect, buf: &mut Buffer, selected: bool) {
        if let Some(process) = self.processes.get(index) {
            let command = truncate_text(&process.command_display(), MAX_COMMAND_LABEL_GRAPHEMES);
            let mut headline = vec![
                status_span(&process.status, process.exit_code),
                "  ".into(),
                command.bold(),
            ];
            if let Some(runtime) = process.runtime() {
                headline.push("  ".into());
                headline.push(Span::from(format_duration(runtime)).dim());
            }
            let mut secondary = vec!["   ".into(), process.label().dim()];
            if let Some(description) = &process.description {
                secondary.push(" · ".dim());
                secondary.push(description.clone().into());
            }
            let output = process
                .latest_output()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "waiting for stdout".to_string());
            secondary.push(" · ".dim());
            secondary.push(format!("stdout: {output}").dim());

            let lines = vec![Line::from(headline), Line::from(secondary)];
            let mut paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
            if selected {
                paragraph = paragraph.style(Style::default().fg(Color::Black).bg(Color::Cyan));
            }
            paragraph.render(area, buf);
        }
    }
}

fn status_span(status: &BackgroundShellStatus, exit_code: Option<i32>) -> Span<'static> {
    match status {
        BackgroundShellStatus::Running => Span::from("running").cyan(),
        BackgroundShellStatus::Completed => Span::from("completed").green(),
        BackgroundShellStatus::Failed => {
            let code = exit_code.unwrap_or(-1);
            Span::from(format!("failed ({code})")).red()
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{hours}h {mins:02}m")
    }
}
