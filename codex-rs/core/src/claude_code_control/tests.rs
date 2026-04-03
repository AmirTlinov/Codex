use std::sync::Arc;
use std::time::Duration as StdDuration;

use super::resolve_claude_code_permission_request;
use crate::codex::make_session_and_context_with_rx;
use crate::protocol::AskForApproval;
use crate::protocol::GranularApprovalConfig;
use crate::protocol::ReviewDecision;
use crate::state::ActiveTurn;
use codex_api::common::ClaudeCodeControlResponseSubtype;
use codex_api::common::ClaudeCodePermissionRequest;
use codex_protocol::protocol::EventMsg;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use serde_json::json;

#[tokio::test]
async fn bash_permission_requests_route_through_exec_approval() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let request = ClaudeCodePermissionRequest::new(
        "approval-1".to_string(),
        "Bash".to_string(),
        json!({ "command": "git status" }),
        "tool-1".to_string(),
        Some("Claude Code wants to run git status".to_string()),
        /*decision_reason*/ None,
        codex_api::common::ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
    );

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        async move { resolve_claude_code_permission_request(&session, &turn_context, &request).await }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("exec approval request timed out")
        .expect("exec approval event missing");
    let EventMsg::ExecApprovalRequest(event) = request_event.msg else {
        panic!("expected exec approval request event");
    };
    assert_eq!(event.call_id, "tool-1");
    assert_eq!(event.approval_id.as_deref(), Some("approval-1"));

    session
        .notify_approval(&event.effective_approval_id(), ReviewDecision::Approved)
        .await;

    let outcome = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("resolve future timed out")
        .expect("resolve join error");
    assert!(matches!(
        outcome.response,
        ClaudeCodeControlResponseSubtype::Allow {
            updated_input: None
        }
    ));
    assert!(!outcome.interrupt_turn);
}

#[tokio::test]
async fn read_permission_requests_route_through_request_permissions() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let request = ClaudeCodePermissionRequest::new(
        "permission-1".to_string(),
        "Read".to_string(),
        json!({ "file_path": "AGENTS.md" }),
        "tool-2".to_string(),
        Some("Claude Code wants to read AGENTS.md".to_string()),
        /*decision_reason*/ None,
        codex_api::common::ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
    );

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        async move { resolve_claude_code_permission_request(&session, &turn_context, &request).await }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("request_permissions event timed out")
        .expect("request_permissions event missing");
    let EventMsg::RequestPermissions(event) = request_event.msg else {
        panic!("expected request_permissions event");
    };
    assert_eq!(event.call_id, "permission-1");

    session
        .notify_request_permissions_response(
            &event.call_id,
            RequestPermissionsResponse {
                permissions: event.permissions.clone(),
                scope: PermissionGrantScope::Turn,
            },
        )
        .await;

    let outcome = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("resolve future timed out")
        .expect("resolve join error");
    assert!(matches!(
        outcome.response,
        ClaudeCodeControlResponseSubtype::Allow {
            updated_input: None
        }
    ));
    assert!(!outcome.interrupt_turn);
    let RequestPermissionProfile { file_system, .. } = event.permissions;
    assert!(file_system.is_some());
}
