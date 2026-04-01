use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;

struct MockClaudeScript {
    script_path: std::path::PathBuf,
    args_log_path: std::path::PathBuf,
    stdin_log_path: std::path::PathBuf,
}

async fn wait_for_final_status(
    registry: &ExternalAgentRegistry,
    thread_id: ThreadId,
) -> AgentStatus {
    let mut status_rx = registry
        .subscribe_status(thread_id)
        .await
        .expect("subscribe to external agent status");
    loop {
        let status = status_rx.borrow().clone();
        if matches!(
            status,
            AgentStatus::Completed(_) | AgentStatus::Errored(_) | AgentStatus::Shutdown
        ) {
            return status;
        }
        status_rx.changed().await.expect("status change");
    }
}

async fn wait_for_status_change(
    registry: &ExternalAgentRegistry,
    thread_id: ThreadId,
    previous_status: &AgentStatus,
) -> AgentStatus {
    for _ in 0..100 {
        let status = registry
            .get_status(thread_id)
            .await
            .expect("get external agent status");
        if &status != previous_status {
            return status;
        }
        tokio::task::yield_now().await;
    }
    panic!("external agent status did not change")
}

fn text_input(text: &str) -> Op {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
    .into()
}

fn test_snapshot(cwd: &std::path::Path, model: &str) -> ThreadConfigSnapshot {
    ThreadConfigSnapshot {
        model: model.to_string(),
        model_provider_id: "claude_cli".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        sandbox_policy: SandboxPolicy::new_read_only_policy(),
        cwd: cwd.to_path_buf(),
        ephemeral: true,
        reasoning_effort: None,
        personality: None,
        session_source: SessionSource::Cli,
    }
}

fn mock_claude_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-claude.sh");
    let args_log_path = root.path().join("invocations.args.log");
    let stdin_log_path = root.path().join("invocations.stdin.log");
    let count_path = root.path().join("invocation.count");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nstdin_payload=$(cat)\nprintf '%s\\n--\\n' \"$@\" >> '{}'\nprintf '%s\\n--\\n' \"$stdin_payload\" >> '{}'\ncount=0\nif [[ -f '{}' ]]; then\n  count=$(cat '{}')\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nprintf 'run-%s' \"$count\"\n",
            args_log_path.display(),
            stdin_log_path.display(),
            count_path.display(),
            count_path.display(),
            count_path.display(),
        ),
    )
    .expect("write mock claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod mock claude");
    }
    MockClaudeScript {
        script_path,
        args_log_path,
        stdin_log_path,
    }
}

#[tokio::test]
async fn external_agent_registry_runs_claude_cli_turns() {
    let root = TempDir::new().expect("create temp dir");
    let mock_script = mock_claude_script(&root);
    let registry = ExternalAgentRegistry::default();
    let thread_id = ThreadId::new();

    let submission_id = registry
        .spawn_agent(
            thread_id,
            ExternalAgentLaunchRequest {
                config_snapshot: test_snapshot(root.path(), "claude-opus-4-6"),
                developer_instructions: Some("Follow repo truth".to_string()),
                claude_cli: ClaudeCliConfig {
                    path: Some(mock_script.script_path.clone()),
                    ..Default::default()
                },
                model: "claude-opus-4-6".to_string(),
                parent_context: Some("[1] user: parent context".to_string()),
            },
            text_input("first"),
        )
        .await
        .expect("spawn external agent");
    assert!(!submission_id.is_empty());

    let status = wait_for_final_status(&registry, thread_id).await;
    assert_eq!(status, AgentStatus::Completed(Some("run-1".to_string())));

    registry
        .send_input(thread_id, text_input("second"))
        .await
        .expect("send input to external agent");
    let status = wait_for_status_change(
        &registry,
        thread_id,
        &AgentStatus::Completed(Some("run-1".to_string())),
    )
    .await;
    let status = if matches!(status, AgentStatus::Completed(_)) {
        status
    } else {
        wait_for_final_status(&registry, thread_id).await
    };
    assert_eq!(status, AgentStatus::Completed(Some("run-2".to_string())));

    let args_log = std::fs::read_to_string(&mock_script.args_log_path).expect("read args log");
    let stdin_log = std::fs::read_to_string(&mock_script.stdin_log_path).expect("read stdin log");
    assert!(args_log.contains("--print"));
    assert!(args_log.contains("--model"));
    assert!(!args_log.contains("first"));
    assert!(!args_log.contains("second"));
    assert!(!args_log.contains("Follow repo truth"));
    assert!(stdin_log.contains("first"));
    assert!(stdin_log.contains("second"));
    assert!(stdin_log.contains("parent context"));
    assert!(stdin_log.contains("Follow repo truth"));

    let snapshot = registry
        .get_config_snapshot(thread_id)
        .await
        .expect("external snapshot");
    assert_eq!(snapshot.model, "claude-opus-4-6");
    assert_eq!(snapshot.model_provider_id, "claude_cli");

    registry
        .close_agent(thread_id)
        .await
        .expect("close external agent");
    assert_eq!(registry.get_status(thread_id).await, None);
}

#[test]
fn default_claude_model_prefers_claude_names_and_falls_back_to_opus_46() {
    assert_eq!(
        default_claude_model(Some("claude-sonnet-4-6")),
        "claude-sonnet-4-6"
    );
    assert_eq!(default_claude_model(Some("gpt-5.4")), "claude-opus-4-6");
    assert_eq!(default_claude_model(/*model*/ None), "claude-opus-4-6");
}

#[test]
fn external_agent_user_prompt_bounds_old_conversation_entries() {
    let conversation = (1..=16)
        .map(|index| ConversationEntry {
            role: if index % 2 == 0 {
                ConversationRole::Assistant
            } else {
                ConversationRole::User
            },
            text: format!("turn-{index:02}"),
        })
        .collect::<Vec<_>>();

    let prompt = build_external_agent_user_prompt(
        Some("Follow repo truth"),
        Some("parent context"),
        &conversation,
        "latest task",
    );

    assert!(prompt.contains("turn-16"));
    assert!(!prompt.contains("turn-01"));
    assert!(prompt.contains("<subagent_conversation_omission>"));
    assert!(prompt.contains("latest task"));
}
