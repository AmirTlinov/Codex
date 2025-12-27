use std::sync::Arc;
use std::sync::Mutex;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use crate::key_hint;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;

use codex_core::protocol::WorkbenchContextSnapshotEvent;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;

#[derive(Debug, Default)]
pub(crate) struct ContextDebugState {
    pub snapshot: Option<WorkbenchContextSnapshotEvent>,
}

pub(crate) type ContextDebugSharedState = Arc<Mutex<ContextDebugState>>;

pub(crate) struct ContextDebugView {
    state: ContextDebugSharedState,
    scroll: ScrollState,
    complete: bool,
    header: Box<dyn Renderable>,
    footer_hint: Line<'static>,
}

impl ContextDebugView {
    pub(crate) fn new(state: ContextDebugSharedState) -> Self {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Context diagnostics".bold()));
        header.push(Line::from(
            "Shows what is sent to the model (pinned context + transcript + memory overlay).".dim(),
        ));

        Self {
            state,
            scroll: ScrollState::new(),
            complete: false,
            header: Box::new(header),
            footer_hint: context_debug_hint_line(),
        }
    }

    fn rows(&self) -> Vec<GenericDisplayRow> {
        let snapshot_opt = self
            .state
            .lock()
            .ok()
            .and_then(|guard| guard.snapshot.clone());

        let Some(snapshot) = snapshot_opt else {
            return vec![GenericDisplayRow {
                name: "loading…".to_string(),
                ..Default::default()
            }];
        };

        let transcript = &snapshot.transcript;
        let mut rows = vec![
            section_row("Features"),
            kv_row("lego_memory", bool_on_off(snapshot.features.lego_memory)),
            kv_row(
                "workbench_transcript",
                bool_on_off(snapshot.features.workbench_transcript),
            ),
            blank_row(),
            section_row("Transcript"),
            kv_row("history items", transcript.total_items.to_string()),
            kv_row(
                "history user msgs",
                transcript.total_user_messages.to_string(),
            ),
            kv_row("trimmed items", transcript.trimmed_items.to_string()),
            kv_row(
                "effective items",
                transcript.effective_items_total.to_string(),
            ),
        ];

        if snapshot.features.workbench_transcript {
            rows.push(kv_row(
                "tail limit (user msgs)",
                transcript.tail_user_messages_limit.to_string(),
            ));
            rows.push(kv_row(
                "tail start index",
                transcript.tail_start_index.to_string(),
            ));
        }

        let pinned = &transcript.pinned;
        rows.push(kv_row(
            "pinned",
            format!(
                "dev={}, user_instr={}, skills={}, env={}",
                bool_short(pinned.developer_instructions),
                bool_short(pinned.user_instructions),
                pinned.skill_instructions,
                bool_short(pinned.environment_context),
            ),
        ));

        if transcript.kept_items_truncated {
            rows.push(kv_row(
                "preview",
                format!("showing first {}", transcript.kept_items.len()),
            ));
        }

        rows.push(blank_row());
        rows.push(section_row("Items (preview)"));
        for item in &transcript.kept_items {
            rows.push(GenericDisplayRow {
                name: format!("{:>3} {}", item.index, kind_tag(item.kind)),
                description: Some(format!("{}: {}", item.role, item.preview)),
                ..Default::default()
            });
        }

        rows.push(blank_row());
        rows.push(section_row("Memory"));
        let memory = &snapshot.memory;
        rows.push(kv_row("enabled", bool_on_off(memory.enabled)));
        rows.push(kv_row(
            "working_set_budget",
            memory.working_set_token_budget.to_string(),
        ));
        rows.push(kv_row("staleness", memory.staleness_mode.clone()));

        if memory.enabled {
            rows.push(kv_row("project_id", memory.project_id.clone()));
            rows.push(kv_row("root_dir", memory.root_dir.clone()));
            rows.push(kv_row("blocks_total", memory.blocks_total.to_string()));
            rows.push(kv_row(
                "blocks_included",
                memory.blocks_included.len().to_string(),
            ));
        }

        if !memory.error.is_empty() {
            rows.push(kv_row("error", memory.error.clone()));
        }

        if memory.enabled && !memory.blocks_included.is_empty() {
            rows.push(blank_row());
            rows.push(section_row("Blocks (included)"));
            for block in &memory.blocks_included {
                rows.push(GenericDisplayRow {
                    name: block.id.clone(),
                    description: Some(format!(
                        "kind={}, status={}, priority={}, repr={}",
                        block.kind, block.status, block.priority, block.representation
                    )),
                    ..Default::default()
                });
            }
        }

        rows
    }

    fn move_up(&mut self) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        self.scroll.move_up_wrap(len);
    }

    fn move_down(&mut self) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        self.scroll.move_down_wrap(len);
    }
}

impl BottomPaneView for ContextDebugView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_down(),
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            _ => {}
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

impl Renderable for ContextDebugView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        Block::default()
            .style(user_message_style())
            .render(content_area, buf);

        let header_height = self
            .header
            .desired_height(content_area.width.saturating_sub(4));
        let rows = self.rows();
        let rows_width = content_area.width.saturating_sub(2);
        let rows_height = measure_rows_height(
            &rows,
            &self.scroll,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );

        let [header_area, _, list_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(rows_height),
        ])
        .areas(content_area.inset(Insets::vh(1, 2)));

        self.header.render(header_area, buf);

        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: rows_width.max(1),
                height: list_area.height,
            };
            render_rows(
                render_area,
                buf,
                &rows,
                &self.scroll,
                MAX_POPUP_ROWS,
                "loading…",
            );
        }

        let hint_area = Rect {
            x: footer_area.x + 2,
            y: footer_area.y,
            width: footer_area.width.saturating_sub(2),
            height: footer_area.height,
        };
        self.footer_hint.clone().dim().render(hint_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let rows = self.rows();
        let rows_width = width.saturating_sub(2);
        let rows_height = measure_rows_height(
            &rows,
            &self.scroll,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );

        // Subtract 4 for the padding on the left and right of the header.
        let mut height = self.header.desired_height(width.saturating_sub(4));
        height = height.saturating_add(rows_height + 3);
        height.saturating_add(1)
    }
}

fn context_debug_hint_line() -> Line<'static> {
    Line::from(vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Esc).into(),
        " to close · ".into(),
        key_hint::plain(KeyCode::Up).into(),
        "/".into(),
        key_hint::plain(KeyCode::Down).into(),
        " to scroll".into(),
    ])
}

fn section_row(title: &str) -> GenericDisplayRow {
    GenericDisplayRow {
        name: format!("== {title} =="),
        ..Default::default()
    }
}

fn blank_row() -> GenericDisplayRow {
    GenericDisplayRow {
        name: String::new(),
        ..Default::default()
    }
}

fn kv_row(name: &str, value: String) -> GenericDisplayRow {
    GenericDisplayRow {
        name: format!("{name}:"),
        description: Some(value),
        ..Default::default()
    }
}

fn bool_on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_string()
}

fn bool_short(value: bool) -> &'static str {
    if value { "y" } else { "n" }
}

fn kind_tag(kind: codex_core::protocol::WorkbenchContextItemKind) -> &'static str {
    match kind {
        codex_core::protocol::WorkbenchContextItemKind::UserMessage => "user",
        codex_core::protocol::WorkbenchContextItemKind::AssistantMessage => "assistant",
        codex_core::protocol::WorkbenchContextItemKind::ToolCall => "tool_call",
        codex_core::protocol::WorkbenchContextItemKind::ToolOutput => "tool_out",
        codex_core::protocol::WorkbenchContextItemKind::Other => "other",
    }
}
