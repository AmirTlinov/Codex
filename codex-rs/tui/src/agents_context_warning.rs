use std::io::Result;

use crate::key_hint;
use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt as _;
use crate::selection_list::selection_option_row;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;
use tokio_stream::StreamExt;

pub(crate) struct AgentsContextWarningParams {
    pub tokens: usize,
    pub percent_of_window: Option<f64>,
    pub truncated: bool,
    pub global_context_path: String,
    pub project_context_path: Option<String>,
    pub global_entry_count: usize,
    pub project_entry_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentsContextDecision {
    Continue,
    DisableForSession,
    ShowPathsAndExit,
    RequestCompression,
    ManageEntries,
}

pub(crate) async fn run_agents_context_warning(
    tui: &mut Tui,
    params: AgentsContextWarningParams,
) -> Result<AgentsContextDecision> {
    let mut screen = AgentsContextWarningScreen::new(tui.frame_requester(), params);
    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&screen, frame.area());
    })?;

    let mut events = tui.event_stream().fuse();

    while !screen.is_done() {
        let Some(event) = events.next().await else {
            break;
        };
        match event {
            TuiEvent::Key(key_event) => screen.handle_key(key_event),
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    frame.render_widget_ref(&screen, frame.area());
                })?;
            }
        }
    }

    Ok(screen
        .selection()
        .unwrap_or(WarningSelection::Continue)
        .into())
}

struct AgentsContextWarningScreen {
    request_frame: FrameRequester,
    params: AgentsContextWarningParams,
    highlighted: WarningSelection,
    selection: Option<WarningSelection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WarningSelection {
    Continue,
    DisableForSession,
    ShowPathsAndExit,
    RequestCompression,
    ManageEntries,
}

impl AgentsContextWarningScreen {
    fn new(request_frame: FrameRequester, params: AgentsContextWarningParams) -> Self {
        Self {
            request_frame,
            params,
            highlighted: WarningSelection::Continue,
            selection: None,
        }
    }

    fn handle_key(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            self.select(WarningSelection::Continue);
            return;
        }
        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => self.set_highlight(self.highlighted.prev()),
            KeyCode::Down | KeyCode::Char('j') => self.set_highlight(self.highlighted.next()),
            KeyCode::Char('1') => self.select(WarningSelection::Continue),
            KeyCode::Char('2') => self.select(WarningSelection::DisableForSession),
            KeyCode::Char('3') => self.select(WarningSelection::ShowPathsAndExit),
            KeyCode::Char('4') => self.select(WarningSelection::RequestCompression),
            KeyCode::Char('5') => self.select(WarningSelection::ManageEntries),
            KeyCode::Enter => self.select(self.highlighted),
            KeyCode::Esc => self.select(WarningSelection::Continue),
            _ => {}
        }
    }

    fn set_highlight(&mut self, highlight: WarningSelection) {
        if self.highlighted != highlight {
            self.highlighted = highlight;
            self.request_frame.schedule_frame();
        }
    }

    fn select(&mut self, selection: WarningSelection) {
        self.highlighted = selection;
        self.selection = Some(selection);
        self.request_frame.schedule_frame();
    }

    fn is_done(&self) -> bool {
        self.selection.is_some()
    }

    fn selection(&self) -> Option<WarningSelection> {
        self.selection
    }
}

impl WarningSelection {
    fn next(self) -> Self {
        match self {
            WarningSelection::Continue => WarningSelection::DisableForSession,
            WarningSelection::DisableForSession => WarningSelection::ShowPathsAndExit,
            WarningSelection::ShowPathsAndExit => WarningSelection::RequestCompression,
            WarningSelection::RequestCompression => WarningSelection::ManageEntries,
            WarningSelection::ManageEntries => WarningSelection::Continue,
        }
    }

    fn prev(self) -> Self {
        match self {
            WarningSelection::Continue => WarningSelection::RequestCompression,
            WarningSelection::DisableForSession => WarningSelection::Continue,
            WarningSelection::ShowPathsAndExit => WarningSelection::DisableForSession,
            WarningSelection::RequestCompression => WarningSelection::ShowPathsAndExit,
            WarningSelection::ManageEntries => WarningSelection::RequestCompression,
        }
    }
}

impl From<WarningSelection> for AgentsContextDecision {
    fn from(value: WarningSelection) -> Self {
        match value {
            WarningSelection::Continue => AgentsContextDecision::Continue,
            WarningSelection::DisableForSession => AgentsContextDecision::DisableForSession,
            WarningSelection::ShowPathsAndExit => AgentsContextDecision::ShowPathsAndExit,
            WarningSelection::RequestCompression => AgentsContextDecision::RequestCompression,
            WarningSelection::ManageEntries => AgentsContextDecision::ManageEntries,
        }
    }
}

fn format_entry_count(count: usize) -> String {
    match count {
        1 => "1 file".to_string(),
        _ => format!("{count} files"),
    }
}

impl WidgetRef for &AgentsContextWarningScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let mut column = ColumnRenderable::new();

        column.push(Line::from(""));
        column.push(
            Line::from("Agents context warning".bold().magenta()).inset(Insets::tlbr(0, 0, 0, 0)),
        );
        column.push("");

        let tokens = super::format_token_count(self.params.tokens as u64);
        let usage_line = match self.params.percent_of_window {
            Some(percent) => {
                let pct = super::format_percent(percent);
                format!("Estimated usage: {pct} (~{tokens} tokens)")
            }
            None => format!("Estimated usage: ~{tokens} tokens"),
        };
        column.push(Line::from(usage_line).dim());

        if self.params.truncated {
            column.push(Line::from("Context was truncated to fit the 1 MiB limit.").dim());
        }

        column.push("");
        let global_label = format!(
            "Global context ({})",
            format_entry_count(self.params.global_entry_count)
        )
        .dim();
        column.push(Line::from(vec![
            global_label,
            ": ".into(),
            self.params.global_context_path.clone().into(),
        ]));
        let project_label = format!(
            "Project context ({})",
            format_entry_count(self.params.project_entry_count)
        )
        .dim();
        column.push(Line::from(match &self.params.project_context_path {
            Some(path) => vec![project_label, ": ".into(), path.clone().into()],
            None => vec![project_label, ": ".into(), "not detected".dim()],
        }));

        column.push("");

        column.push(selection_option_row(
            0,
            "Continue with this context".to_string(),
            self.highlighted == WarningSelection::Continue,
        ));
        column.push(selection_option_row(
            1,
            "Disable the context for this run".to_string(),
            self.highlighted == WarningSelection::DisableForSession,
        ));
        column.push(selection_option_row(
            2,
            "Show context directories and exit".to_string(),
            self.highlighted == WarningSelection::ShowPathsAndExit,
        ));
        column.push(selection_option_row(
            3,
            "Ask Codex to compress the context before continuing".to_string(),
            self.highlighted == WarningSelection::RequestCompression,
        ));
        column.push(selection_option_row(
            4,
            "Manage which context files are loaded".to_string(),
            self.highlighted == WarningSelection::ManageEntries,
        ));

        column.push("");
        column.push(
            Line::from(vec![
                key_hint::plain(KeyCode::Enter).into(),
                " to confirm ".dim(),
                "    ".into(),
                key_hint::plain(KeyCode::Up).into(),
                "/".into(),
                key_hint::plain(KeyCode::Down).into(),
                " to choose ".dim(),
                "    ".into(),
                "1-5".dim(),
                " to jump".dim(),
            ])
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );

        column.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use crate::tui::FrameRequester;
    use ratatui::Terminal;

    fn sample_params() -> AgentsContextWarningParams {
        AgentsContextWarningParams {
            tokens: 72_000,
            percent_of_window: Some(96.0),
            truncated: true,
            global_context_path: "/home/user/.agents/context".to_string(),
            project_context_path: Some("/work/project/.agents/context".to_string()),
            global_entry_count: 7,
            project_entry_count: 2,
        }
    }

    #[test]
    fn renders_warning_modal_snapshot() {
        let screen = AgentsContextWarningScreen::new(FrameRequester::test_dummy(), sample_params());
        let mut terminal = Terminal::new(VT100Backend::new(80, 16)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget_ref(&screen, frame.area()))
            .expect("render warning");
        insta::assert_snapshot!("agents_context_warning_modal", terminal.backend());
    }
}
