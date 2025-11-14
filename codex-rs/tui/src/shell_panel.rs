use std::io::Result;
use std::io::Write as _;
use std::sync::Arc;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::history_cell::ShellCardData;
use crate::history_cell::format_shell_command;
use crate::history_cell::shell_label_for_display;
use crate::text_formatting::truncate_text;
use crate::tui;
use crate::tui::TuiEvent;
use codex_core::protocol::BackgroundShellControlAction;
use codex_core::protocol::Op;
use codex_protocol::models::BackgroundShellEndedBy;
use codex_protocol::models::BackgroundShellStartMode;
use codex_protocol::models::BackgroundShellStatus;
use codex_shell_model::ShellState;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use parking_lot::Mutex;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use tempfile::Builder as TempFileBuilder;

const LIST_HINT: &str = "←/→ tabs · ↑/↓ select · Enter details · k kill · d diagnostics · r resume · Ctrl+R background · q/Esc exit";
const DETAIL_HINT: &str = "↑/↓ scroll · d diagnostics · c copy · q/Esc back";
const DIAG_HINT: &str = "↑/↓ scroll · c copy · q/Esc back";

pub(crate) type SharedShellCard = Arc<Mutex<ShellCardData>>;

pub(crate) struct ShellPanelOverlay {
    cards: Vec<SharedShellCard>,
    tab: ShellPanelTab,
    selections: [usize; ShellPanelTab::COUNT],
    view: ShellPanelView,
    status_message: Option<String>,
    app_event_tx: AppEventSender,
    is_done: bool,
    esc_press_pending: bool,
}

impl ShellPanelOverlay {
    pub(crate) fn new(
        cards: Vec<SharedShellCard>,
        focused_shell: Option<String>,
        app_event_tx: AppEventSender,
    ) -> Self {
        let mut overlay = Self {
            cards,
            tab: ShellPanelTab::Running,
            selections: [0; ShellPanelTab::COUNT],
            view: ShellPanelView::List,
            status_message: None,
            app_event_tx,
            is_done: false,
            esc_press_pending: false,
        };
        overlay.focus_on_shell(focused_shell);
        overlay
    }

    pub(crate) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                match key_event.kind {
                    KeyEventKind::Press | KeyEventKind::Repeat => {
                        if key_event.code == KeyCode::Esc {
                            self.esc_press_pending = true;
                        }
                        self.process_key(key_event);
                        tui.frame_requester().schedule_frame();
                    }
                    KeyEventKind::Release if key_event.code == KeyCode::Esc => {
                        if self.esc_press_pending {
                            self.esc_press_pending = false;
                        } else {
                            self.process_key(key_event);
                            tui.frame_requester().schedule_frame();
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    self.render(frame.area(), frame.buffer);
                })?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
    }

    fn process_key(&mut self, key_event: KeyEvent) {
        match (&self.view, key_event.code, key_event.modifiers) {
            (_, KeyCode::Esc, _) | (_, KeyCode::Char('q'), _) => {
                if matches!(self.view, ShellPanelView::List) {
                    self.is_done = true;
                } else {
                    self.view = ShellPanelView::List;
                }
            }
            (ShellPanelView::List, KeyCode::Left, _) => {
                self.tab = self.tab.prev();
            }
            (ShellPanelView::List, KeyCode::Right, _) => {
                self.tab = self.tab.next();
            }
            (ShellPanelView::List, KeyCode::Up, _) => {
                self.move_selection(-1);
            }
            (ShellPanelView::List, KeyCode::Down, _)
            | (ShellPanelView::List, KeyCode::Char('j'), _) => {
                self.move_selection(1);
            }
            (ShellPanelView::List, KeyCode::Enter, _) => {
                if let Some(shell_id) = self.selected_shell_id() {
                    self.view = ShellPanelView::Detail {
                        shell_id,
                        scroll: 0,
                    };
                }
            }
            (ShellPanelView::List, KeyCode::Char('d'), _) => {
                if let Some(shell_id) = self.selected_shell_id() {
                    self.view = ShellPanelView::Diagnostic {
                        shell_id,
                        scroll: 0,
                    };
                }
            }
            (ShellPanelView::List, KeyCode::Char('k'), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.dispatch_control(BackgroundShellControlAction::Kill);
            }
            (ShellPanelView::List, KeyCode::Char('r'), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.dispatch_control(BackgroundShellControlAction::Resume);
            }
            (_, KeyCode::Char('r'), modifiers)
                if modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.dispatch_control(BackgroundShellControlAction::BackgroundRequest);
            }
            (ShellPanelView::Detail { .. }, KeyCode::Char('d'), _)
            | (ShellPanelView::Diagnostic { .. }, KeyCode::Char('d'), _) => {
                self.toggle_diagnostics();
            }
            (ShellPanelView::Detail { .. }, KeyCode::Up, _)
            | (ShellPanelView::Detail { .. }, KeyCode::Char('k'), _) => {
                self.adjust_scroll(-1);
            }
            (ShellPanelView::Detail { .. }, KeyCode::Down, _)
            | (ShellPanelView::Detail { .. }, KeyCode::Char('j'), _) => {
                self.adjust_scroll(1);
            }
            (ShellPanelView::Diagnostic { .. }, KeyCode::Up, _)
            | (ShellPanelView::Diagnostic { .. }, KeyCode::Char('k'), _) => {
                self.adjust_scroll(-1);
            }
            (ShellPanelView::Diagnostic { .. }, KeyCode::Down, _)
            | (ShellPanelView::Diagnostic { .. }, KeyCode::Char('j'), _) => {
                self.adjust_scroll(1);
            }
            (ShellPanelView::Detail { .. }, KeyCode::Char('c'), _)
            | (ShellPanelView::Diagnostic { .. }, KeyCode::Char('c'), _) => {
                self.copy_logs_to_temp();
            }
            _ => {}
        }
    }

    #[cfg(test)]
    fn handle_key_for_test(&mut self, key_event: KeyEvent) {
        match key_event.kind {
            KeyEventKind::Press | KeyEventKind::Repeat => {
                if key_event.code == KeyCode::Esc {
                    self.esc_press_pending = true;
                }
                self.process_key(key_event);
            }
            KeyEventKind::Release if key_event.code == KeyCode::Esc => {
                if self.esc_press_pending {
                    self.esc_press_pending = false;
                } else {
                    self.process_key(key_event);
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        match self.view.clone() {
            ShellPanelView::List => self.render_list(area, buf),
            ShellPanelView::Detail { shell_id, scroll } => {
                self.render_detail(area, buf, shell_id, scroll, false)
            }
            ShellPanelView::Diagnostic { shell_id, scroll } => {
                self.render_detail(area, buf, shell_id, scroll, true)
            }
        }
    }

    fn render_list(&mut self, area: Rect, buf: &mut Buffer) {
        let tab_line = Rect::new(area.x, area.y, area.width, 1);
        self.render_tabs(tab_line, buf);

        if area.height < 3 {
            return;
        }

        let list_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let entries = self.entries_for_tab(self.tab);
        let count = entries.len();
        let max_rows = list_area.height as usize;
        let mut start = 0usize;
        if count > max_rows && max_rows > 0 {
            let selected = self.selections[self.tab.index()];
            let half = max_rows / 2;
            if selected > half {
                start = (selected - half).min(count - max_rows);
            }
        }

        for (visible_idx, entry) in entries.iter().enumerate().skip(start).take(max_rows) {
            let y = list_area.y + (visible_idx - start) as u16;
            if y >= list_area.y + list_area.height {
                break;
            }
            let prefix = if visible_idx == self.selections[self.tab.index()] {
                "> ".bold()
            } else {
                "  ".into()
            };
            let mut line = Line::from(vec![prefix]);
            line.push_span(entry.status_icon());
            line.push_span(Span::from(" "));
            line.push_span(Span::from(entry.label()));
            line.push_span(Span::from("  "));
            let info_width = list_area
                .width
                .saturating_sub(line.width() as u16)
                .saturating_sub(1) as usize;
            let info = truncate_text(&entry.info_text(), info_width);
            line.push_span(info.dim());
            Paragraph::new(line).render(Rect::new(list_area.x, y, list_area.width, 1), buf);
        }

        let hint_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let mut hint_line = Line::from(LIST_HINT);
        if let Some(status) = &self.status_message {
            hint_line.push_span(Span::raw("  ·  "));
            hint_line.push_span(status.clone().dim());
        }
        Paragraph::new(hint_line)
            .style(Style::default().dim())
            .render(hint_area, buf);
    }

    fn render_tabs(&self, area: Rect, buf: &mut Buffer) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (idx, tab) in ShellPanelTab::ALL.iter().enumerate() {
            if idx > 0 {
                spans.push(" - ".into());
            }
            if *tab == self.tab {
                spans.push(format!("[{}]", tab.label()).bold());
            } else {
                spans.push(tab.label().to_lowercase().dim());
            }
        }
        if let Some(text) = self.tab_counts_summary() {
            spans.push("   ".into());
            spans.push(text.dim());
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    fn render_detail(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        shell_id: String,
        scroll: usize,
        diagnostics: bool,
    ) {
        let Some(entry) = self.entry_by_id(&shell_id) else {
            self.view = ShellPanelView::List;
            return;
        };
        let header = Rect::new(area.x, area.y, area.width, 1);
        let header_line = Line::from(format!(
            "[PROCESS]: {} ({})",
            entry.shell_id(),
            entry.label()
        ));
        Paragraph::new(header_line).render(header, buf);

        if area.height < 3 {
            return;
        }

        let body_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );

        let mut lines: Vec<String> = Vec::new();
        if !diagnostics {
            lines.push(format!("Status: {}", entry.status_label()));
            lines.push(format!("Start mode: {}", entry.start_mode_label()));
            if let Some(promoted_by) = entry.state.promoted_by {
                lines.push(format!("Promoted by: {}", describe_actor(promoted_by)));
            }
            if let Some(ended_by) = entry.state.ended_by {
                lines.push(format!("Ended by: {}", describe_actor(ended_by)));
            }
            if let Some(code) = entry.state.exit_code {
                lines.push(format!("Exit code: {code}"));
            }
            let command = format_shell_command(&entry.state.command);
            if !command.is_empty() {
                lines.push(format!("Command: {command}"));
            }
            if let Some(pid) = entry.state.pid {
                lines.push(format!("PID: {pid}"));
            }
            if let Some(reason) = &entry.state.reason
                && !reason.is_empty()
            {
                lines.push(format!("Reason: {reason}"));
            }
            lines.push(String::new());
            lines.push("Logs:".to_string());
        }

        let (log_lines, truncated) = if let Some(tail) = entry.state.tail.as_ref() {
            if tail.lines.is_empty() {
                (vec!["(no output)".to_string()], tail.truncated)
            } else {
                (tail.lines.clone(), tail.truncated)
            }
        } else if let Some(last) = entry.state.last_log.as_ref() {
            if last.is_empty() {
                (vec!["(no output)".to_string()], false)
            } else {
                (last.clone(), false)
            }
        } else {
            (vec!["(no output)".to_string()], false)
        };

        lines.extend(log_lines);
        if truncated {
            lines.push("… output truncated (~2KiB cap)".to_string());
        }

        let max_visible = body_area.height as usize;
        let mut start = scroll.min(lines.len().saturating_sub(1));
        if lines.len() > max_visible && max_visible > 0 {
            start = start.min(lines.len() - max_visible);
        } else {
            start = 0;
        }

        for (idx, line) in lines.into_iter().enumerate().skip(start).take(max_visible) {
            let y = body_area.y + idx.saturating_sub(start) as u16;
            Paragraph::new(line).render(Rect::new(body_area.x, y, body_area.width, 1), buf);
        }

        let hint_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let hint = if diagnostics { DIAG_HINT } else { DETAIL_HINT };
        let mut hint_line = Line::from(hint);
        if let Some(status) = &self.status_message {
            hint_line.push_span(Span::raw("  ·  "));
            hint_line.push_span(status.clone().dim());
        }
        Paragraph::new(hint_line)
            .style(Style::default().dim())
            .render(hint_area, buf);
    }

    fn focus_on_shell(&mut self, target: Option<String>) {
        if let Some(shell_id) = target
            && let Some((tab, idx)) = self.locate_shell(&shell_id)
        {
            self.tab = tab;
            self.selections[tab.index()] = idx;
        }
    }

    fn locate_shell(&self, shell_id: &str) -> Option<(ShellPanelTab, usize)> {
        let mut running = 0usize;
        let mut completed = 0usize;
        let mut failed = 0usize;
        for handle in &self.cards {
            let card = handle.lock();
            match ShellPanelTab::from_status(card.state.status) {
                ShellPanelTab::Running => {
                    if card.state.shell_id == shell_id {
                        return Some((ShellPanelTab::Running, running));
                    }
                    running += 1;
                }
                ShellPanelTab::Completed => {
                    if card.state.shell_id == shell_id {
                        return Some((ShellPanelTab::Completed, completed));
                    }
                    completed += 1;
                }
                ShellPanelTab::Failed => {
                    if card.state.shell_id == shell_id {
                        return Some((ShellPanelTab::Failed, failed));
                    }
                    failed += 1;
                }
            }
        }
        None
    }

    fn selected_shell_id(&self) -> Option<String> {
        let idx = self.selections[self.tab.index()];
        let mut current = 0usize;
        for handle in &self.cards {
            let card = handle.lock();
            if ShellPanelTab::from_status(card.state.status) == self.tab {
                if current == idx {
                    return Some(card.state.shell_id.clone());
                }
                current += 1;
            }
        }
        None
    }

    fn current_shell_id(&self) -> Option<String> {
        match &self.view {
            ShellPanelView::List => self.selected_shell_id(),
            ShellPanelView::Detail { shell_id, .. }
            | ShellPanelView::Diagnostic { shell_id, .. } => Some(shell_id.clone()),
        }
    }

    fn entries_for_tab(&mut self, tab: ShellPanelTab) -> Vec<ShellPanelEntry> {
        let mut entries = Vec::new();
        for handle in &self.cards {
            let card = handle.lock();
            if ShellPanelTab::from_status(card.state.status) == tab {
                entries.push(ShellPanelEntry::from(&card));
            }
        }
        if entries.is_empty() {
            self.selections[tab.index()] = 0;
        } else {
            let sel = self.selections[tab.index()].min(entries.len().saturating_sub(1));
            self.selections[tab.index()] = sel;
        }
        entries
    }

    fn tab_counts_summary(&self) -> Option<String> {
        if self.cards.is_empty() {
            return None;
        }
        let mut running = 0usize;
        let mut completed = 0usize;
        let mut failed = 0usize;
        for handle in &self.cards {
            let card = handle.lock();
            match card.state.status {
                BackgroundShellStatus::Pending | BackgroundShellStatus::Running => running += 1,
                BackgroundShellStatus::Completed => completed += 1,
                BackgroundShellStatus::Failed => failed += 1,
            }
        }
        Some(format!("{running}R · {completed}C · {failed}F"))
    }

    fn move_selection(&mut self, delta: isize) {
        let entries = self.entries_for_tab(self.tab);
        if entries.is_empty() {
            return;
        }
        let idx = self.selections[self.tab.index()] as isize + delta;
        let new_idx = idx.clamp(0, (entries.len() - 1) as isize) as usize;
        self.selections[self.tab.index()] = new_idx;
    }

    fn dispatch_control(&mut self, action: BackgroundShellControlAction) {
        let Some(shell_id) = self.current_shell_id() else {
            self.status_message = Some("No shell selected".to_string());
            return;
        };

        if let Some(entry) = self.entry_by_id(&shell_id) {
            match action {
                BackgroundShellControlAction::BackgroundRequest => {
                    let running = matches!(
                        entry.state.status,
                        BackgroundShellStatus::Pending | BackgroundShellStatus::Running
                    );
                    if !running
                        || !matches!(entry.state.start_mode, BackgroundShellStartMode::Foreground)
                    {
                        self.status_message =
                            Some("Process is already running in the background".to_string());
                        return;
                    }
                }
                BackgroundShellControlAction::Kill => {
                    if matches!(
                        entry.state.status,
                        BackgroundShellStatus::Completed | BackgroundShellStatus::Failed
                    ) {
                        self.status_message = Some("Process has already finished".to_string());
                        return;
                    }
                }
                BackgroundShellControlAction::Resume => {
                    if !matches!(
                        entry.state.status,
                        BackgroundShellStatus::Completed | BackgroundShellStatus::Failed
                    ) {
                        self.status_message = Some(
                            "Resume is only available for completed or failed shells".to_string(),
                        );
                        return;
                    }
                }
            }
        }

        let action_for_event = action.clone();
        self.app_event_tx
            .send(AppEvent::CodexOp(Op::BackgroundShellControl {
                shell_id: shell_id.clone(),
                action: action_for_event,
            }));
        let status = match action {
            BackgroundShellControlAction::Kill => "Requested kill".to_string(),
            BackgroundShellControlAction::Resume => "Requested resume".to_string(),
            BackgroundShellControlAction::BackgroundRequest => "Requested background".to_string(),
        };
        self.status_message = Some(format!("{status} for {shell_id}"));
    }

    fn toggle_diagnostics(&mut self) {
        match &self.view {
            ShellPanelView::Detail { shell_id, .. } => {
                self.view = ShellPanelView::Diagnostic {
                    shell_id: shell_id.clone(),
                    scroll: 0,
                };
            }
            ShellPanelView::Diagnostic { shell_id, .. } => {
                self.view = ShellPanelView::Detail {
                    shell_id: shell_id.clone(),
                    scroll: 0,
                };
            }
            ShellPanelView::List => {}
        }
    }

    fn adjust_scroll(&mut self, delta: isize) {
        match &mut self.view {
            ShellPanelView::Detail { scroll, .. } | ShellPanelView::Diagnostic { scroll, .. } => {
                let current = *scroll as isize + delta;
                *scroll = current.max(0) as usize;
            }
            ShellPanelView::List => {}
        }
    }

    fn copy_logs_to_temp(&mut self) {
        let shell_id = match &self.view {
            ShellPanelView::Detail { shell_id, .. }
            | ShellPanelView::Diagnostic { shell_id, .. } => shell_id.clone(),
            ShellPanelView::List => return,
        };
        if let Some(entry) = self.entry_by_id(&shell_id) {
            let mut builder = TempFileBuilder::new();
            builder.prefix("codex-shell-panel-").suffix(".log");
            match builder.tempfile() {
                Ok(mut file) => {
                    let (lines, truncated) = if let Some(tail) = entry.state.tail.as_ref() {
                        (tail.lines.clone(), tail.truncated)
                    } else if let Some(last) = entry.state.last_log.as_ref() {
                        (last.clone(), false)
                    } else {
                        (Vec::new(), false)
                    };
                    let mut payload = if lines.is_empty() {
                        "(no output)\n".to_string()
                    } else {
                        format!("{}\n", lines.join("\n"))
                    };
                    if truncated {
                        payload.push_str("\n… output truncated (~2KiB cap)\n");
                    }
                    if let Err(err) = file.write_all(payload.as_bytes()) {
                        self.status_message = Some(format!("Failed to copy log: {err}"));
                        return;
                    }
                    match file.keep() {
                        Ok((_file, path)) => {
                            self.status_message = Some(format!("Log copied to {}", path.display()));
                        }
                        Err(err) => {
                            self.status_message =
                                Some(format!("Failed to persist log copy: {err}"));
                        }
                    }
                }
                Err(err) => {
                    self.status_message = Some(format!("Failed to copy log: {err}"));
                }
            }
        }
    }

    fn entry_by_id(&self, shell_id: &str) -> Option<ShellPanelEntry> {
        for handle in &self.cards {
            let card = handle.lock();
            if card.state.shell_id == shell_id {
                return Some(ShellPanelEntry::from(&card));
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellPanelTab {
    Running,
    Completed,
    Failed,
}

impl ShellPanelTab {
    const ALL: [ShellPanelTab; 3] = [
        ShellPanelTab::Running,
        ShellPanelTab::Completed,
        ShellPanelTab::Failed,
    ];
    const COUNT: usize = 3;

    fn index(&self) -> usize {
        match self {
            ShellPanelTab::Running => 0,
            ShellPanelTab::Completed => 1,
            ShellPanelTab::Failed => 2,
        }
    }

    fn prev(&self) -> Self {
        match self {
            ShellPanelTab::Running => ShellPanelTab::Failed,
            ShellPanelTab::Completed => ShellPanelTab::Running,
            ShellPanelTab::Failed => ShellPanelTab::Completed,
        }
    }

    fn next(&self) -> Self {
        match self {
            ShellPanelTab::Running => ShellPanelTab::Completed,
            ShellPanelTab::Completed => ShellPanelTab::Failed,
            ShellPanelTab::Failed => ShellPanelTab::Running,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ShellPanelTab::Running => "RUNNING",
            ShellPanelTab::Completed => "COMPLETED",
            ShellPanelTab::Failed => "FAILED",
        }
    }

    fn from_status(status: BackgroundShellStatus) -> Self {
        match status {
            BackgroundShellStatus::Pending | BackgroundShellStatus::Running => {
                ShellPanelTab::Running
            }
            BackgroundShellStatus::Completed => ShellPanelTab::Completed,
            BackgroundShellStatus::Failed => ShellPanelTab::Failed,
        }
    }
}

#[derive(Clone, Debug)]
struct ShellPanelEntry {
    state: ShellState,
}

impl ShellPanelEntry {
    fn from(data: &ShellCardData) -> Self {
        Self {
            state: data.state.clone(),
        }
    }

    fn shell_id(&self) -> &str {
        &self.state.shell_id
    }

    fn label(&self) -> String {
        shell_label_for_display(&self.state)
    }

    fn status_icon(&self) -> Span<'static> {
        match self.state.status {
            BackgroundShellStatus::Pending | BackgroundShellStatus::Running => "●".green(),
            BackgroundShellStatus::Completed => "●".gray(),
            BackgroundShellStatus::Failed => "●".red(),
        }
    }

    fn status_label(&self) -> &'static str {
        match self.state.status {
            BackgroundShellStatus::Pending => "pending",
            BackgroundShellStatus::Running => "running",
            BackgroundShellStatus::Completed => "completed",
            BackgroundShellStatus::Failed => "failed",
        }
    }

    fn start_mode_label(&self) -> &'static str {
        match self.state.start_mode {
            BackgroundShellStartMode::Foreground => "foreground",
            BackgroundShellStartMode::Background => "background",
        }
    }

    fn info_text(&self) -> String {
        if let Some(reason) = &self.state.reason
            && !reason.is_empty()
        {
            return reason.clone();
        }
        if let Some(line) = self.state.tail.as_ref().and_then(|tail| tail.lines.last())
            && !line.is_empty()
        {
            return line.clone();
        }
        format_shell_command(&self.state.command)
    }
}

fn describe_actor(actor: BackgroundShellEndedBy) -> &'static str {
    match actor {
        BackgroundShellEndedBy::Agent => "agent",
        BackgroundShellEndedBy::User => "user",
        BackgroundShellEndedBy::System => "system",
    }
}

#[derive(Clone, Debug)]
enum ShellPanelView {
    List,
    Detail { shell_id: String, scroll: usize },
    Diagnostic { shell_id: String, scroll: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    use tokio::sync::mpsc::unbounded_channel;

    fn overlay() -> ShellPanelOverlay {
        let (tx, _rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        let card = Arc::new(Mutex::new(ShellCardData::new("shell-test".into())));
        ShellPanelOverlay::new(vec![card], None, sender)
    }

    #[test]
    fn esc_release_closes_overlay_without_press() {
        let mut overlay = overlay();
        let mut esc_release = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        esc_release.kind = KeyEventKind::Release;
        overlay.handle_key_for_test(esc_release);
        assert!(overlay.is_done());
    }

    #[test]
    fn esc_press_then_release_requires_second_press() {
        let mut overlay = overlay();
        overlay.view = ShellPanelView::Detail {
            shell_id: "shell-test".into(),
            scroll: 0,
        };
        overlay.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(overlay.view, ShellPanelView::List));
        assert!(!overlay.is_done());

        let mut esc_release = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        esc_release.kind = KeyEventKind::Release;
        overlay.handle_key_for_test(esc_release);
        assert!(!overlay.is_done());

        overlay.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(overlay.is_done());
    }
}
