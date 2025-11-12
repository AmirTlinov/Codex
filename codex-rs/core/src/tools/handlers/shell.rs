use async_trait::async_trait;
use codex_protocol::models::ShellToolCallParams;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant as StdInstant;
use tokio::time::Instant as TokioInstant;

use crate::apply_patch;
use crate::apply_patch::InternalApplyPatchInvocation;
use crate::apply_patch::convert_apply_patch_to_protocol;
use crate::background_shell::BackgroundStartContext;
use crate::background_shell::BackgroundStartResponse;
use crate::codex::TurnContext;
use crate::command_label::friendly_command_label_from_args;
use crate::exec::ExecParams;
use crate::exec::ExecToolCallOutput;
use crate::exec_env::create_env;
use crate::foreground_shell::ForegroundCompletion;
use crate::foreground_shell::ForegroundShellState;
use crate::foreground_shell::drive_foreground_shell;
use crate::function_tool::FunctionCallError;
use crate::protocol::BackgroundShellStatus;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::runtimes::apply_patch::ApplyPatchRequest;
use crate::tools::runtimes::apply_patch::ApplyPatchRuntime;
use crate::tools::runtimes::shell::ShellBackgroundRuntime;
use crate::tools::runtimes::shell::ShellRequest;
use crate::tools::sandboxing::ToolCtx;
use crate::unified_exec::MIN_YIELD_TIME_MS;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecSessionManager;

pub struct ShellHandler;

impl ShellHandler {
    fn to_exec_params(params: &ShellToolCallParams, turn_context: &TurnContext) -> ExecParams {
        ExecParams {
            command: params.command.clone(),
            cwd: turn_context.resolve_path(params.workdir.clone()),
            timeout_ms: params.timeout_ms,
            env: create_env(&turn_context.shell_environment_policy),
            with_escalated_permissions: params.with_escalated_permissions,
            justification: params.justification.clone(),
            arg0: None,
        }
    }

    fn to_shell_request(params: &ShellToolCallParams, turn_context: &TurnContext) -> ShellRequest {
        ShellRequest {
            command: params.command.clone(),
            cwd: turn_context.resolve_path(params.workdir.clone()),
            timeout_ms: params.timeout_ms,
            env: create_env(&turn_context.shell_environment_policy),
            with_escalated_permissions: params.with_escalated_permissions,
            justification: params.justification.clone(),
            risk: None,
        }
    }

    fn background_description(explicit: Option<&str>, command: &[String]) -> Option<String> {
        let cleaned = explicit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        cleaned.or_else(|| {
            if command.is_empty() {
                None
            } else {
                shlex::try_join(command.iter().map(String::as_str))
                    .ok()
                    .or_else(|| Some(command.join(" ")))
            }
        })
    }
}

#[async_trait]
impl ToolHandler for ShellHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            payload,
            ToolPayload::Function { .. } | ToolPayload::LocalShell { .. }
        )
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
        } = invocation;

        match payload {
            ToolPayload::Function { arguments } => {
                let params: ShellToolCallParams =
                    serde_json::from_str(&arguments).map_err(|e| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to parse function arguments: {e:?}"
                        ))
                    })?;
                Self::run_exec_like(
                    tool_name.as_str(),
                    params,
                    session,
                    turn,
                    tracker,
                    call_id,
                    false,
                )
                .await
            }
            ToolPayload::LocalShell { params } => {
                Self::run_exec_like(
                    tool_name.as_str(),
                    params,
                    session,
                    turn,
                    tracker,
                    call_id,
                    true,
                )
                .await
            }
            _ => Err(FunctionCallError::RespondToModel(format!(
                "unsupported payload for shell handler: {tool_name}"
            ))),
        }
    }
}

impl ShellHandler {
    async fn run_exec_like(
        tool_name: &str,
        params: ShellToolCallParams,
        session: Arc<crate::codex::Session>,
        turn: Arc<TurnContext>,
        tracker: crate::tools::context::SharedTurnDiffTracker,
        call_id: String,
        is_user_shell_command: bool,
    ) -> Result<ToolOutput, FunctionCallError> {
        let exec_params = Self::to_exec_params(&params, turn.as_ref());

        if exec_params.with_escalated_permissions.unwrap_or(false)
            && !matches!(
                turn.approval_policy,
                codex_protocol::protocol::AskForApproval::OnRequest
            )
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}",
                policy = turn.approval_policy
            )));
        }

        match codex_apply_patch::maybe_parse_apply_patch_verified(
            &exec_params.command,
            &exec_params.cwd,
        ) {
            codex_apply_patch::MaybeApplyPatchVerified::Body(changes) => {
                match apply_patch::apply_patch(session.as_ref(), turn.as_ref(), &call_id, changes)
                    .await
                {
                    InternalApplyPatchInvocation::Output(item) => {
                        let content = item?;
                        return Ok(ToolOutput::Function {
                            content,
                            content_items: None,
                            success: Some(true),
                        });
                    }
                    InternalApplyPatchInvocation::DelegateToExec(apply) => {
                        let emitter = ToolEmitter::apply_patch(
                            convert_apply_patch_to_protocol(&apply.action),
                            !apply.user_explicitly_approved_this_action,
                        );
                        let event_ctx = ToolEventCtx::new(
                            session.as_ref(),
                            turn.as_ref(),
                            &call_id,
                            Some(&tracker),
                        );
                        emitter.begin(event_ctx).await;

                        let req = ApplyPatchRequest {
                            patch: apply.action.patch.clone(),
                            cwd: apply.action.cwd.clone(),
                            timeout_ms: exec_params.timeout_ms,
                            user_explicitly_approved: apply.user_explicitly_approved_this_action,
                            codex_exe: turn.codex_linux_sandbox_exe.clone(),
                        };
                        let mut orchestrator = ToolOrchestrator::new();
                        let mut runtime = ApplyPatchRuntime::new();
                        let tool_ctx = ToolCtx {
                            session: session.as_ref(),
                            turn: turn.as_ref(),
                            call_id: call_id.clone(),
                            tool_name: tool_name.to_string(),
                        };
                        let out = orchestrator
                            .run(&mut runtime, &req, &tool_ctx, &turn, turn.approval_policy)
                            .await;
                        let event_ctx = ToolEventCtx::new(
                            session.as_ref(),
                            turn.as_ref(),
                            &call_id,
                            Some(&tracker),
                        );
                        let content = emitter.finish(event_ctx, out).await?;
                        return Ok(ToolOutput::Function {
                            content,
                            content_items: None,
                            success: Some(true),
                        });
                    }
                }
            }
            codex_apply_patch::MaybeApplyPatchVerified::CorrectnessError(parse_error) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "apply_patch verification failed: {parse_error}"
                )));
            }
            codex_apply_patch::MaybeApplyPatchVerified::ShellParseError(error) => {
                tracing::trace!("Failed to parse shell command, {error:?}");
            }
            codex_apply_patch::MaybeApplyPatchVerified::NotApplyPatch => {}
        }

        let shell_request = Self::to_shell_request(&params, turn.as_ref());
        let bookmark = params.bookmark.clone();
        let description =
            Self::background_description(params.description.as_deref(), &shell_request.command);

        if params.run_in_background.unwrap_or(false) {
            let manager = &session.services.background_shell;
            let start_ctx = BackgroundStartContext {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn),
                tracker: &tracker,
                call_id: call_id.clone(),
                description: description.clone(),
                bookmark: bookmark.clone(),
            };
            let response = manager.start(shell_request, start_ctx).await?;
            let content = serde_json::to_string(&response).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to serialize run_in_background response: {err:?}"
                ))
            })?;
            return Ok(ToolOutput::Function {
                content,
                content_items: None,
                success: Some(true),
            });
        }

        let emitter = ToolEmitter::shell(
            exec_params.command.clone(),
            exec_params.cwd.clone(),
            is_user_shell_command,
        );
        let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
        emitter.begin(event_ctx).await;

        let manager = &session.services.unified_exec_manager;
        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = ShellBackgroundRuntime::new(manager);
        let tool_ctx = ToolCtx {
            session: session.as_ref(),
            turn: turn.as_ref(),
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
        };

        let start_tick = TokioInstant::now();
        let start_wall = StdInstant::now();
        let unified_session = orchestrator
            .run(
                &mut runtime,
                &shell_request,
                &tool_ctx,
                &turn,
                turn.approval_policy,
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to launch shell command: {err:?}"
                ))
            })?;

        let (output_buffer, output_notify) = unified_session.output_handles();
        let initial_deadline = start_tick + Duration::from_millis(MIN_YIELD_TIME_MS);
        let initial_collected = UnifiedExecSessionManager::collect_output_until_deadline(
            &output_buffer,
            &output_notify,
            initial_deadline,
        )
        .await;
        let initial_output = String::from_utf8_lossy(&initial_collected).to_string();

        if unified_session.has_exited() {
            let exit_code = unified_session.exit_code().unwrap_or(-1);
            let duration = StdInstant::now().saturating_duration_since(start_wall);
            let exec_output = ExecToolCallOutput {
                exit_code,
                stdout: crate::exec::StreamOutput::new(initial_output.clone()),
                stderr: crate::exec::StreamOutput::new(String::new()),
                aggregated_output: crate::exec::StreamOutput::new(initial_output.clone()),
                duration,
                timed_out: false,
            };
            let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
            let content = emitter.finish(event_ctx, Ok(exec_output)).await?;
            return Ok(ToolOutput::Function {
                content,
                content_items: None,
                success: Some(true),
            });
        }

        let context =
            UnifiedExecContext::new(Arc::clone(&session), Arc::clone(&turn), call_id.clone());
        let command_label = {
            let label = friendly_command_label_from_args(&exec_params.command);
            if label.is_empty() {
                exec_params.command.join(" ")
            } else {
                label
            }
        };
        let session_id = manager
            .store_session(
                unified_session,
                &context,
                &command_label,
                TokioInstant::now(),
            )
            .await;

        let (state, promotion_rx, promotion_result_rx) = ForegroundShellState::new(session_id);
        state.push_output(&initial_output).await;
        session
            .services
            .foreground_shell
            .insert(call_id.clone(), state.clone())
            .await;

        let completion = drive_foreground_shell(
            state.clone(),
            promotion_rx,
            promotion_result_rx,
            Arc::clone(&session),
            Arc::clone(&turn),
            call_id.clone(),
            command_label.clone(),
            initial_output,
            exec_params.timeout_ms,
            description.clone(),
            bookmark.clone(),
        )
        .await;

        match completion {
            ForegroundCompletion::Finished {
                exit_code,
                stdout,
                stderr,
                aggregated_output,
                duration_ms,
                timed_out,
            } => {
                session.services.foreground_shell.remove(&call_id).await;
                let exec_output = ExecToolCallOutput {
                    exit_code,
                    stdout: crate::exec::StreamOutput::new(stdout),
                    stderr: crate::exec::StreamOutput::new(stderr),
                    aggregated_output: crate::exec::StreamOutput::new(aggregated_output),
                    duration: Duration::from_millis(duration_ms as u64),
                    timed_out,
                };
                let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
                let content = emitter.finish(event_ctx, Ok(exec_output)).await?;
                Ok(ToolOutput::Function {
                    content,
                    content_items: None,
                    success: Some(true),
                })
            }
            ForegroundCompletion::Promoted(result) => {
                session.services.foreground_shell.remove(&call_id).await;
                let message = match (result.description.as_deref(), result.bookmark.as_deref()) {
                    (Some(desc), Some(alias)) => format!(
                        "Foreground shell promoted to background shell {} ({desc}, bookmark: {alias})",
                        result.shell_id
                    ),
                    (Some(desc), None) => format!(
                        "Foreground shell promoted to background shell {} ({desc})",
                        result.shell_id
                    ),
                    (None, Some(alias)) => format!(
                        "Foreground shell promoted to background shell {} (bookmark: {alias})",
                        result.shell_id
                    ),
                    (None, None) => format!(
                        "Foreground shell promoted to background shell {}",
                        result.shell_id
                    ),
                };
                let exec_output = ExecToolCallOutput {
                    exit_code: 0,
                    stdout: crate::exec::StreamOutput::new(message.clone()),
                    stderr: crate::exec::StreamOutput::new(String::new()),
                    aggregated_output: crate::exec::StreamOutput::new(message),
                    duration: Duration::ZERO,
                    timed_out: false,
                };
                let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
                emitter.finish(event_ctx, Ok(exec_output)).await?;

                let response = BackgroundStartResponse {
                    shell_id: result.shell_id,
                    status: BackgroundShellStatus::Running,
                    exit_code: None,
                    initial_output: result.initial_output,
                    description: result.description,
                    bookmark: result.bookmark,
                };
                let content = serde_json::to_string(&response).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to serialize promoted background shell response: {err:?}"
                    ))
                })?;
                Ok(ToolOutput::Function {
                    content,
                    content_items: None,
                    success: Some(true),
                })
            }
            ForegroundCompletion::Failed(err) => {
                session.services.foreground_shell.remove(&call_id).await;
                let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
                let tool_err = crate::tools::sandboxing::ToolError::Rejected(err);
                let content = emitter.finish(event_ctx, Err(tool_err)).await?;
                Ok(ToolOutput::Function {
                    content,
                    content_items: None,
                    success: Some(false),
                })
            }
        }
    }
}
