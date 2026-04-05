mod claude_cli;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::Op;
use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::CodexThread;
use crate::agent::control::render_input_preview;
use crate::agent::role::spawn_tool_spec;
use crate::claude_code_control::ClaudeControlRequest;
use crate::claude_code_control::ControlRequestParseOutcome;
use crate::claude_code_control::parse_control_request_line;
use crate::claude_code_control::resolve_external_claude_code_permission_request;
use crate::claude_code_stream::completed_response_items;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::compact_transcript::render_compact_transcript;
use crate::config::ClaudeCliConfig;
use crate::config::ClaudeCliEffort;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::model_provider_info::model_picker_provider_ids;
use crate::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use crate::models_manager::manager::ModelsManager;
use crate::models_manager::manager::RefreshStrategy;
use crate::protocol::EventMsg;
use crate::protocol::WarningEvent;

pub(crate) use self::claude_cli::ClaudeCliRequest;
pub(crate) use self::claude_cli::ClaudeCliSession;
pub(crate) use self::claude_cli::ClaudeCodeTurnResult;
pub(crate) use self::claude_cli::run_claude_cli;
pub(crate) use self::claude_cli::run_claude_cli_stream_json_controlled;

const MAX_EXTERNAL_AGENT_CONVERSATION_ENTRIES: usize = 12;
const CLAUDE_RUNTIME_TRUTH_PROMPT: &str = concat!(
    "The user prompt may include a `<codex_runtime_truth>` block with current Codex runtime context.\n",
    "Treat that block as authoritative for this delegated turn.\n",
    "When it includes provider, approval, subagent-role, or tool-inventory updates, ",
    "prefer the latest such update over older conversation text or guesses."
);

#[derive(Clone)]
pub(crate) struct ExternalAgentLaunchRequest {
    pub(crate) host_thread: Arc<CodexThread>,
    pub(crate) config_snapshot: ThreadConfigSnapshot,
    pub(crate) developer_instructions: Option<String>,
    pub(crate) claude_cli: ClaudeCliConfig,
    pub(crate) model: String,
    pub(crate) parent_context: Option<String>,
}

#[derive(Default)]
pub(crate) struct ExternalAgentRegistry {
    agents: Mutex<HashMap<ThreadId, Arc<ExternalAgentState>>>,
}

impl ExternalAgentRegistry {
    pub(crate) async fn spawn_agent(
        &self,
        thread_id: ThreadId,
        request: ExternalAgentLaunchRequest,
        initial_operation: Op,
    ) -> CodexResult<String> {
        let agent = Arc::new(ExternalAgentState::new(request));
        let submission_id = agent.submit(initial_operation).await?;
        self.agents.lock().await.insert(thread_id, agent);
        Ok(submission_id)
    }

    pub(crate) async fn send_input(
        &self,
        thread_id: ThreadId,
        operation: Op,
    ) -> CodexResult<String> {
        self.get(thread_id).await?.submit(operation).await
    }

    pub(crate) async fn interrupt_agent(&self, thread_id: ThreadId) -> CodexResult<String> {
        self.get(thread_id).await?.interrupt().await
    }

    pub(crate) async fn close_agent(&self, thread_id: ThreadId) -> CodexResult<String> {
        let agent = self
            .agents
            .lock()
            .await
            .remove(&thread_id)
            .ok_or(CodexErr::ThreadNotFound(thread_id))?;
        agent.close().await
    }

    pub(crate) async fn get_status(&self, thread_id: ThreadId) -> Option<AgentStatus> {
        let agent = self.agents.lock().await.get(&thread_id).cloned()?;
        Some(agent.status())
    }

    pub(crate) async fn get_config_snapshot(
        &self,
        thread_id: ThreadId,
    ) -> Option<ThreadConfigSnapshot> {
        let agent = self.agents.lock().await.get(&thread_id).cloned()?;
        Some(agent.config_snapshot())
    }

    pub(crate) async fn subscribe_status(
        &self,
        thread_id: ThreadId,
    ) -> Option<watch::Receiver<AgentStatus>> {
        let agent = self.agents.lock().await.get(&thread_id).cloned()?;
        Some(agent.subscribe_status())
    }

    async fn get(&self, thread_id: ThreadId) -> CodexResult<Arc<ExternalAgentState>> {
        self.agents
            .lock()
            .await
            .get(&thread_id)
            .cloned()
            .ok_or(CodexErr::ThreadNotFound(thread_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationEntry {
    role: ConversationRole,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationRole {
    User,
    Assistant,
}

impl ConversationRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone)]
struct ExternalActiveTurn {
    generation: u64,
    cancellation_token: CancellationToken,
}

struct ExternalAgentState {
    host_thread: Arc<CodexThread>,
    config_snapshot: ThreadConfigSnapshot,
    developer_instructions: Option<String>,
    claude_cli: ClaudeCliConfig,
    model: String,
    parent_context: Option<String>,
    claude_session_id: Mutex<Option<String>>,
    conversation: Mutex<Vec<ConversationEntry>>,
    active_turn: Mutex<Option<ExternalActiveTurn>>,
    next_generation: AtomicU64,
    status_tx: watch::Sender<AgentStatus>,
}

impl ExternalAgentState {
    fn new(request: ExternalAgentLaunchRequest) -> Self {
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        Self {
            host_thread: request.host_thread,
            config_snapshot: request.config_snapshot,
            developer_instructions: request.developer_instructions,
            claude_cli: request.claude_cli,
            model: request.model,
            parent_context: request.parent_context,
            claude_session_id: Mutex::new(None),
            conversation: Mutex::new(Vec::new()),
            active_turn: Mutex::new(None),
            next_generation: AtomicU64::new(1),
            status_tx,
        }
    }

    fn config_snapshot(&self) -> ThreadConfigSnapshot {
        self.config_snapshot.clone()
    }

    fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        self.status_tx.subscribe()
    }

    fn status(&self) -> AgentStatus {
        self.status_tx.borrow().clone()
    }

    async fn submit(self: &Arc<Self>, operation: Op) -> CodexResult<String> {
        let user_message = operation_to_external_message(&operation)?;
        let carrier_session_id = self.claude_session_id.lock().await.clone();
        let conversation_snapshot = if carrier_session_id.is_none() {
            Some(self.conversation.lock().await.clone())
        } else {
            None
        };
        let generation = self.next_generation.fetch_add(1, Ordering::AcqRel);
        let cancellation_token = CancellationToken::new();
        {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return Err(CodexErr::UnsupportedOperation(
                    "external Claude agent is already running; wait or interrupt first".to_string(),
                ));
            }
            *active_turn = Some(ExternalActiveTurn {
                generation,
                cancellation_token: cancellation_token.clone(),
            });
        }
        let runtime_truth = self
            .build_runtime_truth(
                /*include_developer_instructions*/ carrier_session_id.is_none(),
                /*include_parent_context*/ carrier_session_id.is_none(),
                /*include_static_inventory*/ carrier_session_id.is_none(),
            )
            .await;
        self.conversation.lock().await.push(ConversationEntry {
            role: ConversationRole::User,
            text: user_message.clone(),
        });
        self.status_tx.send_replace(AgentStatus::Running);
        let host_turn_context = self.begin_host_turn().await;
        let host_turn_id = host_turn_context.sub_id.clone();
        let host_truncation_policy = host_turn_context.truncation_policy;
        self.record_host_message(
            host_turn_id.as_str(),
            host_truncation_policy,
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![codex_protocol::models::ContentItem::InputText {
                    text: user_message.clone(),
                }],
                end_turn: None,
                phase: None,
            },
        )
        .await;

        let submission_id = Uuid::new_v4().to_string();
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let request = ClaudeCliRequest {
                cwd: state.config_snapshot.cwd.clone(),
                model: state.model.clone(),
                system_prompt: build_external_agent_system_prompt_with_bridge(
                    state.claude_cli.codex_self_exe.is_some(),
                    /*include_runtime_truth_guidance*/ !runtime_truth.trim().is_empty(),
                ),
                user_prompt: if carrier_session_id.is_some() {
                    build_external_agent_continuation_prompt(
                        runtime_truth.as_str(),
                        user_message.as_str(),
                    )
                } else {
                    build_external_agent_user_prompt(
                        runtime_truth.as_str(),
                        conversation_snapshot.as_deref().unwrap_or(&[]),
                        user_message.as_str(),
                    )
                },
                session: carrier_session_id
                    .map(ClaudeCliSession::ResumeExisting)
                    .unwrap_or(ClaudeCliSession::Persistent),
                json_schema: None,
                tools: state.claude_cli.tools.clone(),
                force_toolless: false,
                effort: state.claude_cli.effort.or(state
                    .config_snapshot
                    .reasoning_effort
                    .map(ClaudeCliEffort::from)),
            };
            let result = state
                .run_external_claude_turn(
                    request,
                    host_turn_id.clone(),
                    host_truncation_policy,
                    cancellation_token,
                )
                .await;
            state
                .finish_turn(generation, host_turn_id, host_truncation_policy, result)
                .await;
        });

        Ok(submission_id)
    }

    async fn interrupt(&self) -> CodexResult<String> {
        let active_turn = self.active_turn.lock().await.take();
        if let Some(active_turn) = active_turn {
            active_turn.cancellation_token.cancel();
            self.finish_host_turn().await;
            self.status_tx.send_replace(AgentStatus::Interrupted);
        }
        Ok(Uuid::new_v4().to_string())
    }

    async fn close(&self) -> CodexResult<String> {
        let _ = self.interrupt().await?;
        self.status_tx.send_replace(AgentStatus::Shutdown);
        Ok(String::new())
    }

    async fn finish_turn(
        &self,
        generation: u64,
        host_turn_id: String,
        host_truncation_policy: codex_protocol::protocol::TruncationPolicy,
        result: anyhow::Result<ClaudeCodeTurnResult>,
    ) {
        let mut active_turn = self.active_turn.lock().await;
        if active_turn
            .as_ref()
            .is_none_or(|active_turn| active_turn.generation != generation)
        {
            return;
        }
        *active_turn = None;
        drop(active_turn);

        match result {
            Ok(output) => {
                if let Some(session_id) = output.session_id {
                    *self.claude_session_id.lock().await = Some(session_id);
                }
                if output.recorded_response_items_live {
                    // The child-thread host already received raw response items during the turn.
                } else if output.response_items.is_empty() {
                    let Some(text) = output.assistant_text.as_ref() else {
                        self.status_tx.send_replace(AgentStatus::Errored(
                            "Claude Code turn completed without assistant text or response items"
                                .to_string(),
                        ));
                        self.finish_host_turn().await;
                        return;
                    };
                    self.record_host_message(
                        host_turn_id.as_str(),
                        host_truncation_policy,
                        ResponseItem::Message {
                            id: None,
                            role: "assistant".to_string(),
                            content: vec![codex_protocol::models::ContentItem::OutputText {
                                text: text.clone(),
                            }],
                            end_turn: Some(true),
                            phase: None,
                        },
                    )
                    .await;
                } else {
                    self.host_thread
                        .codex
                        .session
                        .record_external_host_items(
                            host_turn_id.as_str(),
                            host_truncation_policy,
                            &output.response_items,
                        )
                        .await;
                }
                if let Some(text) = output.assistant_text.as_ref() {
                    self.conversation.lock().await.push(ConversationEntry {
                        role: ConversationRole::Assistant,
                        text: text.clone(),
                    });
                }
                self.status_tx
                    .send_replace(AgentStatus::Completed(output.assistant_text));
            }
            Err(err) => {
                let message = err.to_string();
                if should_clear_claude_session(&message) {
                    *self.claude_session_id.lock().await = None;
                }
                if !message.contains("Claude CLI run cancelled") {
                    self.host_thread
                        .codex
                        .session
                        .send_event_raw(Event {
                            id: host_turn_id,
                            msg: EventMsg::Warning(WarningEvent {
                                message: message.clone(),
                            }),
                        })
                        .await;
                }
                let status = if message.contains("Claude CLI run cancelled") {
                    AgentStatus::Interrupted
                } else {
                    AgentStatus::Errored(message)
                };
                self.status_tx.send_replace(status);
            }
        }
        self.finish_host_turn().await;
    }

    async fn begin_host_turn(&self) -> Arc<crate::codex::TurnContext> {
        let turn_context = self.host_thread.codex.session.new_default_turn().await;
        let mut active_turn = self.host_thread.codex.session.active_turn.lock().await;
        if let Some(active_turn_state) = active_turn.as_mut() {
            let mut turn_state = active_turn_state.turn_state.lock().await;
            turn_state.clear_pending();
        }
        *active_turn = Some(crate::state::ActiveTurn::default());
        turn_context
    }

    async fn build_runtime_truth(
        &self,
        include_developer_instructions: bool,
        include_parent_context: bool,
        include_static_inventory: bool,
    ) -> String {
        let runtime_snapshot = self
            .host_thread
            .codex
            .external_agent_prompt_snapshot()
            .await;
        let mut sections = Vec::new();
        let visible_provider_ids = model_picker_provider_ids(
            &runtime_snapshot.model_providers,
            &runtime_snapshot.model_provider_id,
        );
        sections.push(format!(
            "<external_agent_runtime>\nmode: delegated_subagent\nactive_model_provider: {}\nactive_model: {}\nvisible_model_providers: {}\napproval_policy: {:?}\nsandbox_policy: {:?}\nworking_directory: {}\n</external_agent_runtime>",
            runtime_snapshot.model_provider_id,
            runtime_snapshot.model,
            visible_provider_ids.join(", "),
            runtime_snapshot.approval_policy,
            runtime_snapshot.sandbox_policy,
            runtime_snapshot.cwd.display(),
        ));
        if include_developer_instructions
            && let Some(developer_instructions) = self.developer_instructions.as_deref()
            && !developer_instructions.trim().is_empty()
        {
            sections.push(format!(
                "<codex_runtime_update role=\"developer\">\n{}\n</codex_runtime_update>",
                developer_instructions.trim()
            ));
        }
        if include_parent_context
            && let Some(parent_context) = self.parent_context.as_deref()
            && !parent_context.trim().is_empty()
        {
            sections.push(format!(
                "<codex_runtime_update role=\"forked_parent_context\">\n{}\n</codex_runtime_update>",
                parent_context.trim()
            ));
        }
        if include_static_inventory {
            let spawn_roles = spawn_tool_spec::build(&runtime_snapshot.agent_roles);
            sections.push(format!(
                "<codex_subagent_roles>\nThis is the current Codex role and model inventory exposed through spawn_agent.\n{spawn_roles}\n</codex_subagent_roles>"
            ));
            if let Some(available_models) = render_external_available_models(
                &runtime_snapshot,
                &self.host_thread.codex.session.services.auth_manager,
                &visible_provider_ids,
                self.host_thread
                    .codex
                    .enabled(codex_features::Feature::DefaultModeRequestUserInput),
            )
            .await
            {
                sections.push(available_models);
            }
        }
        sections.push(render_external_tool_summary(
            self.claude_cli.tools.as_deref(),
            self.claude_cli.codex_self_exe.is_some(),
        ));
        sections.join("\n\n")
    }

    async fn finish_host_turn(&self) {
        let mut active_turn = self.host_thread.codex.session.active_turn.lock().await;
        if let Some(active_turn_state) = active_turn.as_mut() {
            let mut turn_state = active_turn_state.turn_state.lock().await;
            turn_state.clear_pending();
        }
        *active_turn = None;
    }

    async fn record_host_message(
        &self,
        event_id: &str,
        truncation_policy: codex_protocol::protocol::TruncationPolicy,
        message: ResponseItem,
    ) {
        self.host_thread
            .codex
            .session
            .record_external_host_items(event_id, truncation_policy, &[message])
            .await;
    }

    async fn run_external_claude_turn(
        &self,
        request: ClaudeCliRequest,
        host_turn_id: String,
        host_truncation_policy: codex_protocol::protocol::TruncationPolicy,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ClaudeCodeTurnResult> {
        let controlled = run_claude_cli_stream_json_controlled(
            &self.claude_cli,
            request,
            cancellation_token.clone(),
        )
        .await?;
        let mut raw_lines = controlled.lines;
        let control_responder = controlled.control_responder;
        let mut accumulator = crate::claude_code_stream::ClaudeCodeStreamAccumulator::default();
        let mut recorded_response_items_live = false;

        while let Some(line) = raw_lines.recv().await {
            match line {
                Ok(line) => match parse_control_request_line(&line, &control_responder) {
                    Ok(ControlRequestParseOutcome::ControlRequest(
                        ClaudeControlRequest::CanUseTool(permission_request),
                    )) => {
                        let resolution = resolve_external_claude_code_permission_request(
                            &self.host_thread.codex.session,
                            self.config_snapshot.approval_policy,
                            &host_turn_id,
                            &self.config_snapshot.cwd,
                            &permission_request,
                        )
                        .await;
                        let responder = permission_request.responder();
                        let request_id = permission_request.request_id.clone();
                        match resolution.response {
                            codex_api::common::ClaudeCodeControlResponseSubtype::Allow {
                                updated_input,
                            } => responder
                                .allow_for_request(request_id, updated_input)
                                .await
                                .map_err(anyhow::Error::msg)?,
                            codex_api::common::ClaudeCodeControlResponseSubtype::Deny {
                                message,
                            } => responder
                                .deny(request_id, message)
                                .await
                                .map_err(anyhow::Error::msg)?,
                        }
                        if resolution.interrupt_turn {
                            cancellation_token.cancel();
                        }
                    }
                    Ok(ControlRequestParseOutcome::ControlRequest(
                        ClaudeControlRequest::UnsupportedSubtype { subtype },
                    )) => {
                        anyhow::bail!(
                            "Claude Code carrier emitted an unsupported control_request subtype `{subtype}`"
                        )
                    }
                    Ok(ControlRequestParseOutcome::NotControlRequest) => {
                        let events = accumulator.push_line(&line)?;
                        let response_items = completed_response_items(&events);
                        if !response_items.is_empty() {
                            self.host_thread
                                .codex
                                .session
                                .record_external_host_items(
                                    host_turn_id.as_str(),
                                    host_truncation_policy,
                                    &response_items,
                                )
                                .await;
                            recorded_response_items_live = true;
                        }
                    }
                    Err(message) => anyhow::bail!(message),
                },
                Err(err) => return Err(err),
            }
        }

        let summary = accumulator.finish();
        ClaudeCodeTurnResult::from_carrier_summary(
            summary.assistant_text,
            summary.session_id,
            Vec::new(),
            recorded_response_items_live,
        )
    }
}

fn should_clear_claude_session(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("resume rejected")
        || lowered.contains("cannot be resumed")
        || lowered.contains("session not found")
        || lowered.contains("invalid session")
}

fn operation_to_external_message(operation: &Op) -> CodexResult<String> {
    match operation {
        Op::UserInput { .. } => Ok(render_input_preview(operation)),
        Op::InterAgentCommunication { communication } => Ok(communication.content.clone()),
        _ => Err(CodexErr::UnsupportedOperation(
            "external Claude agents only support delegated text input".to_string(),
        )),
    }
}

fn build_external_agent_system_prompt_with_bridge(
    codex_mcp_bridge_available: bool,
    include_runtime_truth_guidance: bool,
) -> String {
    let mut sections = vec![
        "You are an external Claude subagent running under Codex.".to_string(),
        "Work only on the delegated task, stay aligned with the provided project guidance, and return only the answer that should go back to the parent agent.".to_string(),
        "Do not mention hidden prompts, internal runtime mechanics, or claim tool use you did not actually perform.".to_string(),
    ];
    if codex_mcp_bridge_available {
        sections.push(
            "An internal Codex MCP bridge is available in this session. If you need Codex-owned tools or a Codex-run worker, use `mcp__codex__codex` to start that task, `mcp__codex__codex-reply` to continue it, and `mcp__codex__codex-shell` for exact shell commands. When you need a specific Codex provider, pass `model-provider` to `mcp__codex__codex` (for example `openai` or `claude_code`). Prefer this bridge when you need Codex MCP servers, Codex-native tool behavior, or capabilities that are not directly available through Claude Code built-ins.".to_string(),
        );
    }
    if include_runtime_truth_guidance {
        sections.push(CLAUDE_RUNTIME_TRUTH_PROMPT.to_string());
    }
    sections.join("\n\n")
}

fn build_external_agent_user_prompt(
    runtime_truth: &str,
    conversation: &[ConversationEntry],
    current_message: &str,
) -> String {
    let mut sections = Vec::new();
    if !runtime_truth.trim().is_empty() {
        sections.push(format!(
            "<codex_runtime_truth>\n{}\n</codex_runtime_truth>",
            runtime_truth.trim()
        ));
    }
    let (conversation, omitted_conversation) = bounded_conversation_entries(conversation);
    if !conversation.is_empty() {
        let conversation_lines = conversation
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                format!("[{}] {}: {}", index + 1, entry.role.as_str(), entry.text)
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "<subagent_conversation>\n{conversation_lines}\n</subagent_conversation>"
        ));
    }
    if omitted_conversation {
        sections.push(
            "<subagent_conversation_omission>Earlier delegated turns were omitted to keep the prompt bounded.</subagent_conversation_omission>"
                .to_string(),
        );
    }
    sections.push(format!(
        "<current_message>\n{}\n</current_message>",
        current_message.trim()
    ));
    sections.join("\n\n")
}

fn build_external_agent_continuation_prompt(runtime_truth: &str, current_message: &str) -> String {
    let mut sections = vec![
        "<claude_code_session_continuation>\ntrue\n</claude_code_session_continuation>".to_string(),
    ];
    if !runtime_truth.trim().is_empty() {
        sections.push(format!(
            "<codex_runtime_truth>\n{}\n</codex_runtime_truth>",
            runtime_truth.trim()
        ));
    }
    sections.push(format!(
        "<current_message>\n{}\n</current_message>",
        current_message.trim()
    ));
    sections.join("\n\n")
}

fn render_external_tool_summary(
    direct_claude_tools: Option<&[String]>,
    codex_mcp_bridge_available: bool,
) -> String {
    let mut sections = vec![
        "<codex_tool_inventory>".to_string(),
        "The following capability summary is authoritative for this delegated Claude turn."
            .to_string(),
    ];
    match direct_claude_tools {
        Some(tools) if !tools.is_empty() => sections.push(format!(
            "Current direct Claude Code carrier tool allowlist: {}",
            tools.join(", ")
        )),
        _ => sections.push(
            "Current direct Claude Code carrier tools are controlled by Claude Code defaults for this session."
                .to_string(),
        ),
    }
    if codex_mcp_bridge_available {
        sections.push(
            "Current Codex bridge tool inventory: mcp__codex__codex, mcp__codex__codex-reply, mcp__codex__codex-shell"
                .to_string(),
        );
        sections.push(
            "`mcp__codex__codex` accepts `model-provider` when this delegated Claude turn needs a specific Codex provider such as `openai` or `claude_code`."
                .to_string(),
        );
    }
    sections.push("</codex_tool_inventory>".to_string());
    sections.join("\n")
}

async fn render_external_available_models(
    runtime_snapshot: &crate::codex::ExternalAgentPromptSnapshot,
    auth_manager: &Arc<crate::AuthManager>,
    visible_provider_ids: &[String],
    default_mode_request_user_input: bool,
) -> Option<String> {
    let mut provider_sections = Vec::new();
    for provider_id in visible_provider_ids {
        let Some(provider) = runtime_snapshot.model_providers.get(provider_id) else {
            continue;
        };
        let manager = ModelsManager::new_with_provider(
            runtime_snapshot.codex_home.clone(),
            Arc::clone(auth_manager),
            /*model_catalog*/ None,
            CollaborationModesConfig {
                default_mode_request_user_input,
            },
            provider.clone(),
        );
        let models = manager.list_models(RefreshStrategy::Offline).await;
        let visible_models = models
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .map(|preset| format!("{} (`{}`)", preset.display_name, preset.model))
            .collect::<Vec<_>>();
        if visible_models.is_empty() {
            continue;
        }
        provider_sections.push(format!(
            "{} [{}]: {}",
            render_provider_family_label(provider_id),
            provider_id,
            visible_models.join(", ")
        ));
    }
    if provider_sections.is_empty() {
        return None;
    }
    Some(format!(
        "<codex_available_models>\nCurrent picker-visible models by provider:\n{}\n</codex_available_models>",
        provider_sections.join("\n")
    ))
}

fn render_provider_family_label(provider_id: &str) -> &'static str {
    match provider_id {
        crate::OPENAI_PROVIDER_ID => "OpenAI",
        crate::CLAUDE_CODE_PROVIDER_ID | crate::ANTHROPIC_PROVIDER_ID => "Anthropic",
        _ => "Provider",
    }
}

fn bounded_conversation_entries(
    conversation: &[ConversationEntry],
) -> (&[ConversationEntry], bool) {
    if conversation.len() > MAX_EXTERNAL_AGENT_CONVERSATION_ENTRIES {
        (
            &conversation[conversation.len() - MAX_EXTERNAL_AGENT_CONVERSATION_ENTRIES..],
            true,
        )
    } else {
        (conversation, false)
    }
}

pub(crate) fn default_claude_model(model: Option<&str>) -> String {
    match model {
        Some(model)
            if model == "opus"
                || model == "haiku"
                || model == "sonnet"
                || model.starts_with("claude-")
                || model.starts_with("claude_") =>
        {
            model.to_string()
        }
        _ => "claude-opus-4-6".to_string(),
    }
}

pub(crate) fn render_parent_context_for_fork(
    history_items: &[codex_protocol::models::ResponseItem],
    last_n_user_turns: Option<usize>,
) -> Option<String> {
    let transcript = render_compact_transcript(history_items, last_n_user_turns);
    (!transcript.trim().is_empty()).then_some(transcript)
}

#[cfg(test)]
mod tests;
