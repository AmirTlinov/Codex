use std::path::Path;
use std::sync::Arc;

use codex_api::common::ClaudeCodeControlResponseSubtype;
use codex_api::common::ClaudeCodePermissionRequest;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsArgs;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::protocol::ReviewDecision;

pub(crate) struct ClaudeCodePermissionResolution {
    pub(crate) response: ClaudeCodeControlResponseSubtype,
    pub(crate) interrupt_turn: bool,
}

pub(crate) async fn resolve_claude_code_permission_request(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    request: &ClaudeCodePermissionRequest,
) -> ClaudeCodePermissionResolution {
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

fn is_bash_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Bash" | "BashTool")
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
        "Read" | "Glob" | "Grep" => {
            let paths = resolve_paths(input, cwd);
            (!paths.is_empty()).then_some(RequestPermissionProfile {
                file_system: Some(codex_protocol::models::FileSystemPermissions {
                    read: Some(paths),
                    write: None,
                }),
                ..RequestPermissionProfile::default()
            })
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let paths = resolve_paths(input, cwd);
            (!paths.is_empty()).then_some(RequestPermissionProfile {
                file_system: Some(codex_protocol::models::FileSystemPermissions {
                    read: None,
                    write: Some(paths),
                }),
                ..RequestPermissionProfile::default()
            })
        }
        "WebFetch" | "WebSearch" => Some(RequestPermissionProfile {
            network: Some(codex_protocol::models::NetworkPermissions {
                enabled: Some(true),
            }),
            ..RequestPermissionProfile::default()
        }),
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
