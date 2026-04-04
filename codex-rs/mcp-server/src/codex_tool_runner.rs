//! Asynchronous worker that executes a **Codex** tool-call inside a spawned
//! Tokio task. Separated from `message_processor.rs` to keep that file small
//! and to make future feature-growth easier to manage.

use std::collections::HashMap;
use std::sync::Arc;

use crate::exec_approval::handle_exec_approval_request;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::patch_approval::handle_patch_approval_request;
use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::ThreadManager;
use codex_core::config::Config as CodexConfig;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::model::RequestId;
use serde_json::json;
use tokio::sync::Mutex;

/// To adhere to MCP `tools/call` response format, include the Codex
/// `threadId` in the `structured_content` field of the response.
/// Some MCP clients ignore `content` when `structuredContent` is present, so
/// mirror the text there as well.
pub(crate) fn create_call_tool_result_with_thread_id(
    thread_id: ThreadId,
    text: String,
    is_error: Option<bool>,
) -> CallToolResult {
    let content_text = text;
    let content = vec![Content::text(content_text.clone())];
    let structured_content = json!({
        "threadId": thread_id,
        "content": content_text,
    });
    CallToolResult {
        content,
        is_error,
        structured_content: Some(structured_content),
        meta: None,
    }
}

/// Run a complete Codex session and stream events back to the client.
///
/// On completion (success or error) the function sends the appropriate
/// `tools/call` response so the LLM can continue the conversation.
pub async fn run_codex_tool_session(
    id: RequestId,
    initial_prompt: String,
    config: CodexConfig,
    outgoing: Arc<OutgoingMessageSender>,
    thread_manager: Arc<ThreadManager>,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
) {
    let NewThread {
        thread_id,
        thread,
        session_configured,
    } = match thread_manager.start_thread(config).await {
        Ok(res) => res,
        Err(e) => {
            let result = CallToolResult {
                content: vec![Content::text(format!("Failed to start Codex session: {e}"))],
                is_error: Some(true),
                structured_content: None,
                meta: None,
            };
            outgoing.send_response(id.clone(), result).await;
            return;
        }
    };

    let session_configured_event = Event {
        // Use a fake id value for now.
        id: "".to_string(),
        msg: EventMsg::SessionConfigured(session_configured.clone()),
    };
    outgoing
        .send_event_as_notification(
            &session_configured_event,
            Some(OutgoingNotificationMeta {
                request_id: Some(id.clone()),
                thread_id: Some(thread_id),
            }),
        )
        .await;

    // Use the original MCP request ID as the `sub_id` for the Codex submission so that
    // any events emitted for this tool-call can be correlated with the
    // originating `tools/call` request.
    let sub_id = id.to_string();
    running_requests_id_to_codex_uuid
        .lock()
        .await
        .insert(id.clone(), thread_id);
    if let Err(e) = thread
        .submit_with_id(Submission {
            id: sub_id.clone(),
            op: Op::UserInput {
                items: vec![UserInput::Text {
                    text: initial_prompt.clone(),
                    // MCP tool prompts are plain text with no UI element ranges.
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            },
            trace: None,
        })
        .await
    {
        tracing::error!("Failed to submit initial prompt: {e}");
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!("Failed to submit initial prompt: {e}"),
            Some(true),
        );
        outgoing.send_response(id.clone(), result).await;
        // unregister the id so we don't keep it in the map
        running_requests_id_to_codex_uuid.lock().await.remove(&id);
        return;
    }

    run_codex_tool_session_inner(
        thread_id,
        thread,
        outgoing,
        id,
        running_requests_id_to_codex_uuid,
        ShellCaptureState::default(),
    )
    .await;
}

pub async fn run_codex_shell_tool_session(
    id: RequestId,
    initial_prompt: String,
    config: CodexConfig,
    outgoing: Arc<OutgoingMessageSender>,
    thread_manager: Arc<ThreadManager>,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
) {
    let NewThread {
        thread_id,
        thread,
        session_configured,
    } = match thread_manager.start_thread(config).await {
        Ok(res) => res,
        Err(e) => {
            let result = CallToolResult {
                content: vec![Content::text(format!(
                    "Failed to start Codex shell session: {e}"
                ))],
                is_error: Some(true),
                structured_content: None,
                meta: None,
            };
            outgoing.send_response(id.clone(), result).await;
            return;
        }
    };

    let session_configured_event = Event {
        id: "".to_string(),
        msg: EventMsg::SessionConfigured(session_configured.clone()),
    };
    outgoing
        .send_event_as_notification(
            &session_configured_event,
            Some(OutgoingNotificationMeta {
                request_id: Some(id.clone()),
                thread_id: Some(thread_id),
            }),
        )
        .await;

    let sub_id = id.to_string();
    running_requests_id_to_codex_uuid
        .lock()
        .await
        .insert(id.clone(), thread_id);
    let expected_command = initial_prompt.clone();
    if let Err(e) = thread
        .submit_with_id(Submission {
            id: sub_id.clone(),
            op: Op::UserInput {
                items: vec![UserInput::Text {
                    text: crate::codex_tool_config::CodexShellToolCallParam::bridge_prompt(
                        &initial_prompt,
                    ),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            },
            trace: None,
        })
        .await
    {
        tracing::error!("Failed to submit Codex shell request: {e}");
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!("Failed to submit Codex shell request: {e}"),
            Some(true),
        );
        outgoing.send_response(id.clone(), result).await;
        running_requests_id_to_codex_uuid.lock().await.remove(&id);
        return;
    }

    run_codex_tool_session_inner(
        thread_id,
        thread,
        outgoing,
        id,
        running_requests_id_to_codex_uuid,
        ShellCaptureState::new(Some(expected_command)),
    )
    .await;
}

pub async fn run_codex_tool_session_reply(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    prompt: String,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
) {
    running_requests_id_to_codex_uuid
        .lock()
        .await
        .insert(request_id.clone(), thread_id);
    if let Err(e) = thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt,
                // MCP tool prompts are plain text with no UI element ranges.
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
    {
        tracing::error!("Failed to submit user input: {e}");
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!("Failed to submit user input: {e}"),
            Some(true),
        );
        outgoing.send_response(request_id.clone(), result).await;
        // unregister the id so we don't keep it in the map
        running_requests_id_to_codex_uuid
            .lock()
            .await
            .remove(&request_id);
        return;
    }

    run_codex_tool_session_inner(
        thread_id,
        thread,
        outgoing,
        request_id,
        running_requests_id_to_codex_uuid,
        ShellCaptureState::default(),
    )
    .await;
}

async fn run_codex_tool_session_inner(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    shell_capture: ShellCaptureState,
) {
    let request_id_str = request_id.to_string();
    let mut shell_capture = shell_capture;

    // Stream events until the task needs to pause for user interaction or
    // completes.
    loop {
        match thread.next_event().await {
            Ok(event) => {
                let should_publish =
                    match shell_capture.should_publish_event_notification(&event.msg) {
                        Ok(should_publish) => should_publish,
                        Err(error) => {
                            let result = create_call_tool_result_with_thread_id(
                                thread_id,
                                error,
                                /*is_error*/ Some(true),
                            );
                            outgoing.send_response(request_id.clone(), result).await;
                            running_requests_id_to_codex_uuid
                                .lock()
                                .await
                                .remove(&request_id);
                            break;
                        }
                    };

                if should_publish {
                    outgoing
                        .send_event_as_notification(
                            &event,
                            Some(OutgoingNotificationMeta {
                                request_id: Some(request_id.clone()),
                                thread_id: Some(thread_id),
                            }),
                        )
                        .await;
                }

                match event.msg {
                    EventMsg::ExecApprovalRequest(ev) => {
                        let approval_id = ev.effective_approval_id();
                        let ExecApprovalRequestEvent {
                            turn_id: _,
                            command,
                            cwd,
                            call_id,
                            approval_id: _,
                            reason: _,
                            proposed_execpolicy_amendment: _,
                            proposed_network_policy_amendments: _,
                            parsed_cmd,
                            network_approval_context: _,
                            additional_permissions: _,
                            available_decisions: _,
                        } = ev;
                        handle_exec_approval_request(
                            command,
                            cwd,
                            outgoing.clone(),
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            call_id,
                            approval_id,
                            parsed_cmd,
                            thread_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::PlanDelta(_) => {
                        continue;
                    }
                    EventMsg::Error(err_event) => {
                        // Always respond in tools/call's expected shape, and include conversationId so the client can resume.
                        let result = create_call_tool_result_with_thread_id(
                            thread_id,
                            err_event.message,
                            Some(true),
                        );
                        outgoing.send_response(request_id.clone(), result).await;
                        break;
                    }
                    EventMsg::Warning(_) => {
                        continue;
                    }
                    EventMsg::GuardianAssessment(_) => {
                        continue;
                    }
                    EventMsg::ElicitationRequest(_) => {
                        continue;
                    }
                    EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id,
                        turn_id: _,
                        reason,
                        grant_root,
                        changes,
                    }) => {
                        handle_patch_approval_request(
                            call_id,
                            reason,
                            grant_root,
                            changes,
                            outgoing.clone(),
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            thread_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::TurnComplete(TurnCompleteEvent {
                        last_agent_message, ..
                    }) => {
                        let text = match shell_capture.finish_turn(last_agent_message) {
                            Ok(text) => text,
                            Err(error) => {
                                let result = create_call_tool_result_with_thread_id(
                                    thread_id,
                                    error,
                                    /*is_error*/ Some(true),
                                );
                                outgoing.send_response(request_id.clone(), result).await;
                                running_requests_id_to_codex_uuid
                                    .lock()
                                    .await
                                    .remove(&request_id);
                                break;
                            }
                        };
                        let result = create_call_tool_result_with_thread_id(
                            thread_id, text, /*is_error*/ None,
                        );
                        outgoing.send_response(request_id.clone(), result).await;
                        // unregister the id so we don't keep it in the map
                        running_requests_id_to_codex_uuid
                            .lock()
                            .await
                            .remove(&request_id);
                        break;
                    }
                    EventMsg::SessionConfigured(_) => {
                        tracing::error!("unexpected SessionConfigured event");
                    }
                    EventMsg::ThreadNameUpdated(_) => {
                        // Ignore session metadata updates in MCP tool runner.
                    }
                    EventMsg::AgentMessageDelta(_) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::AgentReasoningDelta(_) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::McpStartupUpdate(_) | EventMsg::McpStartupComplete(_) => {
                        // Ignored in MCP tool runner.
                    }
                    EventMsg::AgentMessage(AgentMessageEvent { .. }) => {
                        // TODO: think how we want to support this in the MCP
                    }
                    EventMsg::ExecCommandBegin(_) => {}
                    EventMsg::ExecCommandEnd(_) => {}
                    EventMsg::RequestPermissions(_) => {}
                    EventMsg::DynamicToolCallRequest(_) => {}
                    EventMsg::McpToolCallBegin(_) | EventMsg::McpToolCallEnd(_) => {}
                    EventMsg::PatchApplyBegin(_) | EventMsg::PatchApplyEnd(_) => {}
                    EventMsg::WebSearchBegin(_) | EventMsg::WebSearchEnd(_) => {}
                    EventMsg::ViewImageToolCall(_) => {}
                    EventMsg::ImageGenerationBegin(_) | EventMsg::ImageGenerationEnd(_) => {}
                    EventMsg::AgentReasoningRawContent(_)
                    | EventMsg::AgentReasoningRawContentDelta(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::AgentReasoning(_)
                    | EventMsg::AgentReasoningSectionBreak(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::ListSkillsResponse(_)
                    | EventMsg::TerminalInteraction(_)
                    | EventMsg::ExecCommandOutputDelta(_)
                    | EventMsg::BackgroundEvent(_)
                    | EventMsg::StreamError(_)
                    | EventMsg::TurnDiff(_)
                    | EventMsg::GetHistoryEntryResponse(_)
                    | EventMsg::PlanUpdate(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::UserMessage(_)
                    | EventMsg::ShutdownComplete
                    | EventMsg::RawResponseItem(_)
                    | EventMsg::EnteredReviewMode(_)
                    | EventMsg::ItemStarted(_)
                    | EventMsg::ItemCompleted(_)
                    | EventMsg::HookStarted(_)
                    | EventMsg::HookCompleted(_)
                    | EventMsg::AgentMessageContentDelta(_)
                    | EventMsg::ReasoningContentDelta(_)
                    | EventMsg::ReasoningRawContentDelta(_)
                    | EventMsg::SkillsUpdateAvailable
                    | EventMsg::UndoStarted(_)
                    | EventMsg::UndoCompleted(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::DynamicToolCallResponse(_)
                    | EventMsg::ContextCompacted(_)
                    | EventMsg::ModelReroute(_)
                    | EventMsg::ThreadRolledBack(_)
                    | EventMsg::CollabAgentSpawnBegin(_)
                    | EventMsg::CollabAgentSpawnEnd(_)
                    | EventMsg::CollabAgentInteractionBegin(_)
                    | EventMsg::CollabAgentInteractionEnd(_)
                    | EventMsg::CollabWaitingBegin(_)
                    | EventMsg::CollabWaitingEnd(_)
                    | EventMsg::CollabCloseBegin(_)
                    | EventMsg::CollabCloseEnd(_)
                    | EventMsg::CollabResumeBegin(_)
                    | EventMsg::CollabResumeEnd(_)
                    | EventMsg::RealtimeConversationStarted(_)
                    | EventMsg::RealtimeConversationRealtime(_)
                    | EventMsg::RealtimeConversationClosed(_)
                    | EventMsg::DeprecationNotice(_) => {
                        // For now, we do not do anything extra for these
                        // events. Note that
                        // send(codex_event_to_notification(&event)) above has
                        // already dispatched these events as notifications,
                        // though we may want to do give different treatment to
                        // individual events in the future.
                    }
                    EventMsg::RequestUserInput(_) => {}
                }
            }
            Err(e) => {
                let result = create_call_tool_result_with_thread_id(
                    thread_id,
                    format!("Codex runtime error: {e}"),
                    Some(true),
                );
                outgoing.send_response(request_id.clone(), result).await;
                break;
            }
        }
    }
}

#[derive(Default)]
struct ShellCaptureState {
    expected_command: Option<String>,
    approval_count: u32,
    exec_count: u32,
    completion_text: Option<String>,
}

impl ShellCaptureState {
    fn new(expected_command: Option<String>) -> Self {
        Self {
            expected_command,
            ..Default::default()
        }
    }

    fn observe_exec_approval_request(&mut self, command: &[String]) -> Result<(), String> {
        if self.expected_command.is_none() {
            return Ok(());
        }
        self.approval_count += 1;
        if self.approval_count > 1 {
            return Err("codex-shell worker requested more than one shell approval".to_string());
        }
        self.validate_expected_command(command, "requested approval for")
    }

    fn observe_exec_command_begin(&mut self, command: &[String]) -> Result<(), String> {
        self.exec_count += 1;
        if self.exec_count > 1 {
            return Err("codex-shell worker attempted more than one shell command".to_string());
        }
        self.validate_expected_command(command, "executed")
    }

    fn capture_exec_output(&mut self, formatted_output: String) -> Result<(), String> {
        if self.expected_command.is_some() && self.exec_count == 0 {
            return Err(
                "codex-shell worker reported shell output before executing the command".to_string(),
            );
        }
        self.completion_text = Some(formatted_output);
        Ok(())
    }

    fn finish_turn(&mut self, last_agent_message: Option<String>) -> Result<String, String> {
        if self.expected_command.is_none() {
            return Ok(last_agent_message.unwrap_or_default());
        }
        self.ensure_turn_can_complete()?;
        self.completion_text
            .take()
            .ok_or_else(|| "codex-shell worker completed without shell output".to_string())
    }

    fn reject_non_shell_surface(&self, surface: &str) -> Option<String> {
        self.expected_command.as_ref().map(|_| {
            format!("codex-shell worker attempted unsupported non-shell surface: {surface}")
        })
    }

    fn should_publish_event_notification(&mut self, event: &EventMsg) -> Result<bool, String> {
        match event {
            EventMsg::ExecApprovalRequest(ev) => {
                self.observe_exec_approval_request(&ev.command)?;
                Ok(true)
            }
            EventMsg::ExecCommandBegin(event) => {
                self.observe_exec_command_begin(&event.command)?;
                Ok(true)
            }
            EventMsg::ExecCommandEnd(event) => {
                self.capture_exec_output(event.formatted_output.clone())?;
                Ok(true)
            }
            EventMsg::TurnComplete(_) => {
                self.ensure_turn_can_complete()?;
                Ok(true)
            }
            EventMsg::ElicitationRequest(_) => self
                .reject_non_shell_surface("elicitation requests")
                .map_or(Ok(true), Err),
            EventMsg::ApplyPatchApprovalRequest(_)
            | EventMsg::PatchApplyBegin(_)
            | EventMsg::PatchApplyEnd(_) => self
                .reject_non_shell_surface("patch application")
                .map_or(Ok(true), Err),
            EventMsg::RequestPermissions(_) => self
                .reject_non_shell_surface("request_permissions")
                .map_or(Ok(true), Err),
            EventMsg::RequestUserInput(_) => self
                .reject_non_shell_surface("request_user_input")
                .map_or(Ok(true), Err),
            EventMsg::DynamicToolCallRequest(_) => self
                .reject_non_shell_surface("dynamic tool calls")
                .map_or(Ok(true), Err),
            EventMsg::McpToolCallBegin(_) | EventMsg::McpToolCallEnd(_) => self
                .reject_non_shell_surface("MCP tool calls")
                .map_or(Ok(true), Err),
            EventMsg::WebSearchBegin(_) | EventMsg::WebSearchEnd(_) => self
                .reject_non_shell_surface("web search")
                .map_or(Ok(true), Err),
            EventMsg::ViewImageToolCall(_) => self
                .reject_non_shell_surface("view_image")
                .map_or(Ok(true), Err),
            EventMsg::ImageGenerationBegin(_) | EventMsg::ImageGenerationEnd(_) => self
                .reject_non_shell_surface("image generation")
                .map_or(Ok(true), Err),
            EventMsg::Warning(_) => Ok(self.expected_command.is_none()),
            EventMsg::RawResponseItem(_) => Ok(self.expected_command.is_none()),
            _ => Ok(true),
        }
    }

    fn ensure_turn_can_complete(&self) -> Result<(), String> {
        if self.expected_command.is_some() && self.exec_count == 0 {
            return Err(
                "codex-shell worker did not execute the requested shell command".to_string(),
            );
        }
        if self.expected_command.is_some() && self.completion_text.is_none() {
            return Err("codex-shell worker completed without shell output".to_string());
        }
        Ok(())
    }

    fn validate_expected_command(&self, command: &[String], phase: &str) -> Result<(), String> {
        if let Some(expected_command) = self.expected_command.as_deref()
            && command
                .last()
                .is_none_or(|actual| actual != expected_command)
        {
            return Err(format!(
                "codex-shell worker {phase} an unexpected command: {command:?}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::protocol::ExecCommandBeginEvent;
    use codex_protocol::protocol::ExecCommandEndEvent;
    use codex_protocol::protocol::ExecCommandSource;
    use codex_protocol::protocol::ExecCommandStatus;
    use codex_protocol::protocol::PatchApplyBeginEvent;
    use codex_protocol::protocol::RawResponseItemEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::protocol::WarningEvent;
    use pretty_assertions::assert_eq;

    #[test]
    fn call_tool_result_includes_thread_id_in_structured_content() {
        let thread_id = ThreadId::new();
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            "done".to_string(),
            /*is_error*/ None,
        );
        assert_eq!(
            result.structured_content,
            Some(json!({
                "threadId": thread_id,
                "content": "done",
            }))
        );
    }

    #[test]
    fn shell_capture_rejects_unexpected_command_at_approval_time() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        let error = shell_capture
            .observe_exec_approval_request(&[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ])
            .expect_err("unexpected approval command should fail closed");

        assert_eq!(
            error,
            "codex-shell worker requested approval for an unexpected command: [\"/bin/sh\", \"-c\", \"echo hi\"]"
        );
    }

    #[test]
    fn shell_capture_rejects_request_user_input_surface() {
        let shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.reject_non_shell_surface("request_user_input"),
            Some(
                "codex-shell worker attempted unsupported non-shell surface: request_user_input"
                    .to_string()
            )
        );
    }

    #[test]
    fn shell_capture_rejects_patch_notifications_before_publish() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::PatchApplyBegin(
                PatchApplyBeginEvent {
                    call_id: "call1234".to_string(),
                    turn_id: "turn-1".to_string(),
                    auto_approved: false,
                    changes: HashMap::new(),
                }
            )),
            Err(
                "codex-shell worker attempted unsupported non-shell surface: patch application"
                    .to_string()
            )
        );
    }

    #[test]
    fn shell_capture_suppresses_raw_non_shell_function_calls() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::RawResponseItem(
                RawResponseItemEvent {
                    item: ResponseItem::FunctionCall {
                        id: None,
                        name: "request_user_input".to_string(),
                        namespace: None,
                        arguments: "{}".to_string(),
                        call_id: "call1234".to_string(),
                    },
                }
            )),
            Ok(false)
        );
    }

    #[test]
    fn shell_capture_suppresses_warnings_before_publish() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::Warning(WarningEvent {
                message: "unexpected".to_string(),
            })),
            Ok(false)
        );
    }

    #[test]
    fn shell_capture_rejects_unexpected_command_before_exec_begin_publish() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::ExecCommandBegin(
                ExecCommandBeginEvent {
                    call_id: "call1234".to_string(),
                    process_id: None,
                    turn_id: "turn-1".to_string(),
                    command: vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "echo hi".to_string(),
                    ],
                    cwd: std::path::PathBuf::from("."),
                    parsed_cmd: Vec::new(),
                    source: ExecCommandSource::default(),
                    interaction_input: None,
                }
            )),
            Err(
                "codex-shell worker executed an unexpected command: [\"/bin/sh\", \"-c\", \"echo hi\"]"
                    .to_string()
            )
        );
    }

    #[test]
    fn shell_capture_rejects_output_before_exec_end_publish() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::ExecCommandEnd(
                ExecCommandEndEvent {
                    call_id: "call1234".to_string(),
                    process_id: None,
                    turn_id: "turn-1".to_string(),
                    command: vec!["printf codex-shell-ok".to_string()],
                    cwd: std::path::PathBuf::from("."),
                    parsed_cmd: Vec::new(),
                    source: ExecCommandSource::default(),
                    interaction_input: None,
                    stdout: "hi\n".to_string(),
                    stderr: String::new(),
                    aggregated_output: "hi\n".to_string(),
                    exit_code: 0,
                    duration: std::time::Duration::from_millis(1),
                    formatted_output: "hi".to_string(),
                    status: ExecCommandStatus::Completed,
                }
            )),
            Err(
                "codex-shell worker reported shell output before executing the command".to_string()
            )
        );
    }

    #[test]
    fn shell_capture_rejects_turn_complete_without_shell_execution_before_publish() {
        let mut shell_capture = ShellCaptureState::new(Some("printf codex-shell-ok".to_string()));

        assert_eq!(
            shell_capture.should_publish_event_notification(&EventMsg::TurnComplete(
                TurnCompleteEvent {
                    turn_id: "turn-1".to_string(),
                    last_agent_message: Some("done".to_string()),
                }
            )),
            Err("codex-shell worker did not execute the requested shell command".to_string())
        );
    }
}
