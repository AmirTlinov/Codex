use std::path::Path;
use std::sync::Arc;

use codex_api::common::ClaudeCodeControlResponder;
use codex_api::common::ClaudeCodeControlResponseSubtype;
use codex_api::common::ClaudeCodePermissionRequest;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsArgs;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;

use crate::codex::ExternalCommandApprovalRequest;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::protocol::AskForApproval;
use crate::protocol::ReviewDecision;

pub(crate) struct ClaudeCodePermissionResolution {
    pub(crate) response: ClaudeCodeControlResponseSubtype,
    pub(crate) interrupt_turn: bool,
}

pub(crate) enum ControlRequestParseOutcome {
    NotControlRequest,
    Supported(ClaudeCodePermissionRequest),
}

pub(crate) fn parse_control_request_line(
    line: &str,
    control_responder: &ClaudeCodeControlResponder,
) -> std::result::Result<ControlRequestParseOutcome, String> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|err| format!("parse Claude Code control_request: {err}"))?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("control_request") {
        return Ok(ControlRequestParseOutcome::NotControlRequest);
    }
    let request = value
        .get("request")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            "Claude Code carrier emitted malformed control_request payload".to_string()
        })?;
    let subtype = request
        .get("subtype")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "Claude Code carrier emitted malformed control_request subtype".to_string()
        })?;
    if subtype != "can_use_tool" {
        return Err(
            "Claude Code carrier emitted an unsupported control_request subtype".to_string(),
        );
    }
    let input = request
        .get("input")
        .cloned()
        .ok_or_else(|| "Claude Code carrier emitted malformed can_use_tool input".to_string())?;
    Ok(ControlRequestParseOutcome::Supported(
        ClaudeCodePermissionRequest::new(
            value
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    "Claude Code carrier emitted malformed can_use_tool request_id".to_string()
                })?
                .to_string(),
            request
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    "Claude Code carrier emitted malformed can_use_tool tool_name".to_string()
                })?
                .to_string(),
            input,
            request
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    "Claude Code carrier emitted malformed can_use_tool tool_use_id".to_string()
                })?
                .to_string(),
            request
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            request
                .get("decision_reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            control_responder.clone(),
        ),
    ))
}

pub(crate) async fn resolve_claude_code_permission_request(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    request: &ClaudeCodePermissionRequest,
) -> ClaudeCodePermissionResolution {
    if approval_policy_auto_allows_claude_control(turn_context.approval_policy.value()) {
        return allowed_resolution();
    }
    if is_bash_tool(&request.tool_name) {
        return resolve_bash_permission_request(sess, turn_context, request).await;
    }

    let Some(permissions) = requested_permissions_for_tool(
        &request.tool_name,
        &request.input,
        turn_context.cwd.as_path(),
    ) else {
        return denied_resolution(format!(
            "Claudex does not yet know how to approve Claude Code tool `{}` with these inputs.",
            request.tool_name
        ));
    };

    let response = sess
        .request_permissions(
            turn_context.as_ref(),
            request.request_id.clone(),
            RequestPermissionsArgs {
                reason: Some(permission_reason(request)),
                permissions,
            },
        )
        .await;

    match response {
        Some(response) if !response.permissions.is_empty() => allowed_resolution(),
        _ => denied_resolution(format!(
            "Permission denied for Claude Code tool `{}`",
            request.tool_name
        )),
    }
}

pub(crate) async fn resolve_external_claude_code_permission_request(
    sess: &Arc<Session>,
    approval_policy: AskForApproval,
    turn_id: &str,
    cwd: &Path,
    request: &ClaudeCodePermissionRequest,
) -> ClaudeCodePermissionResolution {
    if approval_policy_auto_allows_claude_control(approval_policy) {
        return allowed_resolution();
    }
    if is_bash_tool(&request.tool_name) {
        return resolve_external_bash_permission_request(
            sess,
            approval_policy,
            turn_id,
            cwd,
            request,
        )
        .await;
    }

    let Some(permissions) = requested_permissions_for_tool(&request.tool_name, &request.input, cwd)
    else {
        return denied_resolution(format!(
            "Claudex does not yet know how to approve Claude Code tool `{}` with these inputs.",
            request.tool_name
        ));
    };

    let response = sess
        .request_external_permissions(
            approval_policy,
            turn_id.to_string(),
            request.request_id.clone(),
            RequestPermissionsArgs {
                reason: Some(permission_reason(request)),
                permissions,
            },
        )
        .await;

    match response {
        Some(response) if !response.permissions.is_empty() => allowed_resolution(),
        _ => denied_resolution(format!(
            "Permission denied for Claude Code tool `{}`",
            request.tool_name
        )),
    }
}

fn is_bash_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Bash" | "BashTool")
}

fn approval_policy_auto_allows_claude_control(approval_policy: AskForApproval) -> bool {
    matches!(approval_policy, AskForApproval::Never)
}

async fn resolve_bash_permission_request(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    request: &ClaudeCodePermissionRequest,
) -> ClaudeCodePermissionResolution {
    let Some(command) = request
        .input
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return denied_resolution(
            "Claude Code Bash request did not include a command.".to_string(),
        );
    };

    let decision = sess
        .request_command_approval(
            turn_context.as_ref(),
            request.tool_use_id.clone(),
            Some(request.request_id.clone()),
            vec![command],
            turn_context.cwd.to_path_buf(),
            Some(permission_reason(request)),
            /*network_approval_context*/ None,
            /*proposed_execpolicy_amendment*/ None,
            /*additional_permissions*/ None,
            /*available_decisions*/ None,
        )
        .await;

    review_decision_to_control_response(decision, &request.tool_name)
}

async fn resolve_external_bash_permission_request(
    sess: &Arc<Session>,
    approval_policy: AskForApproval,
    turn_id: &str,
    cwd: &Path,
    request: &ClaudeCodePermissionRequest,
) -> ClaudeCodePermissionResolution {
    let Some(command) = request
        .input
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return denied_resolution(
            "Claude Code Bash request did not include a command.".to_string(),
        );
    };

    let decision = sess
        .request_external_command_approval(ExternalCommandApprovalRequest {
            approval_policy,
            turn_id: turn_id.to_string(),
            call_id: request.tool_use_id.clone(),
            approval_id: Some(request.request_id.clone()),
            command: vec![command],
            cwd: cwd.to_path_buf(),
            reason: Some(permission_reason(request)),
        })
        .await;

    review_decision_to_control_response(decision, &request.tool_name)
}

fn permission_reason(request: &ClaudeCodePermissionRequest) -> String {
    request
        .description
        .clone()
        .or_else(|| request.decision_reason.clone())
        .unwrap_or_else(|| {
            format!(
                "Claude Code requests permission to use `{}`.",
                request.tool_name
            )
        })
}

fn review_decision_to_control_response(
    decision: ReviewDecision,
    tool_name: &str,
) -> ClaudeCodePermissionResolution {
    match decision {
        ReviewDecision::Approved
        | ReviewDecision::ApprovedExecpolicyAmendment { .. }
        | ReviewDecision::ApprovedForSession
        | ReviewDecision::NetworkPolicyAmendment { .. } => allowed_resolution(),
        ReviewDecision::Denied => denied_resolution(format!(
            "Permission denied for Claude Code tool `{tool_name}`"
        )),
        ReviewDecision::Abort => ClaudeCodePermissionResolution {
            response: ClaudeCodeControlResponseSubtype::Deny {
                message: format!("Claude Code tool `{tool_name}` was aborted by the user."),
            },
            interrupt_turn: true,
        },
    }
}

fn allowed_resolution() -> ClaudeCodePermissionResolution {
    ClaudeCodePermissionResolution {
        response: ClaudeCodeControlResponseSubtype::Allow {
            updated_input: None,
        },
        interrupt_turn: false,
    }
}

fn denied_resolution(message: String) -> ClaudeCodePermissionResolution {
    ClaudeCodePermissionResolution {
        response: ClaudeCodeControlResponseSubtype::Deny { message },
        interrupt_turn: false,
    }
}

fn requested_permissions_for_tool(
    tool_name: &str,
    input: &Value,
    cwd: &Path,
) -> Option<RequestPermissionProfile> {
    match tool_name {
        "Read" | "FileReadTool" | "NotebookRead" | "NotebookReadTool" | "Glob" | "GlobTool"
        | "Grep" | "GrepTool" | "LSP" | "LS" | "ListDir" => {
            let paths = resolve_paths(input, cwd);
            (!paths.is_empty()).then_some(RequestPermissionProfile {
                file_system: Some(codex_protocol::models::FileSystemPermissions {
                    read: Some(paths),
                    write: None,
                }),
                ..RequestPermissionProfile::default()
            })
        }
        "Write" | "Edit" | "MultiEdit" | "FileWriteTool" | "FileEditTool" | "NotebookEdit"
        | "NotebookEditTool" => {
            let paths = resolve_paths(input, cwd);
            (!paths.is_empty()).then_some(RequestPermissionProfile {
                file_system: Some(codex_protocol::models::FileSystemPermissions {
                    read: None,
                    write: Some(paths),
                }),
                ..RequestPermissionProfile::default()
            })
        }
        "WebFetch" | "WebFetchTool" | "WebSearch" | "WebSearchTool" => {
            Some(RequestPermissionProfile {
                network: Some(codex_protocol::models::NetworkPermissions {
                    enabled: Some(true),
                }),
                ..RequestPermissionProfile::default()
            })
        }
        _ => None,
    }
}

fn resolve_paths(input: &Value, cwd: &Path) -> Vec<AbsolutePathBuf> {
    let mut raw_paths = Vec::new();
    collect_string_field(input, "file_path", &mut raw_paths);
    collect_string_field(input, "path", &mut raw_paths);
    collect_string_field(input, "notebook_path", &mut raw_paths);
    collect_string_field(input, "directory", &mut raw_paths);
    collect_string_array_field(input, "paths", &mut raw_paths);

    raw_paths
        .into_iter()
        .filter_map(|path| AbsolutePathBuf::resolve_path_against_base(path, cwd).ok())
        .collect()
}

fn collect_string_field<'a>(input: &'a Value, field: &str, out: &mut Vec<&'a str>) {
    if let Some(value) = input.get(field).and_then(Value::as_str) {
        out.push(value);
    }
}

fn collect_string_array_field<'a>(input: &'a Value, field: &str, out: &mut Vec<&'a str>) {
    let Some(values) = input.get(field).and_then(Value::as_array) else {
        return;
    };
    out.extend(values.iter().filter_map(Value::as_str));
}

#[cfg(test)]
mod tests;
