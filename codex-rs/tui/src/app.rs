use crate::UpdateAction;
use crate::agents_context_manager::AgentsContextManagerConfig;
use crate::agents_context_manager::AgentsContextManagerOutcome;
use crate::agents_context_manager::run_agents_context_manager;
use crate::agents_context_warning::AgentsContextDecision;
use crate::agents_context_warning::AgentsContextWarningParams;
use crate::agents_context_warning::run_agents_context_warning;
use crate::app_backtrack::BacktrackState;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalRequest;
use crate::chatwidget::ChatWidget;
use crate::diff_render::DiffSummary;
use crate::exec_cell::ExecCell;
use crate::exec_cell::ExecStreamAction;
use crate::exec_cell::ExecStreamKind;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::file_search::FileSearchManager;
use crate::format_token_count;
#[cfg(not(debug_assertions))]
use crate::get_update_action;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::live_exec::LiveExecState;
use crate::mcp::McpManagerEntry;
use crate::mcp::McpManagerState;
use crate::mcp::McpWizardDraft;
use crate::mcp::McpWizardInit;
use crate::pager_overlay::Overlay;
use crate::process_manager::ProcessManagerEntry;
use crate::process_manager::ProcessOutputData;
use crate::process_manager::entry_and_data_from_output;
use crate::render::highlight::highlight_bash_to_lines;
use crate::resume_picker::ResumeSelection;
use crate::tui;
use crate::tui::TuiEvent;
use codex_ansi_escape::ansi_escape_line;
use codex_core::AuthManager;
use codex_core::CodexConversation;
use codex_core::ConversationManager;
use codex_core::UnifiedExecOutputWindow;
use codex_core::config::AgentsSource;
use codex_core::config::Config;
use codex_core::config::load_global_mcp_servers;
use codex_core::config::persist_model_selection;
use codex_core::config::set_auto_attach_agents_context;
use codex_core::config::set_hide_full_access_warning;
use codex_core::config::set_tui_notifications_enabled;
use codex_core::config::set_wrap_break_long_words;
use codex_core::config_types::McpServerConfig;
use codex_core::config_types::Notifications;
use codex_core::mcp::registry::McpRegistry;
use codex_core::mcp::templates::TemplateCatalog;
use codex_core::model_family::find_family_for_model;
use codex_core::protocol::Op;
use codex_core::protocol::SessionSource;
use codex_core::protocol::TokenUsage;
use codex_core::protocol_config_types::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::ConversationId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::eyre;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub conversation_id: Option<ConversationId>,
    pub update_action: Option<UpdateAction>,
}

const LIVE_EXEC_POLL_INTERVAL: Duration = Duration::from_millis(350);

#[derive(Debug, Default)]
struct LiveExecPollState {
    active: bool,
    task: Option<JoinHandle<()>>,
}

pub(crate) struct App {
    pub(crate) server: Arc<ConversationManager>,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    pub(crate) auth_manager: Arc<AuthManager>,

    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    pub(crate) live_exec: Rc<RefCell<LiveExecState>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,
    live_exec_poll: LiveExecPollState,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        config: Config,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        resume_selection: ResumeSelection,
        feedback: codex_feedback::CodexFeedback,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);

        let conversation_manager = Arc::new(ConversationManager::new(
            auth_manager.clone(),
            SessionSource::Cli,
        ));

        let enhanced_keys_supported = tui.enhanced_keys_supported();

        let chat_widget = match resume_selection {
            ResumeSelection::StartFresh | ResumeSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_prompt: initial_prompt.clone(),
                    initial_images: initial_images.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                };
                ChatWidget::new(init, conversation_manager.clone())
            }
            ResumeSelection::Resume(path) => {
                let resumed = conversation_manager
                    .resume_conversation_from_rollout(
                        config.clone(),
                        path.clone(),
                        auth_manager.clone(),
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to resume session from {}", path.display())
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_prompt: initial_prompt.clone(),
                    initial_images: initial_images.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                };
                ChatWidget::new_from_existing(
                    init,
                    resumed.conversation,
                    resumed.session_configured,
                )
            }
        };

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let live_exec_root = config.cwd.clone();
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let mut app = Self {
            server: conversation_manager,
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            file_search,
            live_exec: Rc::new(RefCell::new(LiveExecState::new(live_exec_root))),
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            feedback: feedback.clone(),
            pending_update_action: None,
            live_exec_poll: LiveExecPollState::default(),
        };

        #[cfg(not(debug_assertions))]
        if let Some(latest_version) = upgrade_version {
            app.handle_event(
                tui,
                AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                    latest_version,
                    get_update_action(),
                ))),
            )
            .await?;
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        while select! {
            Some(event) = app_event_rx.recv() => {
                app.handle_event(tui, event).await?
            }
            Some(event) = tui_events.next() => {
                app.handle_tui_event(tui, event).await?
            }
        } {}
        tui.terminal.clear()?;
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            conversation_id: app.chat_widget.conversation_id(),
            update_action: app.pending_update_action,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<bool> {
        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw => {
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(true);
                    }
                    tui.draw(
                        self.chat_widget.desired_height(tui.terminal.size()?.width),
                        |frame| {
                            frame.render_widget_ref(&self.chat_widget, frame.area());
                            if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
                                frame.set_cursor_position((x, y));
                            }
                        },
                    )?;
                }
            }
        }
        Ok(true)
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<bool> {
        match event {
            AppEvent::NewSession => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: self.config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: self.app_event_tx.clone(),
                    initial_prompt: None,
                    initial_images: Vec::new(),
                    enhanced_keys_supported: self.enhanced_keys_supported,
                    auth_manager: self.auth_manager.clone(),
                    feedback: self.feedback.clone(),
                };
                self.chat_widget = ChatWidget::new(init, self.server.clone());
                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_cell(cell.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_cells.push(cell.clone());
                let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
                if !display.is_empty() {
                    // Only insert a separating blank line for new cells that are not
                    // part of an ongoing stream. Streaming continuations should not
                    // accrue extra blank lines between chunks.
                    if !cell.is_stream_continuation() {
                        if self.has_emitted_history_lines {
                            display.insert(0, Line::from(""));
                        } else {
                            self.has_emitted_history_lines = true;
                        }
                    }
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    } else {
                        tui.insert_history_lines(display);
                    }
                }
            }
            AppEvent::LiveExecCommandBegin {
                call_id,
                command,
                cwd,
            } => {
                {
                    let mut state = self.live_exec.borrow_mut();
                    state.begin(call_id, command, cwd);
                }
                if let Some(overlay) = &mut self.overlay {
                    overlay.on_live_exec_state_updated();
                }
                if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
                    tui.frame_requester().schedule_frame();
                }
                self.enable_live_exec_polling(tui);
            }
            AppEvent::LiveExecOutputChunk { call_id, chunk } => {
                let updated = {
                    let mut state = self.live_exec.borrow_mut();
                    state.append_chunk(&call_id, &chunk)
                };
                if updated {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.on_live_exec_state_updated();
                    }
                    if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
                        tui.frame_requester().schedule_frame();
                    }
                }
            }
            AppEvent::LiveExecCommandFinished {
                call_id,
                exit_code,
                duration,
                aggregated_output,
            } => {
                {
                    let mut state = self.live_exec.borrow_mut();
                    state.finish(&call_id, exit_code, duration, aggregated_output);
                }
                if let Some(overlay) = &mut self.overlay {
                    overlay.on_live_exec_state_updated();
                }
                if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
                    tui.frame_requester().schedule_frame();
                }
                self.enable_live_exec_polling(tui);
            }
            AppEvent::LiveExecPromoted {
                call_id,
                shell_id,
                initial_output,
                description,
            } => {
                let shell_label = shell_id.clone();
                {
                    let mut state = self.live_exec.borrow_mut();
                    state.promote(&call_id, shell_id, initial_output);
                }
                if let Some(overlay) = &mut self.overlay {
                    overlay.on_live_exec_state_updated();
                }
                if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
                    tui.frame_requester().schedule_frame();
                }
                self.enable_live_exec_polling(tui);
                if let Some(desc) = description.as_deref() {
                    let trimmed = desc.trim();
                    if !trimmed.is_empty() {
                        self.chat_widget.add_info_message(
                            format!("Background shell `{shell_label}`: {trimmed}"),
                            None,
                        );
                    }
                }
            }
            AppEvent::LiveExecPollTick => {
                self.maybe_poll_live_exec(tui);
            }
            AppEvent::EnsureLiveExecPolling => {
                self.enable_live_exec_polling(tui);
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::CodexEvent(event) => {
                self.chat_widget.handle_codex_event(event);
            }
            AppEvent::ConversationHistory(ev) => {
                self.on_conversation_history_for_backtrack(tui, ev).await?;
            }
            AppEvent::ExitRequest => {
                return Ok(false);
            }
            AppEvent::CodexOp(op) => self.chat_widget.submit_op(op),
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                self.disable_live_exec_polling();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::StartFileSearch(query) => {
                if !query.is_empty() {
                    self.file_search.on_user_query(query);
                }
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(&model);
                self.config.model = model.clone();
                if let Some(family) = find_family_for_model(&model) {
                    self.config.model_family = family;
                }
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenFullAccessConfirmation { preset } => {
                self.chat_widget.open_full_access_confirmation(preset);
            }
            AppEvent::OpenFeedbackNote {
                category,
                include_logs,
            } => {
                self.chat_widget.open_feedback_note(category, include_logs);
            }
            AppEvent::OpenFeedbackConsent { category } => {
                self.chat_widget.open_feedback_consent(category);
            }
            AppEvent::OpenAgentsContextManager => {
                let all_entries = match self.config.load_all_agents_context_entries() {
                    Ok(entries) => entries,
                    Err(err) => {
                        tracing::error!("failed to load agents context entries: {err}");
                        self.chat_widget.add_error_message(format!(
                            "Failed to load agents context entries: {err}"
                        ));
                        return Ok(true);
                    }
                };
                let enabled_paths: HashSet<String> = self
                    .config
                    .agents_context_entries
                    .iter()
                    .map(|entry| entry.relative_path.clone())
                    .collect();
                let hidden_paths = enabled_paths.clone();

                let manager_config = AgentsContextManagerConfig {
                    tools: self.config.agents_tools.clone(),
                    warning_tokens: self.config.agents_context_warning_tokens,
                    model_context_window: self.config.model_context_window,
                    global_agents_home: self.config.agents_home.clone(),
                    project_agents_home: self.config.project_agents_home.clone(),
                    cwd: self.config.cwd.clone(),
                    enabled_paths,
                    hidden_paths,
                    include_mode: self.chat_widget.agents_context_include_mode(),
                    existing_include: &self.config.agents_context_include,
                    existing_exclude: &self.config.agents_context_exclude,
                };
                match run_agents_context_manager(tui, all_entries, manager_config).await? {
                    AgentsContextManagerOutcome::Cancelled => {}
                    AgentsContextManagerOutcome::Applied { include, exclude } => {
                        match self.config.refresh_agents_context_entries(include, exclude) {
                            Ok(()) => {
                                self.chat_widget
                                    .apply_agents_context_from_config(&self.config);
                                let include_mode = !self.config.agents_context_include.is_empty();
                                self.chat_widget
                                    .set_agents_context_include_mode(include_mode);
                                let count = self.config.agents_context_entries.len();
                                let tokens = self.config.agents_context_prompt_tokens as u64;
                                if count > 0 && self.config.agents_context_prompt.is_some() {
                                    self.chat_widget.set_agents_context_active(true);
                                    let message = if count == 1 {
                                        format!(
                                            "Attached 1 agents context file (~{} tokens) to all future messages.",
                                            format_token_count(tokens)
                                        )
                                    } else {
                                        format!(
                                            "Attached {count} agents context files (~{} tokens) to all future messages.",
                                            format_token_count(tokens)
                                        )
                                    };
                                    self.chat_widget.add_info_message(
                                        message,
                                        Some(
                                            "Use `/context off` to stop including these files."
                                                .to_string(),
                                        ),
                                    );
                                    self.maybe_prompt_agents_context_warning(tui).await?;
                                } else {
                                    self.chat_widget.set_agents_context_active(false);
                                    self.chat_widget.add_info_message(
                                        "Agents context disabled for upcoming messages."
                                            .to_string(),
                                        Some("Use `/context` to attach context again.".to_string()),
                                    );
                                }
                            }
                            Err(err) => {
                                tracing::error!("failed to apply agents context filters: {err}");
                                self.chat_widget.add_error_message(format!(
                                    "Failed to apply agents context filters: {err}"
                                ));
                            }
                        }
                    }
                }
            }
            AppEvent::OpenMcpManager => {
                if let Err(err) = self.open_mcp_manager_overlay().await {
                    tracing::error!(error = %err, "failed to open MCP manager");
                    self.chat_widget
                        .add_error_message(format!("Failed to open MCP manager: {err}"));
                    self.chat_widget.add_mcp_output();
                }
            }
            AppEvent::OpenProcessManager => {
                if let Err(err) = self.open_process_manager_overlay().await {
                    tracing::error!(error = %err, "failed to open process manager overlay");
                    self.chat_widget
                        .add_error_message(format!("Failed to open process manager: {err}"));
                }
            }
            AppEvent::OpenUnifiedExecInputPrompt { session_id } => {
                self.chat_widget.open_unified_exec_input_prompt(session_id);
            }
            AppEvent::OpenUnifiedExecOutput { session_id } => {
                if let Err(err) = self.show_unified_exec_output(session_id, None).await {
                    tracing::error!(error = %err, "failed to load unified exec output");
                    self.chat_widget
                        .add_error_message(format!("Failed to load process output: {err}"));
                }
            }
            AppEvent::RefreshUnifiedExecOutput { session_id } => {
                if let Err(err) = self.show_unified_exec_output(session_id, None).await {
                    tracing::error!(error = %err, "failed to refresh unified exec output");
                    self.chat_widget
                        .add_error_message(format!("Failed to refresh process output: {err}"));
                }
            }
            AppEvent::LoadUnifiedExecOutputWindow { session_id, window } => {
                if let Err(err) = self
                    .show_unified_exec_output(session_id, Some(window))
                    .await
                {
                    tracing::error!(error = %err, "failed to load output window");
                    self.chat_widget.add_error_message(format!(
                        "Failed to load requested output window: {err}"
                    ));
                }
            }
            AppEvent::OpenUnifiedExecExportPrompt { session_id } => {
                let suggestion = self.default_export_suggestion(session_id);
                self.chat_widget
                    .open_unified_exec_export_prompt(session_id, suggestion);
            }
            AppEvent::SendUnifiedExecInput { session_id, input } => {
                if let Err(err) = self.send_unified_exec_input(session_id, input).await {
                    tracing::error!(error = %err, "failed to send unified exec input");
                    self.chat_widget
                        .add_error_message(format!("Failed to send input: {err}"));
                }
            }
            AppEvent::ExportUnifiedExecSessionLog {
                session_id,
                destination,
            } => {
                if let Err(err) = self.export_unified_exec_log(session_id, destination).await {
                    tracing::error!(error = %err, "failed to export unified exec log");
                    self.chat_widget
                        .add_error_message(format!("Failed to export session log: {err}"));
                }
            }
            AppEvent::KillUnifiedExecSession { session_id } => {
                if let Err(err) = self.kill_unified_exec_session(session_id).await {
                    tracing::error!(error = %err, "failed to kill unified exec session");
                    self.chat_widget
                        .add_error_message(format!("Failed to kill session: {err}"));
                } else if let Err(err) = self.open_process_manager_overlay().await {
                    tracing::error!(error = %err, "failed to refresh process manager");
                }
            }
            AppEvent::RemoveUnifiedExecSession { session_id } => {
                if let Err(err) = self.remove_unified_exec_session(session_id).await {
                    tracing::error!(error = %err, "failed to remove unified exec session");
                    self.chat_widget
                        .add_error_message(format!("Failed to remove session: {err}"));
                } else if let Err(err) = self.open_process_manager_overlay().await {
                    tracing::error!(error = %err, "failed to refresh process manager");
                }
            }
            AppEvent::ToggleExecStream {
                call_id,
                stream,
                action,
            } => {
                self.handle_exec_stream_toggle(tui, call_id, stream, action)
                    .await?;
            }
            AppEvent::OpenMcpWizard {
                template_id,
                draft,
                existing_name,
            } => {
                if let Err(err) = self
                    .open_mcp_wizard(template_id, draft, existing_name)
                    .await
                {
                    tracing::error!(error = %err, "failed to open MCP wizard");
                    self.chat_widget
                        .add_error_message(format!("Failed to open MCP wizard: {err}"));
                }
            }
            AppEvent::ApplyMcpWizard {
                draft,
                existing_name,
            } => {
                if let Err(err) = self.apply_mcp_wizard(draft, existing_name).await {
                    tracing::error!(error = %err, "failed to apply MCP wizard");
                    self.chat_widget
                        .add_error_message(format!("Failed to save MCP server: {err}"));
                }
            }
            AppEvent::ReloadMcpServers => {
                if let Err(err) = self.reload_mcp_servers().await {
                    tracing::error!(error = %err, "failed to reload MCP servers");
                    self.chat_widget
                        .add_error_message(format!("Failed to reload MCP servers: {err}"));
                }
            }
            AppEvent::RemoveMcpServer { name } => {
                if let Err(err) = self.remove_mcp_server(&name).await {
                    tracing::error!(error = %err, server = %name, "failed to remove MCP server");
                    self.chat_widget
                        .add_error_message(format!("Failed to remove MCP server `{name}`: {err}"));
                }
            }
            AppEvent::ShowWindowsAutoModeInstructions => {
                self.chat_widget.open_windows_auto_mode_instructions();
            }
            AppEvent::PersistModelSelection { model, effort } => {
                let profile = self.active_profile.as_deref();
                match persist_model_selection(&self.config.codex_home, profile, &model, effort)
                    .await
                {
                    Ok(()) => {
                        let effort_label = effort
                            .map(|eff| format!(" with {eff} reasoning"))
                            .unwrap_or_else(|| " with default reasoning".to_string());
                        if let Some(profile) = profile {
                            self.chat_widget.add_info_message(
                                format!(
                                    "Model changed to {model}{effort_label} for {profile} profile"
                                ),
                                None,
                            );
                        } else {
                            self.chat_widget.add_info_message(
                                format!("Model changed to {model}{effort_label}"),
                                None,
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.chat_widget.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                self.chat_widget.set_sandbox_policy(policy);
            }
            AppEvent::SetAutoAttachAgentsContext { enabled, persist } => {
                self.chat_widget.update_auto_attach_agents_context(enabled);
                self.config.auto_attach_agents_context = enabled;
                if persist
                    && let Err(err) = set_auto_attach_agents_context(
                        &self.config.codex_home,
                        self.active_profile.as_deref(),
                        enabled,
                    )
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist auto-attach agents context preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save agents context preference: {err}"
                    ));
                }
            }
            AppEvent::SetWrapBreakLongWords { enabled, persist } => {
                self.chat_widget.update_wrap_break_long_words(enabled);
                self.config.wrap_break_long_words = enabled;
                if persist
                    && let Err(err) = set_wrap_break_long_words(
                        &self.config.codex_home,
                        self.active_profile.as_deref(),
                        enabled,
                    )
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist wrap preference"
                    );
                    self.chat_widget
                        .add_error_message(format!("Failed to save wrapping preference: {err}"));
                }
            }
            AppEvent::SetDesktopNotifications { enabled, persist } => {
                self.chat_widget.update_notifications_enabled(enabled);
                self.config.tui_notifications = Notifications::Enabled(enabled);
                if persist
                    && let Err(err) = set_tui_notifications_enabled(
                        &self.config.codex_home,
                        self.active_profile.as_deref(),
                        enabled,
                    )
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist notification preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save notification preference: {err}"
                    ));
                }
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = set_hide_full_access_warning(&self.config.codex_home, true) {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenSettings => {
                self.chat_widget.open_settings_overlay();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    self.disable_live_exec_polling();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    self.disable_live_exec_polling();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
            },
        }
        Ok(true)
    }

    fn enable_live_exec_polling(&mut self, tui: &mut tui::Tui) {
        if !self.live_exec_poll.active {
            self.live_exec_poll.active = true;
            if self.live_exec_poll.task.is_none() {
                let tx = self.app_event_tx.clone();
                self.live_exec_poll.task = Some(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(LIVE_EXEC_POLL_INTERVAL).await;
                        if tx.app_event_tx.is_closed() {
                            break;
                        }
                        tx.send(AppEvent::LiveExecPollTick);
                    }
                }));
            }
        }

        self.app_event_tx.send(AppEvent::LiveExecPollTick);
        if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
            tui.frame_requester().schedule_frame();
        }
    }

    pub(crate) fn disable_live_exec_polling(&mut self) {
        if !self.chat_widget.active_background_shell_ids().is_empty() {
            return;
        }
        self.live_exec_poll.active = false;
        if let Some(task) = self.live_exec_poll.task.take() {
            task.abort();
        }
    }

    pub(crate) fn maybe_poll_live_exec(&mut self, tui: &mut tui::Tui) {
        if !self.live_exec_poll.active {
            return;
        }

        let shell_ids = self.chat_widget.active_background_shell_ids();
        if shell_ids.is_empty() {
            self.disable_live_exec_polling();
            return;
        }

        for shell_id in shell_ids {
            self.chat_widget
                .submit_op(Op::PollBackgroundShell { shell_id });
        }

        if matches!(self.overlay, Some(Overlay::LiveExec(_))) {
            tui.frame_requester().schedule_frame();
        }
    }

    pub(crate) fn token_usage(&self) -> codex_core::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    async fn open_process_manager_overlay(&mut self) -> Result<()> {
        let entries = self.fetch_process_manager_entries().await?;
        self.chat_widget.show_process_manager(entries);
        Ok(())
    }

    async fn fetch_process_manager_entries(&self) -> Result<Vec<ProcessManagerEntry>> {
        let conversation = self.active_conversation().await?;
        let snapshots = conversation.unified_exec_sessions().await;
        Ok(snapshots
            .into_iter()
            .map(ProcessManagerEntry::from_snapshot)
            .collect())
    }

    async fn show_unified_exec_output(
        &mut self,
        session_id: i32,
        window: Option<UnifiedExecOutputWindow>,
    ) -> Result<()> {
        match self.fetch_unified_exec_output(session_id, window).await? {
            Some((entry, data)) => {
                self.chat_widget.show_unified_exec_output(entry, data);
            }
            None => {
                self.chat_widget.add_info_message(
                    format!("No output available for session {session_id}."),
                    None,
                );
            }
        }
        Ok(())
    }

    async fn fetch_unified_exec_output(
        &self,
        session_id: i32,
        window: Option<UnifiedExecOutputWindow>,
    ) -> Result<Option<(ProcessManagerEntry, ProcessOutputData)>> {
        let conversation = self.active_conversation().await?;
        let output = if let Some(window) = window {
            conversation
                .unified_exec_output_window(session_id, window)
                .await
        } else {
            conversation.unified_exec_output(session_id).await
        };
        Ok(output.map(entry_and_data_from_output))
    }

    async fn send_unified_exec_input(&mut self, session_id: i32, input: String) -> Result<()> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            self.chat_widget
                .add_info_message("Input cancelled: nothing to send.".to_string(), None);
            return Ok(());
        }

        let conversation = self.active_conversation().await?;
        conversation
            .run_unified_exec(Some(session_id), std::slice::from_ref(&input), None)
            .await
            .map_err(|err| eyre!(err))?;

        self.chat_widget
            .add_info_message(format!("Sent input to session {session_id}."), None);
        self.show_unified_exec_output(session_id, None).await?;
        Ok(())
    }

    async fn export_unified_exec_log(
        &mut self,
        session_id: i32,
        destination: String,
    ) -> Result<()> {
        let path = self.resolve_export_path(&destination, session_id);
        let conversation = self.active_conversation().await?;
        conversation
            .export_unified_exec_log(session_id, path.clone())
            .await
            .map_err(|err| eyre!(err))?;
        self.chat_widget.add_info_message(
            format!(
                "Exported session {session_id} output to {}.",
                path.display()
            ),
            None,
        );
        Ok(())
    }

    async fn kill_unified_exec_session(&mut self, session_id: i32) -> Result<()> {
        let conversation = self.active_conversation().await?;
        let killed = conversation.kill_unified_exec_session(session_id).await;
        if killed {
            self.chat_widget.add_info_message(
                format!("Requested session {session_id} to terminate."),
                None,
            );
            Ok(())
        } else {
            Err(eyre!("Session {session_id} is not running"))
        }
    }

    async fn remove_unified_exec_session(&mut self, session_id: i32) -> Result<()> {
        let conversation = self.active_conversation().await?;
        let removed = conversation.remove_unified_exec_session(session_id).await;
        if removed {
            self.chat_widget.add_info_message(
                format!("Removed session {session_id} from the manager."),
                None,
            );
            Ok(())
        } else {
            Err(eyre!("Session {session_id} not found"))
        }
    }

    fn default_export_suggestion(&self, session_id: i32) -> Option<String> {
        let path = self.resolve_export_path("", session_id);
        Some(path.display().to_string())
    }

    fn resolve_export_path(&self, destination: &str, session_id: i32) -> PathBuf {
        let trimmed = destination.trim();
        if trimmed.is_empty() {
            return self
                .config
                .cwd
                .join(format!("unified-exec-session-{session_id}.log"));
        }

        let path = PathBuf::from(trimmed);
        if path.is_absolute() {
            path
        } else {
            self.config.cwd.join(path)
        }
    }

    async fn handle_exec_stream_toggle(
        &mut self,
        tui: &mut tui::Tui,
        call_id: Option<String>,
        stream: ExecStreamKind,
        action: ExecStreamAction,
    ) -> Result<()> {
        let call_id = match call_id.or_else(|| self.latest_exec_call_id()) {
            Some(id) => id,
            None => {
                self.chat_widget
                    .add_error_message("No completed shell commands to toggle.".to_string());
                return Ok(());
            }
        };

        let Some((idx, exec_cell)) =
            self.transcript_cells
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, cell)| {
                    cell.as_any()
                        .downcast_ref::<ExecCell>()
                        .and_then(|exec| exec.contains_call(&call_id).then_some((index, exec)))
                })
        else {
            self.chat_widget
                .add_error_message(format!("Could not find shell output for `{call_id}`."));
            return Ok(());
        };

        let Some(output) = exec_cell.call_output(&call_id) else {
            self.chat_widget
                .add_error_message(format!("Shell output for `{call_id}` is still running."));
            return Ok(());
        };

        let stream_text = match stream {
            ExecStreamKind::Stdout => &output.stdout,
            ExecStreamKind::Stderr => &output.stderr,
        };
        if stream_text.trim().is_empty() {
            self.chat_widget.add_info_message(
                format!(
                    "{} has no buffered output for call `{call_id}`.",
                    stream.as_str()
                ),
                None,
            );
            return Ok(());
        }

        if matches!(action, ExecStreamAction::Expand) && !output.stream_collapsed(stream) {
            self.chat_widget.add_info_message(
                format!("{} is already expanded for `{call_id}`.", stream.as_str()),
                None,
            );
            return Ok(());
        }
        if matches!(action, ExecStreamAction::Collapse) && output.stream_collapsed(stream) {
            self.chat_widget.add_info_message(
                format!("{} is already collapsed for `{call_id}`.", stream.as_str()),
                None,
            );
            return Ok(());
        }

        let Some(updated_exec) = exec_cell.apply_stream_action(&call_id, &[stream], action) else {
            self.chat_widget
                .add_error_message("Unable to toggle stream visibility.".to_string());
            return Ok(());
        };

        let cell: Arc<dyn HistoryCell> = Arc::new(updated_exec);
        self.transcript_cells[idx] = cell.clone();
        if let Some(Overlay::Transcript(t)) = &mut self.overlay {
            t.replace_cell(idx, cell.clone());
        }
        self.repaint_transcript(tui)?;

        let state_collapsed = cell
            .as_any()
            .downcast_ref::<ExecCell>()
            .and_then(|exec| exec.call_output(&call_id))
            .map(|out| out.stream_collapsed(stream))
            .unwrap_or(false);
        let state_label = if state_collapsed {
            "collapsed"
        } else {
            "expanded"
        };
        self.chat_widget.add_info_message(
            format!("{} {state_label} for call `{call_id}`.", stream.as_str()),
            None,
        );
        Ok(())
    }

    fn latest_exec_call_id(&self) -> Option<String> {
        self.transcript_cells.iter().rev().find_map(|cell| {
            cell.as_any()
                .downcast_ref::<ExecCell>()
                .and_then(|exec| exec.last_completed_call_id().map(str::to_string))
        })
    }

    fn repaint_transcript(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let width = tui.terminal.last_known_screen_size.width;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut has_visible = false;
        for cell in &self.transcript_cells {
            let mut rendered = cell.display_lines(width);
            if rendered.is_empty() {
                continue;
            }
            if !cell.is_stream_continuation() {
                if has_visible {
                    lines.push(Line::from(""));
                } else {
                    has_visible = true;
                }
            }
            lines.append(&mut rendered);
        }
        tui.replace_history_lines(lines)?;
        self.has_emitted_history_lines = has_visible;
        Ok(())
    }

    async fn active_conversation(&self) -> Result<Arc<CodexConversation>> {
        let conversation_id = self
            .chat_widget
            .conversation_id()
            .ok_or_else(|| eyre!("Session is still starting"))?;
        let conversation = self
            .server
            .get_conversation(conversation_id)
            .await
            .wrap_err("failed to load active conversation")?;
        Ok(conversation)
    }

    fn template_catalog(&self) -> TemplateCatalog {
        TemplateCatalog::from_templates(self.config.mcp_templates.clone())
    }

    fn build_mcp_registry(&self) -> McpRegistry<'_> {
        McpRegistry::new(&self.config, self.template_catalog())
    }

    async fn open_mcp_manager_overlay(&mut self) -> Result<()> {
        let registry = self.build_mcp_registry();
        let state = McpManagerState::from_registry(&registry);
        let entries = state
            .servers
            .into_iter()
            .map(|snapshot| McpManagerEntry {
                health: registry.health_report(&snapshot.name),
                snapshot,
            })
            .collect();
        self.chat_widget
            .show_mcp_manager(entries, state.template_count);
        Ok(())
    }

    async fn open_mcp_wizard(
        &mut self,
        mut template_id: Option<String>,
        draft: Option<McpWizardDraft>,
        existing_name: Option<String>,
    ) -> Result<()> {
        let catalog = self.template_catalog();
        let had_draft = draft.is_some();
        let mut resolved_draft = draft.unwrap_or_default();
        if resolved_draft.template_id.is_none() {
            resolved_draft.template_id = template_id.take();
        }
        if !had_draft
            && let Some(template_cfg) = resolved_draft
                .template_id
                .clone()
                .and_then(|id| catalog.instantiate(&id))
        {
            resolved_draft.apply_template_config(&template_cfg);
        }

        let init = McpWizardInit {
            app_event_tx: self.app_event_tx.clone(),
            catalog,
            draft: Some(resolved_draft),
            existing_name,
        };
        self.chat_widget.open_mcp_wizard(init);
        Ok(())
    }

    async fn apply_mcp_wizard(
        &mut self,
        draft: McpWizardDraft,
        existing_name: Option<String>,
    ) -> Result<()> {
        let catalog = self.template_catalog();
        let server = draft
            .build_server_config(&catalog)
            .map_err(|err| eyre!(err))?;
        let name = draft.name.clone();
        self.upsert_mcp_server(existing_name.as_deref(), &name, server)
            .await?;
        self.chat_widget
            .add_info_message(format!("Saved MCP server `{name}`."), None);
        self.open_mcp_manager_overlay().await?;
        Ok(())
    }

    async fn reload_mcp_servers(&mut self) -> Result<()> {
        let servers = load_global_mcp_servers(&self.config.codex_home)
            .await?
            .into_iter()
            .collect::<HashMap<_, _>>();
        self.config.mcp_servers = servers;
        self.chat_widget
            .add_info_message("Reloaded MCP servers from disk.".to_string(), None);
        self.open_mcp_manager_overlay().await?;
        Ok(())
    }

    async fn remove_mcp_server(&mut self, name: &str) -> Result<()> {
        let registry = self.build_mcp_registry();
        let removed = registry.remove_server(name).map_err(|err| eyre!(err))?;
        if removed {
            self.config.mcp_servers.remove(name);
            self.chat_widget
                .add_info_message(format!("Removed MCP server `{name}`."), None);
            self.open_mcp_manager_overlay().await?;
            Ok(())
        } else {
            Err(eyre!(format!("MCP server `{name}` not found")))
        }
    }

    async fn upsert_mcp_server(
        &mut self,
        existing_name: Option<&str>,
        name: &str,
        server: McpServerConfig,
    ) -> Result<()> {
        let registry = self.build_mcp_registry();
        registry
            .upsert_server_with_existing(existing_name, name, server.clone())
            .map_err(|err| eyre!(err))?;
        match existing_name {
            Some(old) if old != name => {
                self.config.mcp_servers.remove(old);
            }
            _ => {}
        }
        self.config.mcp_servers.insert(name.to_string(), server);
        Ok(())
    }

    async fn maybe_prompt_agents_context_warning(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let warning_limit = self.config.agents_context_warning_tokens;
        if warning_limit == 0 {
            return Ok(());
        }
        let tokens = self.config.agents_context_prompt_tokens;
        if tokens <= warning_limit {
            return Ok(());
        }
        if self.config.agents_context_entries.is_empty() {
            return Ok(());
        }

        let (global_count, project_count) = self.agents_context_counts();
        let percent = self
            .chat_widget
            .context_window_capacity()
            .and_then(|window| {
                if window == 0 {
                    None
                } else {
                    Some(tokens as f64 / window as f64 * 100.0)
                }
            });
        let params = AgentsContextWarningParams {
            tokens,
            percent_of_window: percent,
            truncated: self.config.agents_context_prompt_truncated,
            global_context_path: self.config.agents_home.display().to_string(),
            project_context_path: self
                .config
                .project_agents_home
                .as_ref()
                .map(|path| path.display().to_string()),
            global_entry_count: global_count,
            project_entry_count: project_count,
        };

        match run_agents_context_warning(tui, params).await? {
            AgentsContextDecision::Continue => {}
            AgentsContextDecision::DisableForSession => {
                self.chat_widget.set_agents_context_active(false);
                self.chat_widget.add_info_message(
                    "Agents context disabled for this session.".to_string(),
                    Some("Use `/context` to re-enable.".to_string()),
                );
            }
            AgentsContextDecision::ShowPathsAndExit => {
                self.warn_agents_show_paths();
            }
            AgentsContextDecision::RequestCompression => {
                self.warn_agents_request_compression();
            }
            AgentsContextDecision::ManageEntries => {
                self.chat_widget
                    .add_info_message("Re-opening agents context manager.".to_string(), None);
                self.app_event_tx.send(AppEvent::OpenAgentsContextManager);
            }
        }

        Ok(())
    }

    fn agents_context_counts(&self) -> (usize, usize) {
        let mut global = 0;
        let mut project = 0;
        for entry in &self.config.agents_context_entries {
            match entry.source {
                AgentsSource::Global => global += 1,
                AgentsSource::Project => project += 1,
            }
        }
        (global, project)
    }

    fn warn_agents_show_paths(&mut self) {
        let mut lines = vec![format!(
            "Global context directory: {}",
            self.config.agents_home.display()
        )];
        match &self.config.project_agents_home {
            Some(path) => lines.push(format!("Project context directory: {}", path.display())),
            None => lines.push("Project context directory not detected.".to_string()),
        }
        self.chat_widget.add_info_message(
            lines.join("\n"),
            Some("Inspect these folders to trim large files.".to_string()),
        );
    }

    fn warn_agents_request_compression(&mut self) {
        let suggestion = format!(
            "Please compress the agents context so it stays under {} tokens.",
            format_token_count(self.config.agents_context_warning_tokens as u64)
        );
        if self.chat_widget.composer_is_empty() {
            self.chat_widget.set_composer_text(suggestion);
        } else {
            self.chat_widget.add_info_message(
                suggestion,
                Some("Edit the composer before sending.".to_string()),
            );
        }
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.chat_widget.set_reasoning_effort(effort);
        self.config.model_reasoning_effort = effort;
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('r'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if matches!(self.overlay, Some(ref overlay) if overlay.is_live_exec()) {
                    self.close_transcript_overlay(tui);
                } else {
                    let _ = tui.enter_alt_screen();
                    self.overlay = Some(Overlay::new_live_exec(self.live_exec.clone()));
                    if let Some(overlay) = &mut self.overlay {
                        overlay.on_live_exec_opened();
                    }
                    self.enable_live_exec_polling(tui);
                    tui.frame_requester().schedule_frame();
                }
            }
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                && modifiers.contains(crossterm::event::KeyModifiers::SHIFT) =>
            {
                if let Err(err) = self.open_process_manager_overlay().await {
                    tracing::error!(error = %err, "failed to open process manager overlay");
                    self.chat_widget
                        .add_error_message(format!("Failed to open process manager: {err}"));
                }
            }
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                if let Some((call_id, description)) = self.chat_widget.prepare_promotion_request() {
                    self.app_event_tx.send(AppEvent::CodexOp(Op::PromoteShell {
                        call_id,
                        description,
                    }));
                    self.enable_live_exec_polling(tui);
                }
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Enter alternate screen and set viewport to full size.
                let _ = tui.enter_alt_screen();
                self.disable_live_exec_polling();
                self.overlay = Some(Overlay::new_transcript(self.transcript_cells.clone()));
                tui.frame_requester().schedule_frame();
            }
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                // Delegate to helper for clarity; preserves behavior.
                self.confirm_backtrack_from_main();
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                // This avoids stale "Esc-primed" state after the user starts typing
                // (even if they later backspace to empty).
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                // Ignore Release key events.
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::user_count;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use codex_core::AuthManager;
    use codex_core::CodexAuth;
    use codex_core::ConversationManager;
    use codex_core::protocol::SessionConfiguredEvent;
    use codex_protocol::ConversationId;
    use ratatui::prelude::Line;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender();
        let config = chat_widget.config_ref().clone();

        let server = Arc::new(ConversationManager::with_auth(CodexAuth::from_api_key(
            "Test API Key",
        )));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());

        let live_exec_root = config.cwd.clone();

        App {
            server,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            active_profile: None,
            file_search,
            live_exec: Rc::new(RefCell::new(LiveExecState::new(live_exec_root))),
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            feedback: codex_feedback::CodexFeedback::new(),
            pending_update_action: None,
            live_exec_poll: LiveExecPollState::default(),
        }
    }

    #[test]
    fn update_reasoning_effort_updates_config() {
        let mut app = make_test_app();
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[test]
    fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let mut app = make_test_app();

        let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ConversationId::new(),
                model: "gpt-test".to_string(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: PathBuf::new(),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        // Simulate the transcript after trimming for a fork, replaying history, and
        // appending the edited turn. The session header separates the retained history
        // from the forked conversation's replayed turns.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question"),
            agent_cell("answer first"),
            user_cell("follow-up"),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question"),
            agent_cell("answer first"),
            user_cell("follow-up (edited)"),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        app.backtrack.base_id = Some(ConversationId::new());
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        app.confirm_backtrack_from_main();

        let (_, nth, prefill) = app.backtrack.pending.clone().expect("pending backtrack");
        assert_eq!(nth, 1);
        assert_eq!(prefill, "follow-up (edited)");
    }
}
