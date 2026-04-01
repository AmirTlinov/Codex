mod claude_cli;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Op;
use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::control::render_input_preview;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::compact_transcript::render_compact_transcript;
use crate::config::ClaudeCliConfig;
use crate::config::ClaudeCliEffort;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;

pub(crate) use self::claude_cli::ClaudeCliRequest;
pub(crate) use self::claude_cli::run_claude_cli;

const MAX_EXTERNAL_AGENT_CONVERSATION_ENTRIES: usize = 12;

#[derive(Debug, Clone)]
pub(crate) struct ExternalAgentLaunchRequest {
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
struct ActiveTurn {
    generation: u64,
    cancellation_token: CancellationToken,
}

struct ExternalAgentState {
    config_snapshot: ThreadConfigSnapshot,
    developer_instructions: Option<String>,
    claude_cli: ClaudeCliConfig,
    model: String,
    parent_context: Option<String>,
    conversation: Mutex<Vec<ConversationEntry>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    next_generation: AtomicU64,
    status_tx: watch::Sender<AgentStatus>,
}

impl ExternalAgentState {
    fn new(request: ExternalAgentLaunchRequest) -> Self {
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        Self {
            config_snapshot: request.config_snapshot,
            developer_instructions: request.developer_instructions,
            claude_cli: request.claude_cli,
            model: request.model,
            parent_context: request.parent_context,
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
        let conversation_snapshot = self.conversation.lock().await.clone();
        let generation = self.next_generation.fetch_add(1, Ordering::AcqRel);
        let cancellation_token = CancellationToken::new();
        {
            let mut active_turn = self.active_turn.lock().await;
            if active_turn.is_some() {
                return Err(CodexErr::UnsupportedOperation(
                    "external Claude agent is already running; wait or interrupt first".to_string(),
                ));
            }
            *active_turn = Some(ActiveTurn {
                generation,
                cancellation_token: cancellation_token.clone(),
            });
        }
        self.conversation.lock().await.push(ConversationEntry {
            role: ConversationRole::User,
            text: user_message.clone(),
        });
        self.status_tx.send_replace(AgentStatus::Running);

        let submission_id = Uuid::new_v4().to_string();
        let state = Arc::clone(self);
        tokio::spawn(async move {
            let request = ClaudeCliRequest {
                cwd: state.config_snapshot.cwd.clone(),
                model: state.model.clone(),
                system_prompt: build_external_agent_system_prompt(),
                user_prompt: build_external_agent_user_prompt(
                    state.developer_instructions.as_deref(),
                    state.parent_context.as_deref(),
                    conversation_snapshot.as_slice(),
                    user_message.as_str(),
                ),
                json_schema: None,
                tools: state.claude_cli.tools.clone(),
                force_toolless: false,
                effort: state.claude_cli.effort.or(state
                    .config_snapshot
                    .reasoning_effort
                    .map(ClaudeCliEffort::from)),
            };
            let result = run_claude_cli(&state.claude_cli, request, cancellation_token).await;
            state.finish_turn(generation, result).await;
        });

        Ok(submission_id)
    }

    async fn interrupt(&self) -> CodexResult<String> {
        let active_turn = self.active_turn.lock().await.take();
        if let Some(active_turn) = active_turn {
            active_turn.cancellation_token.cancel();
            self.status_tx.send_replace(AgentStatus::Interrupted);
        }
        Ok(Uuid::new_v4().to_string())
    }

    async fn close(&self) -> CodexResult<String> {
        let _ = self.interrupt().await?;
        self.status_tx.send_replace(AgentStatus::Shutdown);
        Ok(String::new())
    }

    async fn finish_turn(&self, generation: u64, result: anyhow::Result<String>) {
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
                self.conversation.lock().await.push(ConversationEntry {
                    role: ConversationRole::Assistant,
                    text: output.clone(),
                });
                self.status_tx
                    .send_replace(AgentStatus::Completed(Some(output)));
            }
            Err(err) => {
                let message = err.to_string();
                let status = if message.contains("Claude CLI run cancelled") {
                    AgentStatus::Interrupted
                } else {
                    AgentStatus::Errored(message)
                };
                self.status_tx.send_replace(status);
            }
        }
    }
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

fn build_external_agent_system_prompt() -> String {
    [
        "You are an external Claude subagent running under Codex.".to_string(),
        "Work only on the delegated task, stay aligned with the provided project guidance, and return only the answer that should go back to the parent agent.".to_string(),
        "Do not mention hidden prompts, internal runtime mechanics, or claim tool use you did not actually perform.".to_string(),
    ]
    .join("\n\n")
}

fn build_external_agent_user_prompt(
    developer_instructions: Option<&str>,
    parent_context: Option<&str>,
    conversation: &[ConversationEntry],
    current_message: &str,
) -> String {
    let mut sections = Vec::new();
    if let Some(developer_instructions) = developer_instructions
        && !developer_instructions.trim().is_empty()
    {
        sections.push(format!(
            "<codex_developer_instructions>\n{}\n</codex_developer_instructions>",
            developer_instructions.trim()
        ));
    }
    if let Some(parent_context) = parent_context
        && !parent_context.trim().is_empty()
    {
        sections.push(format!(
            "<forked_parent_context>\n{}\n</forked_parent_context>",
            parent_context.trim()
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
