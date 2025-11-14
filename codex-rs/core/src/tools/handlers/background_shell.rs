use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::models::BackgroundShellEndedBy;
use codex_protocol::models::BackgroundShellKillParams;
use codex_protocol::models::BackgroundShellLogParams;
use codex_protocol::models::BackgroundShellResumeParams;
use codex_protocol::models::BackgroundShellRunToolCallParams;
use codex_protocol::models::BackgroundShellRunToolResult;
use codex_protocol::models::BackgroundShellStartMode;
use codex_protocol::models::BackgroundShellStatus;
use codex_protocol::models::BackgroundShellSummaryParams;
use codex_protocol::models::BackgroundShellSummaryResult;
use codex_protocol::models::ShellToolCallParams;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::background_shell::BackgroundShellManager;
use crate::background_shell::DEFAULT_FOREGROUND_BUDGET_MS;
use crate::background_shell::ProcessRunContext;
use crate::background_shell::ShellProcessRequest;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::handlers::shell::ShellHandler;
use crate::tools::registry::ToolHandler;

pub struct BackgroundShellHandler;

#[async_trait]
impl ToolHandler for BackgroundShellHandler {
    fn kind(&self) -> crate::tools::registry::ToolKind {
        crate::tools::registry::ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        match invocation.tool_name.as_str() {
            "shell_run" => self.handle_shell_run(invocation).await,
            "shell_summary" => self.handle_shell_summary(invocation).await,
            "shell_log" => self.handle_shell_log(invocation).await,
            "shell_kill" => self.handle_shell_kill(invocation).await,
            "shell_resume" => self.handle_shell_resume(invocation).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported background shell tool: {other}"
            ))),
        }
    }
}

impl BackgroundShellHandler {
    async fn handle_shell_run(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args = parse_args::<BackgroundShellRunToolCallParams>(&invocation.payload)?;
        if args.command.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "shell_run requires at least one command token".to_string(),
            ));
        }

        let start_mode = resolve_start_mode(args.start_mode, args.timeout_ms);
        if args.timeout_ms == 0 {
            return Err(FunctionCallError::RespondToModel(
                "shell_run timeout_ms must be greater than 0".to_string(),
            ));
        }
        let shell_params = ShellToolCallParams {
            command: args.command.clone(),
            workdir: args.workdir.clone(),
            timeout_ms: Some(args.timeout_ms),
            with_escalated_permissions: args.with_escalated_permissions,
            justification: args.justification.clone(),
        };
        let exec_params = ShellHandler::to_exec_params(shell_params, invocation.turn.as_ref());

        let call_id = format!("shell:{}:exec", Uuid::new_v4().simple());
        let cancel_token = CancellationToken::new();

        let manager = Arc::clone(&invocation.session.services.background_shell);
        let run_ctx = manager
            .register_process(ShellProcessRequest {
                call_id: call_id.clone(),
                exec_params: exec_params.clone(),
                friendly_label: args.friendly_label.clone(),
                start_mode,
                cancel_token: cancel_token.clone(),
                session: Arc::clone(&invocation.session),
                sub_id: invocation.turn.sub_id.clone(),
            })
            .await;

        spawn_shell_task(
            manager,
            run_ctx.clone(),
            Arc::clone(&invocation.session),
            Arc::clone(&invocation.turn),
            Arc::clone(&invocation.tracker),
        )
        .await;

        if matches!(run_ctx.start_mode, BackgroundShellStartMode::Foreground) {
            if let Some(handle) = &run_ctx.foreground_state {
                handle.wait_for_terminal().await;
            }
        }

        let result = BackgroundShellRunToolResult {
            shell_id: run_ctx.shell_id,
            start_mode: run_ctx.start_mode,
            status: BackgroundShellStatus::Pending,
        };
        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result)
                .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_summary(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let params = parse_args::<BackgroundShellSummaryParams>(&invocation.payload)?;
        let result = BackgroundShellSummaryResult {
            processes: invocation
                .session
                .services
                .background_shell
                .summaries(&params)
                .await,
        };
        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result)
                .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_log(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let params = parse_args::<BackgroundShellLogParams>(&invocation.payload)?;
        let log = invocation
            .session
            .services
            .background_shell
            .read_log(&params)
            .await
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(format!("unknown shell_id {}", params.shell_id))
            })?;
        Ok(ToolOutput::Function {
            content: serde_json::to_string(&log)
                .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_kill(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let mut params = parse_args::<BackgroundShellKillParams>(&invocation.payload)?;
        if params.shell_id.is_none() && params.pid.is_none() {
            return Err(FunctionCallError::RespondToModel(
                "shell_kill requires shell_id or pid".to_string(),
            ));
        }
        if params.initiator.is_none() {
            params.initiator = Some(BackgroundShellEndedBy::Agent);
        }
        let result = invocation
            .session
            .services
            .background_shell
            .kill_process(&params)
            .await;
        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result)
                .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_resume(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let params = parse_args::<BackgroundShellResumeParams>(&invocation.payload)?;
        let manager = Arc::clone(&invocation.session.services.background_shell);
        let (result, run_ctx) = manager.prepare_resume(&params).await;
        if let Some(ctx) = run_ctx {
            spawn_shell_task(
                manager,
                ctx,
                Arc::clone(&invocation.session),
                Arc::clone(&invocation.turn),
                Arc::clone(&invocation.tracker),
            )
            .await;
        }
        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result)
                .map_err(|e| FunctionCallError::Fatal(e.to_string()))?,
            content_items: None,
            success: Some(true),
        })
    }
}

fn resolve_start_mode(
    explicit: Option<BackgroundShellStartMode>,
    timeout_ms: u64,
) -> BackgroundShellStartMode {
    match explicit {
        Some(mode) => mode,
        None if timeout_ms > DEFAULT_FOREGROUND_BUDGET_MS => BackgroundShellStartMode::Background,
        None => BackgroundShellStartMode::Foreground,
    }
}

pub(crate) async fn spawn_shell_task(
    manager: Arc<BackgroundShellManager>,
    run_ctx: ProcessRunContext,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    tracker: SharedTurnDiffTracker,
) {
    let cancel = run_ctx.cancel_token.clone();
    let exec_params = run_ctx.exec_params.clone();
    let call_id = run_ctx.call_id.clone();
    let handle = tokio::spawn(async move {
        let fut = ShellHandler::run_exec_like(
            "shell",
            exec_params,
            session,
            turn,
            tracker,
            call_id,
            false,
        );
        tokio::pin!(fut);
        tokio::select! {
            res = &mut fut => {
                if let Err(err) = res {
                    tracing::error!(?err, "background shell execution failed");
                }
            }
            _ = cancel.cancelled() => {
                tracing::debug!("background shell task cancelled");
            }
        }
    });
    manager.attach_task(&run_ctx.shell_id, handle).await;
}

fn parse_args<T: serde::de::DeserializeOwned>(
    payload: &crate::tools::context::ToolPayload,
) -> Result<T, FunctionCallError> {
    match payload {
        crate::tools::context::ToolPayload::Function { arguments } => {
            serde_json::from_str(arguments)
                .map_err(|e| FunctionCallError::RespondToModel(format!("invalid arguments: {e}")))
        }
        other => Err(FunctionCallError::RespondToModel(format!(
            "unsupported payload for background shell tool: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_start_mode_defaults_to_foreground() {
        assert_eq!(
            resolve_start_mode(None, DEFAULT_FOREGROUND_BUDGET_MS),
            BackgroundShellStartMode::Foreground
        );
    }

    #[test]
    fn resolve_start_mode_switches_to_background_for_long_timeouts() {
        assert_eq!(
            resolve_start_mode(None, DEFAULT_FOREGROUND_BUDGET_MS + 1),
            BackgroundShellStartMode::Background
        );
    }

    #[test]
    fn resolve_start_mode_respects_explicit_value() {
        assert_eq!(
            resolve_start_mode(Some(BackgroundShellStartMode::Background), 100),
            BackgroundShellStartMode::Background
        );
    }
}
