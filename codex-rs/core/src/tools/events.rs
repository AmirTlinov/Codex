use crate::codex::Session;
use crate::codex::TurnContext;
use crate::error::CodexErr;
use crate::error::SandboxErr;
use crate::exec::ExecToolCallOutput;
use crate::function_tool::FunctionCallError;
use crate::parse_command::parse_command;
use crate::protocol::ApplyPatchReport;
use crate::protocol::EventMsg;
use crate::protocol::ExecCommandBeginEvent;
use crate::protocol::ExecCommandEndEvent;
use crate::protocol::FileChange;
use crate::protocol::PatchApplyBeginEvent;
use crate::protocol::PatchApplyEndEvent;
use crate::protocol::TurnDiffEvent;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::sandboxing::ToolError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use super::format_exec_output_str;

#[derive(Clone, Copy)]
pub(crate) struct ToolEventCtx<'a> {
    pub session: &'a Session,
    pub turn: &'a TurnContext,
    pub call_id: &'a str,
    pub turn_diff_tracker: Option<&'a SharedTurnDiffTracker>,
}

impl<'a> ToolEventCtx<'a> {
    pub fn new(
        session: &'a Session,
        turn: &'a TurnContext,
        call_id: &'a str,
        turn_diff_tracker: Option<&'a SharedTurnDiffTracker>,
    ) -> Self {
        Self {
            session,
            turn,
            call_id,
            turn_diff_tracker,
        }
    }
}

pub(crate) enum ToolEventStage {
    Begin,
    Success(ExecToolCallOutput),
    Failure(ToolEventFailure),
}

pub(crate) enum ToolEventFailure {
    Output(ExecToolCallOutput),
    Message(String),
}

pub(crate) async fn emit_exec_command_begin(
    ctx: ToolEventCtx<'_>,
    command: &[String],
    cwd: &Path,
    is_user_shell_command: bool,
) {
    ctx.session
        .send_event(
            ctx.turn,
            EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
                call_id: ctx.call_id.to_string(),
                command: command.to_vec(),
                cwd: cwd.to_path_buf(),
                parsed_cmd: parse_command(command),
                is_user_shell_command,
            }),
        )
        .await;
}
// Concrete, allocation-free emitter: avoid trait objects and boxed futures.
pub(crate) enum ToolEmitter {
    Shell {
        command: Vec<String>,
        cwd: PathBuf,
        is_user_shell_command: bool,
    },
    ApplyPatch {
        changes: HashMap<PathBuf, FileChange>,
        auto_approved: bool,
    },
    UnifiedExec {
        command: String,
        cwd: PathBuf,
        // True for `exec_command` and false for `write_stdin`.
        #[allow(dead_code)]
        is_startup_command: bool,
    },
}

impl ToolEmitter {
    pub fn shell(command: Vec<String>, cwd: PathBuf, is_user_shell_command: bool) -> Self {
        Self::Shell {
            command,
            cwd,
            is_user_shell_command,
        }
    }

    pub fn apply_patch(changes: HashMap<PathBuf, FileChange>, auto_approved: bool) -> Self {
        Self::ApplyPatch {
            changes,
            auto_approved,
        }
    }

    pub fn unified_exec(command: String, cwd: PathBuf, is_startup_command: bool) -> Self {
        Self::UnifiedExec {
            command,
            cwd,
            is_startup_command,
        }
    }

    pub async fn emit(&self, ctx: ToolEventCtx<'_>, stage: ToolEventStage) {
        match (self, stage) {
            (
                Self::Shell {
                    command,
                    cwd,
                    is_user_shell_command,
                },
                ToolEventStage::Begin,
            ) => {
                emit_exec_command_begin(ctx, command, cwd.as_path(), *is_user_shell_command).await;
            }
            (Self::Shell { .. }, ToolEventStage::Success(output)) => {
                emit_exec_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.aggregated_output.text.clone(),
                    output.exit_code,
                    output.duration,
                    format_exec_output_str(&output),
                )
                .await;
            }
            (Self::Shell { .. }, ToolEventStage::Failure(ToolEventFailure::Output(output))) => {
                emit_exec_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.aggregated_output.text.clone(),
                    output.exit_code,
                    output.duration,
                    format_exec_output_str(&output),
                )
                .await;
            }
            (Self::Shell { .. }, ToolEventStage::Failure(ToolEventFailure::Message(message))) => {
                emit_exec_end(
                    ctx,
                    String::new(),
                    (*message).to_string(),
                    (*message).to_string(),
                    -1,
                    Duration::ZERO,
                    message.clone(),
                )
                .await;
            }

            (
                Self::ApplyPatch {
                    changes,
                    auto_approved,
                },
                ToolEventStage::Begin,
            ) => {
                if let Some(tracker) = ctx.turn_diff_tracker {
                    let mut guard = tracker.lock().await;
                    guard.on_patch_begin(changes);
                }
                ctx.session
                    .send_event(
                        ctx.turn,
                        EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                            call_id: ctx.call_id.to_string(),
                            auto_approved: *auto_approved,
                            changes: changes.clone(),
                        }),
                    )
                    .await;
            }
            (Self::ApplyPatch { .. }, ToolEventStage::Success(output)) => {
                emit_patch_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.exit_code == 0,
                )
                .await;
            }
            (
                Self::ApplyPatch { .. },
                ToolEventStage::Failure(ToolEventFailure::Output(output)),
            ) => {
                emit_patch_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.exit_code == 0,
                )
                .await;
            }
            (
                Self::ApplyPatch { .. },
                ToolEventStage::Failure(ToolEventFailure::Message(message)),
            ) => {
                emit_patch_end(ctx, String::new(), (*message).to_string(), false).await;
            }
            (Self::UnifiedExec { command, cwd, .. }, ToolEventStage::Begin) => {
                emit_exec_command_begin(ctx, &[command.to_string()], cwd.as_path(), false).await;
            }
            (Self::UnifiedExec { .. }, ToolEventStage::Success(output)) => {
                emit_exec_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.aggregated_output.text.clone(),
                    output.exit_code,
                    output.duration,
                    format_exec_output_str(&output),
                )
                .await;
            }
            (
                Self::UnifiedExec { .. },
                ToolEventStage::Failure(ToolEventFailure::Output(output)),
            ) => {
                emit_exec_end(
                    ctx,
                    output.stdout.text.clone(),
                    output.stderr.text.clone(),
                    output.aggregated_output.text.clone(),
                    output.exit_code,
                    output.duration,
                    format_exec_output_str(&output),
                )
                .await;
            }
            (
                Self::UnifiedExec { .. },
                ToolEventStage::Failure(ToolEventFailure::Message(message)),
            ) => {
                emit_exec_end(
                    ctx,
                    String::new(),
                    (*message).to_string(),
                    (*message).to_string(),
                    -1,
                    Duration::ZERO,
                    message.clone(),
                )
                .await;
            }
        }
    }

    pub async fn begin(&self, ctx: ToolEventCtx<'_>) {
        self.emit(ctx, ToolEventStage::Begin).await;
    }

    pub async fn finish(
        &self,
        ctx: ToolEventCtx<'_>,
        out: Result<ExecToolCallOutput, ToolError>,
    ) -> Result<String, FunctionCallError> {
        let (event, result) = match out {
            Ok(output) => {
                let content = super::format_exec_output_for_model(&output);
                let exit_code = output.exit_code;
                let event = ToolEventStage::Success(output);
                let result = if exit_code == 0 {
                    Ok(content)
                } else {
                    Err(FunctionCallError::RespondToModel(content))
                };
                (event, result)
            }
            Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Timeout { output })))
            | Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Denied { output }))) => {
                let response = super::format_exec_output_for_model(&output);
                let event = ToolEventStage::Failure(ToolEventFailure::Output(*output));
                let result = Err(FunctionCallError::RespondToModel(response));
                (event, result)
            }
            Err(ToolError::Codex(err)) => {
                let message = format!("execution error: {err:?}");
                let event = ToolEventStage::Failure(ToolEventFailure::Message(message.clone()));
                let result = Err(FunctionCallError::RespondToModel(message));
                (event, result)
            }
            Err(ToolError::Rejected(msg)) => {
                // Normalize common rejection messages for exec tools so tests and
                // users see a clear, consistent phrase.
                let normalized = if msg == "rejected by user" {
                    "exec command rejected by user".to_string()
                } else {
                    msg
                };
                let event = ToolEventStage::Failure(ToolEventFailure::Message(normalized.clone()));
                let result = Err(FunctionCallError::RespondToModel(normalized));
                (event, result)
            }
        };
        self.emit(ctx, event).await;
        result
    }
}

async fn emit_exec_end(
    ctx: ToolEventCtx<'_>,
    stdout: String,
    stderr: String,
    aggregated_output: String,
    exit_code: i32,
    duration: Duration,
    formatted_output: String,
) {
    ctx.session
        .send_event(
            ctx.turn,
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: ctx.call_id.to_string(),
                stdout,
                stderr,
                aggregated_output,
                exit_code,
                duration,
                formatted_output,
            }),
        )
        .await;
}

const APPLY_PATCH_MACHINE_SCHEMA: &str = "apply_patch/v2";

async fn emit_patch_end(ctx: ToolEventCtx<'_>, stdout: String, stderr: String, success: bool) {
    let report = extract_apply_patch_report(&stdout);
    ctx.session
        .send_event(
            ctx.turn,
            EventMsg::PatchApplyEnd(Box::new(PatchApplyEndEvent {
                call_id: ctx.call_id.to_string(),
                stdout,
                stderr,
                success,
                report,
            })),
        )
        .await;

    if let Some(tracker) = ctx.turn_diff_tracker {
        let unified_diff = {
            let mut guard = tracker.lock().await;
            guard.get_unified_diff()
        };
        if let Ok(Some(unified_diff)) = unified_diff {
            ctx.session
                .send_event(ctx.turn, EventMsg::TurnDiff(TurnDiffEvent { unified_diff }))
                .await;
        }
    }
}

#[derive(Deserialize)]
struct ApplyPatchMachineEnvelope {
    schema: String,
    report: ApplyPatchReport,
}

fn extract_apply_patch_report(stdout: &str) -> Option<ApplyPatchReport> {
    stdout
        .lines()
        .rev()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<ApplyPatchMachineEnvelope>(trimmed).ok()
        })
        .find_map(|env| {
            if env.schema == APPLY_PATCH_MACHINE_SCHEMA {
                Some(env.report)
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ApplyPatchReportStatus;

    #[test]
    fn extract_report_returns_struct_when_schema_matches() {
        let stdout = r#"
Applied operations:
- add: foo (+1)
{"schema":"apply_patch/v2","report":{"status":"success","mode":"apply","duration_ms":1,"operations":[],"errors":[],"options":{"encoding":"utf-8","newline":"lf","strip_trailing_whitespace":false,"ensure_final_newline":null,"preserve_mode":true,"preserve_times":true,"new_file_mode":null,"symbol_fallback_mode":"fuzzy"},"formatting":[],"post_checks":[],"diagnostics":[],"batch":null,"artifacts":{"log":null,"conflicts":[],"unapplied":[]},"amendment_template":null}}
"#;

        let report = extract_apply_patch_report(stdout).expect("report");
        assert_eq!(report.status, ApplyPatchReportStatus::Success);
    }

    #[test]
    fn extract_report_ignores_non_matching_schema() {
        let stdout = r#"{"schema":"apply_patch/v1","report":{"status":"success","mode":"apply","duration_ms":0,"operations":[],"errors":[],"options":{"encoding":"utf-8","newline":"lf","strip_trailing_whitespace":false,"ensure_final_newline":null,"preserve_mode":true,"preserve_times":true,"new_file_mode":null,"symbol_fallback_mode":"fuzzy"},"formatting":[],"post_checks":[],"diagnostics":[],"batch":null,"artifacts":{"log":null,"conflicts":[],"unapplied":[]},"amendment_template":null}}"#;
        assert!(extract_apply_patch_report(stdout).is_none());
    }
}
