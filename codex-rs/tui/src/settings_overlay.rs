use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::render::renderable::Renderable;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Clone, Copy)]
enum SettingsItemKind {
    AutoAttachAgentsContext,
    WrapLongWords,
    DesktopNotifications,
    ManageAgentsContext,
    Close,
}

#[derive(Clone, Copy)]
pub(crate) struct SettingsState {
    pub auto_attach_agents_context: bool,
    pub wrap_break_long_words: bool,
    pub agents_context_available: bool,
    pub notifications_enabled: bool,
    pub notifications_custom: bool,
}

pub(crate) struct SettingsOverlay {
    list: ListSelectionView,
    items: Vec<SettingsItemKind>,
    state: SettingsState,
    app_event_tx: AppEventSender,
}

impl SettingsOverlay {
    pub fn new(app_event_tx: AppEventSender, state: SettingsState) -> Self {
        let items = vec![
            SettingsItemKind::AutoAttachAgentsContext,
            SettingsItemKind::WrapLongWords,
            SettingsItemKind::DesktopNotifications,
            SettingsItemKind::ManageAgentsContext,
            SettingsItemKind::Close,
        ];
        let list = Self::build_list(&items, state, app_event_tx.clone());
        Self {
            list,
            items,
            state,
            app_event_tx,
        }
    }

    fn build_list(
        items: &[SettingsItemKind],
        state: SettingsState,
        app_event_tx: AppEventSender,
    ) -> ListSelectionView {
        let mut selection_items = Vec::with_capacity(items.len());
        for kind in items {
            selection_items.push(match kind {
                SettingsItemKind::AutoAttachAgentsContext => {
                    let status = if state.auto_attach_agents_context {
                        "On"
                    } else {
                        "Off"
                    };
                    let mut desc = "Attach AGENTS.md automatically when Codex starts".to_string();
                    if !state.agents_context_available {
                        desc.push_str(" (no context files detected)");
                    }
                    SelectionItem {
                        name: format!("Auto-attach agents context ({status})"),
                        description: Some(desc),
                        selected_description: Some(
                            "Toggle whether Codex injects AGENTS.md without running /context."
                                .to_string(),
                        ),
                        dismiss_on_select: false,
                        ..Default::default()
                    }
                }
                SettingsItemKind::WrapLongWords => {
                    let status = if state.wrap_break_long_words {
                        "Enabled"
                    } else {
                        "Disabled"
                    };
                    SelectionItem {
                        name: format!("Wrap long tokens ({status})"),
                        description: Some(
                            "Break extremely long tokens when rendering transcript output."
                                .to_string(),
                        ),
                        selected_description: Some(
                            "Disable to keep long tokens intact (useful for diffs/base64)."
                                .to_string(),
                        ),
                        dismiss_on_select: false,
                        ..Default::default()
                    }
                }
                SettingsItemKind::ManageAgentsContext => SelectionItem {
                    name: "Open agents context manager".to_string(),
                    description: Some("Same as running `/context`.".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SettingsItemKind::DesktopNotifications => {
                    let status = if state.notifications_custom {
                        "Custom"
                    } else if state.notifications_enabled {
                        "On"
                    } else {
                        "Off"
                    };
                    SelectionItem {
                        name: format!("Desktop notifications ({status})"),
                        description: Some(
                            "Enable terminal-native notifications for agent turns and approvals.".to_string(),
                        ),
                        selected_description: if state.notifications_custom {
                            Some(
                                "Currently customized via `tui.notifications`; toggling here resets to on/off.".to_string(),
                            )
                        } else {
                            Some("Toggle `/settings` notifications without editing config.toml.".to_string())
                        },
                        dismiss_on_select: false,
                        ..Default::default()
                    }
                }
                SettingsItemKind::Close => SelectionItem {
                    name: "Close settings".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            });
        }

        let params = SelectionViewParams {
            footer_hint: Some(Line::from("Enter to toggle · Esc to close".dim())),
            items: selection_items,
            header: Box::new(Line::from("Settings".bold())),
            ..Default::default()
        };
        ListSelectionView::new(params, app_event_tx)
    }

    fn rebuild_list(&mut self) {
        self.list = Self::build_list(&self.items, self.state, self.app_event_tx.clone());
    }

    fn handle_selection(&mut self) {
        if let Some(idx) = self.list.take_last_selected_index() {
            match self.items.get(idx).copied() {
                Some(SettingsItemKind::AutoAttachAgentsContext) => {
                    self.state.auto_attach_agents_context = !self.state.auto_attach_agents_context;
                    self.app_event_tx
                        .send(AppEvent::SetAutoAttachAgentsContext {
                            enabled: self.state.auto_attach_agents_context,
                            persist: true,
                        });
                    self.rebuild_list();
                }
                Some(SettingsItemKind::WrapLongWords) => {
                    self.state.wrap_break_long_words = !self.state.wrap_break_long_words;
                    self.app_event_tx.send(AppEvent::SetWrapBreakLongWords {
                        enabled: self.state.wrap_break_long_words,
                        persist: true,
                    });
                    self.rebuild_list();
                }
                Some(SettingsItemKind::DesktopNotifications) => {
                    let next = !self.state.notifications_enabled;
                    self.state.notifications_enabled = next;
                    self.state.notifications_custom = false;
                    self.app_event_tx.send(AppEvent::SetDesktopNotifications {
                        enabled: next,
                        persist: true,
                    });
                    self.rebuild_list();
                }
                Some(SettingsItemKind::ManageAgentsContext) => {
                    self.app_event_tx.send(AppEvent::OpenAgentsContextManager);
                }
                Some(SettingsItemKind::Close) | None => {}
            }
        }
    }
}

impl Renderable for SettingsOverlay {
    fn desired_height(&self, width: u16) -> u16 {
        self.list.desired_height(width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }
}

impl BottomPaneView for SettingsOverlay {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        self.list.handle_key_event(key_event);
        self.handle_selection();
    }

    fn is_complete(&self) -> bool {
        self.list.is_complete()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.list.on_ctrl_c()
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        let changed = self.list.handle_paste(pasted);
        if changed {
            self.handle_selection();
        }
        changed
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.list.cursor_pos(area)
    }
}
