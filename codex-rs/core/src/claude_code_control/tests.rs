use std::sync::Arc;
use std::time::Duration as StdDuration;

use super::ClaudeCodeToolClass;
use super::ClaudeControlRequest;
use super::ControlRequestParseOutcome;
use super::classify_tool_name;
use super::parse_control_request_line;
use super::resolve_claude_code_permission_request;
use crate::codex::make_session_and_context_with_rx;
use crate::protocol::AskForApproval;
use crate::protocol::GranularApprovalConfig;
use crate::protocol::ReviewDecision;
use crate::state::ActiveTurn;
use codex_api::common::ClaudeCodeControlResponder;
use codex_api::common::ClaudeCodeControlResponseSubtype;
use codex_api::common::ClaudeCodePermissionRequest;
use codex_protocol::protocol::EventMsg;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn parse_non_control_request_returns_not_control_request() {
    let responder = ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0);
    let outcome = parse_control_request_line(
        r#"{"type":"assistant","message":{"role":"assistant"}}"#,
        &responder,
    )
    .expect("assistant line should parse");
    assert!(matches!(
        outcome,
        ControlRequestParseOutcome::NotControlRequest
    ));
}

#[test]
fn parse_can_use_tool_request_returns_typed_request() {
    let responder = ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0);
    let outcome = parse_control_request_line(
        r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"AGENTS.md"},"tool_use_id":"tool-1","description":"Read AGENTS.md"}}"#,
        &responder,
    )
    .expect("valid can_use_tool should parse");

    let ControlRequestParseOutcome::ControlRequest(ClaudeControlRequest::CanUseTool(request)) =
        outcome
    else {
        panic!("expected typed can_use_tool request")
    };

    assert_eq!(request.request_id, "req-1");
    assert_eq!(request.tool_name, "Read");
    assert_eq!(request.tool_use_id, "tool-1");
    assert_eq!(request.description.as_deref(), Some("Read AGENTS.md"));
    assert_eq!(request.input, json!({ "file_path": "AGENTS.md" }));
}

#[test]
fn parse_unsupported_control_request_subtype_returns_typed_subtype() {
    let responder = ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0);
    let outcome = parse_control_request_line(
        r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"interrupt"}}"#,
        &responder,
    )
    .expect("unsupported subtype should still parse structurally");

    assert!(matches!(
        outcome,
        ControlRequestParseOutcome::ControlRequest(ClaudeControlRequest::UnsupportedSubtype {
            subtype
        }) if subtype == "interrupt"
    ));
}

#[test]
fn parse_malformed_can_use_tool_request_reports_precise_field() {
    let responder = ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0);
    let err = parse_control_request_line(
        r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","input":{"file_path":"AGENTS.md"},"tool_use_id":"tool-1"}}"#,
        &responder,
    )
    .expect_err("missing tool_name should fail");
    assert!(err.contains("malformed can_use_tool tool_name"), "{err}");
}

#[test]
fn classify_tool_name_covers_supported_aliases() {
    let cases = [
        ("Bash", ClaudeCodeToolClass::Command),
        ("BashTool", ClaudeCodeToolClass::Command),
        ("Read", ClaudeCodeToolClass::ReadFs),
        ("FileReadTool", ClaudeCodeToolClass::ReadFs),
        ("NotebookRead", ClaudeCodeToolClass::ReadFs),
        ("NotebookReadTool", ClaudeCodeToolClass::ReadFs),
        ("NotebookReadCell", ClaudeCodeToolClass::ReadFs),
        ("NotebookReadCellTool", ClaudeCodeToolClass::ReadFs),
        ("Glob", ClaudeCodeToolClass::ReadFs),
        ("GlobTool", ClaudeCodeToolClass::ReadFs),
        ("Grep", ClaudeCodeToolClass::ReadFs),
        ("GrepTool", ClaudeCodeToolClass::ReadFs),
        ("LSP", ClaudeCodeToolClass::ReadFs),
        ("LS", ClaudeCodeToolClass::ReadFs),
        ("ListDir", ClaudeCodeToolClass::ReadFs),
        ("Write", ClaudeCodeToolClass::WriteFs),
        ("Edit", ClaudeCodeToolClass::WriteFs),
        ("MultiEdit", ClaudeCodeToolClass::WriteFs),
        ("FileWriteTool", ClaudeCodeToolClass::WriteFs),
        ("FileEditTool", ClaudeCodeToolClass::WriteFs),
        ("NotebookEdit", ClaudeCodeToolClass::WriteFs),
        ("NotebookEditTool", ClaudeCodeToolClass::WriteFs),
        ("NotebookEditCell", ClaudeCodeToolClass::WriteFs),
        ("NotebookEditCellTool", ClaudeCodeToolClass::WriteFs),
        ("WebFetch", ClaudeCodeToolClass::Network),
        ("WebFetchTool", ClaudeCodeToolClass::Network),
        ("WebSearch", ClaudeCodeToolClass::Network),
        ("WebSearchTool", ClaudeCodeToolClass::Network),
        ("FutureTool", ClaudeCodeToolClass::Unknown),
    ];

    for (tool_name, expected_class) in cases {
        assert_eq!(classify_tool_name(tool_name), expected_class, "{tool_name}");
    }
}

#[tokio::test]
async fn unknown_tool_is_denied_explicitly_without_prompting() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::OnRequest)
        .expect("test setup should allow updating approval policy");

    let outcome = resolve_claude_code_permission_request(
        &session,
        &turn_context,
        &ClaudeCodePermissionRequest::new(
            "permission-unknown".to_string(),
            "FutureTool".to_string(),
            json!({}),
            "tool-unknown".to_string(),
            Some("Future drifted tool".to_string()),
            /*decision_reason*/ None,
            ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
        ),
    )
    .await;

    assert!(matches!(
        outcome.response,
        ClaudeCodeControlResponseSubtype::Deny { ref message }
            if message.contains("FutureTool")
    ));
    assert!(!outcome.interrupt_turn);
    assert!(
        tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "unknown tools should fail closed before prompting"
    );
}

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
        ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
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
        ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
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

#[tokio::test]
async fn approval_policy_never_auto_allows_claude_permission_requests() {
    let (session, mut turn_context, _rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::Never)
        .expect("test setup should allow updating approval policy");

    let outcome = resolve_claude_code_permission_request(
        &session,
        &turn_context,
        &ClaudeCodePermissionRequest::new(
            "permission-never".to_string(),
            "TodoWrite".to_string(),
            json!({}),
            "tool-never".to_string(),
            Some("bypassPermissions should not block tool execution".to_string()),
            /*decision_reason*/ None,
            ClaudeCodeControlResponder::new(tokio::sync::mpsc::channel(1).0),
        ),
    )
    .await;

    assert!(matches!(
        outcome.response,
        ClaudeCodeControlResponseSubtype::Allow {
            updated_input: None
        }
    ));
    assert!(!outcome.interrupt_turn);
}
