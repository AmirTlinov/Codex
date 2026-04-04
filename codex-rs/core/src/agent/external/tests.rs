use super::*;
use crate::agent::external::claude_cli::run_claude_code_turn;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

use crate::CodexAuth;
use crate::CodexThread;
use crate::ThreadManager;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use std::sync::Arc;

struct MockClaudeScript {
    script_path: std::path::PathBuf,
    args_log_path: std::path::PathBuf,
    stdin_log_path: std::path::PathBuf,
}

const MOCK_SESSION_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

struct LoggedInvocations {
    args: Vec<String>,
    stdin: Vec<String>,
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
        model_provider_id: "claude_code".to_string(),
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

async fn host_thread(root: &TempDir, model: &str) -> Arc<CodexThread> {
    let mut config = crate::config::ConfigBuilder::default()
        .codex_home(root.path().to_path_buf())
        .build()
        .await
        .expect("build host-thread config");
    config.model = Some(model.to_string());
    config.model_provider = crate::create_claude_code_provider();
    config.model_provider_id = crate::CLAUDE_CODE_PROVIDER_ID.to_string();
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    manager
        .start_thread(config)
        .await
        .expect("start host thread")
        .thread
}

fn mock_claude_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-claude.sh");
    let args_log_path = root.path().join("invocations.args.log");
    let stdin_log_path = root.path().join("invocations.stdin.log");
    let count_path = root.path().join("invocation.count");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r stdin_payload\nprintf '%s\\n' \"$@\" >> '{}'\nprintf -- '--\\n' >> '{}'\nprintf '%s\\n--\\n' \"$stdin_payload\" >> '{}'\ncount=0\nif [[ -f '{}' ]]; then\n  count=$(cat '{}')\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nsession_id='123e4567-e89b-12d3-a456-426614174000'\nfor ((i=1; i<=$#; i++)); do\n  if [[ \"${{!i}}\" == '--session-id' ]]; then\n    echo 'unexpected --session-id' >&2\n    exit 99\n  fi\n  if [[ \"${{!i}}\" == '--resume' ]]; then\n    next=$((i + 1))\n    session_id=\"${{!next}}\"\n  fi\ndone\ncat <<EOF\n{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session_id\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-$count\"}}\n{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-6\",\"id\":\"msg-$count\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"run-$count\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}},\"context_management\":null}},\"parent_tool_use_id\":null,\"session_id\":\"$session_id\",\"uuid\":\"assistant-$count\"}}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"run-$count\",\"stop_reason\":\"end_turn\",\"session_id\":\"$session_id\",\"total_cost_usd\":0.0,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2}},\"modelUsage\":{{}},\"permission_denials\":[],\"uuid\":\"result-$count\"}}\nEOF\n",
            args_log_path.display(),
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

fn mock_interruptible_claude_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-claude-interruptible.sh");
    let args_log_path = root.path().join("interruptible.args.log");
    let stdin_log_path = root.path().join("interruptible.stdin.log");
    let count_path = root.path().join("interruptible.count");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r stdin_payload\nprintf '%s\\n' \"$@\" >> '{}'\nprintf -- '--\\n' >> '{}'\nprintf '%s\\n--\\n' \"$stdin_payload\" >> '{}'\ncount=0\nif [[ -f '{}' ]]; then\n  count=$(cat '{}')\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nsession_id='123e4567-e89b-12d3-a456-426614174000'\nfor ((i=1; i<=$#; i++)); do\n  if [[ \"${{!i}}\" == '--session-id' ]]; then\n    echo 'unexpected --session-id' >&2\n    exit 99\n  fi\n  if [[ \"${{!i}}\" == '--resume' ]]; then\n    next=$((i + 1))\n    session_id=\"${{!next}}\"\n  fi\ndone\nif [[ \"$count\" == '2' ]]; then\n  sleep 30\nfi\ncat <<EOF\n{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session_id\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-$count\"}}\n{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-6\",\"id\":\"msg-$count\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"run-$count\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}},\"context_management\":null}},\"parent_tool_use_id\":null,\"session_id\":\"$session_id\",\"uuid\":\"assistant-$count\"}}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"run-$count\",\"stop_reason\":\"end_turn\",\"session_id\":\"$session_id\",\"total_cost_usd\":0.0,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2}},\"modelUsage\":{{}},\"permission_denials\":[],\"uuid\":\"result-$count\"}}\nEOF\n",
            args_log_path.display(),
            args_log_path.display(),
            stdin_log_path.display(),
            count_path.display(),
            count_path.display(),
            count_path.display(),
        ),
    )
    .expect("write interruptible mock claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("interruptible mock claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod interruptible mock claude");
    }
    MockClaudeScript {
        script_path,
        args_log_path,
        stdin_log_path,
    }
}

fn mock_claude_waits_for_stdin_shutdown_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-claude-waits-for-stdin-shutdown.sh");
    let args_log_path = root.path().join("stdin-shutdown.args.log");
    let stdin_log_path = root.path().join("stdin-shutdown.stdin.log");
    std::fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
IFS= read -r stdin_payload
printf '%s
' "$@" >> '{}'
printf -- '--
' >> '{}'
printf '%s
--
' "$stdin_payload" >> '{}'
session_id='123e4567-e89b-12d3-a456-426614174001'
cat <<EOF
{{"type":"system","subtype":"init","session_id":"$session_id","tools":[],"mcp_servers":[],"model":"claude-opus-4-6","permissionMode":"bypassPermissions","slash_commands":[],"apiKeySource":"none","claude_code_version":"test","output_style":"default","agents":[],"skills":[],"plugins":[],"uuid":"init-1"}}
{{"type":"assistant","message":{{"model":"claude-opus-4-6","id":"msg-1","type":"message","role":"assistant","content":[{{"type":"text","text":"run-1"}}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2,"service_tier":"standard","inference_geo":"not_available"}},"context_management":null}},"parent_tool_use_id":null,"session_id":"$session_id","uuid":"assistant-1"}}
{{"type":"result","subtype":"success","is_error":false,"duration_ms":10,"duration_api_ms":10,"num_turns":1,"result":"run-1","stop_reason":"end_turn","session_id":"$session_id","total_cost_usd":0.0,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2}},"modelUsage":{{}},"permission_denials":[],"uuid":"result-1"}}
EOF
while IFS= read -r trailing_line; do
  printf '%s
--
' "$trailing_line" >> '{}'
done
"#,
            args_log_path.display(),
            args_log_path.display(),
            stdin_log_path.display(),
            stdin_log_path.display(),
        ),
    )
    .expect("write stdin-shutdown mock claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("stdin-shutdown mock claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod stdin-shutdown mock claude");
    }
    MockClaudeScript {
        script_path,
        args_log_path,
        stdin_log_path,
    }
}

fn read_logged_invocations(mock_script: &MockClaudeScript) -> LoggedInvocations {
    let args_log = std::fs::read_to_string(&mock_script.args_log_path).expect("read args log");
    let stdin_log = std::fs::read_to_string(&mock_script.stdin_log_path).expect("read stdin log");
    LoggedInvocations {
        args: split_log_entries(args_log),
        stdin: split_log_entries(stdin_log),
    }
}

fn split_log_entries(log: String) -> Vec<String> {
    log.split("\n--\n")
        .filter(|entry| !entry.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn external_agent_registry_closes_claude_stdin_after_terminal_result() {
    let root = TempDir::new().expect("create temp dir");
    let mock_script = mock_claude_waits_for_stdin_shutdown_script(&root);
    let registry = ExternalAgentRegistry::default();
    let thread_id = ThreadId::new();

    registry
        .spawn_agent(
            thread_id,
            ExternalAgentLaunchRequest {
                host_thread: host_thread(&root, "claude-opus-4-6").await,
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

    let status = timeout(
        Duration::from_secs(5),
        wait_for_final_status(&registry, thread_id),
    )
    .await
    .expect("external Claude agent should finish after terminal result");
    assert_eq!(status, AgentStatus::Completed(Some("run-1".to_string())));
}

#[tokio::test]
async fn external_agent_registry_runs_claude_code_carrier_turns() {
    let root = TempDir::new().expect("create temp dir");
    let mock_script = mock_claude_script(&root);
    let registry = ExternalAgentRegistry::default();
    let thread_id = ThreadId::new();

    let submission_id = registry
        .spawn_agent(
            thread_id,
            ExternalAgentLaunchRequest {
                host_thread: host_thread(&root, "claude-opus-4-6").await,
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

    let invocations = read_logged_invocations(&mock_script);
    let args_log = invocations.args.join("\n");
    assert!(args_log.contains("--print"));
    assert!(args_log.contains("--model"));
    assert!(args_log.contains("stream-json"));
    assert!(args_log.contains("--resume\n123e4567-e89b-12d3-a456-426614174000"));
    assert!(!args_log.contains("first"));
    assert!(!args_log.contains("second"));
    assert!(!args_log.contains("Follow repo truth"));
    let stdin_entries = invocations.stdin;
    assert_eq!(stdin_entries.len(), 2);
    assert!(stdin_entries[0].contains("first"));
    assert!(stdin_entries[0].contains("parent context"));
    assert!(stdin_entries[0].contains("Follow repo truth"));
    assert!(stdin_entries[1].contains("second"));
    assert!(!stdin_entries[1].contains("parent context"));
    assert!(!stdin_entries[1].contains("Follow repo truth"));

    let snapshot = registry
        .get_config_snapshot(thread_id)
        .await
        .expect("external snapshot");
    assert_eq!(snapshot.model, "claude-opus-4-6");
    assert_eq!(snapshot.model_provider_id, "claude_code");

    registry
        .close_agent(thread_id)
        .await
        .expect("close external agent");
    assert_eq!(registry.get_status(thread_id).await, None);
}

#[tokio::test]
async fn interrupt_keeps_last_known_good_claude_session() {
    let root = TempDir::new().expect("create temp dir");
    let mock_script = mock_interruptible_claude_script(&root);
    let registry = ExternalAgentRegistry::default();
    let thread_id = ThreadId::new();

    registry
        .spawn_agent(
            thread_id,
            ExternalAgentLaunchRequest {
                host_thread: host_thread(&root, "claude-opus-4-6").await,
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
    let status = wait_for_final_status(&registry, thread_id).await;
    assert_eq!(status, AgentStatus::Completed(Some("run-1".to_string())));

    registry
        .send_input(thread_id, text_input("second"))
        .await
        .expect("send input to external agent");
    for _ in 0..100 {
        if registry.get_status(thread_id).await == Some(AgentStatus::Running) {
            break;
        }
        tokio::task::yield_now().await;
    }
    for _ in 0..100 {
        if read_logged_invocations(&mock_script).args.len() >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    registry
        .interrupt_agent(thread_id)
        .await
        .expect("interrupt external agent");
    for _ in 0..100 {
        if registry.get_status(thread_id).await == Some(AgentStatus::Interrupted) {
            break;
        }
        tokio::task::yield_now().await;
    }

    registry
        .send_input(thread_id, text_input("third"))
        .await
        .expect("send third input to external agent");
    let status = wait_for_status_change(&registry, thread_id, &AgentStatus::Interrupted).await;
    let status = if matches!(status, AgentStatus::Completed(_)) {
        status
    } else {
        wait_for_final_status(&registry, thread_id).await
    };
    let AgentStatus::Completed(Some(output)) = status else {
        panic!("expected completed output after interrupted follow-up");
    };
    assert!(output.starts_with("run-"));

    let invocations = read_logged_invocations(&mock_script);
    assert!(invocations.args.len() >= 2);
    assert!(!invocations.args[0].contains("--resume"));
    let last_args = invocations.args.last().expect("last args invocation");
    assert!(last_args.contains(&format!("--resume\n{MOCK_SESSION_ID}")));
    let last_stdin = invocations.stdin.last().expect("last stdin invocation");
    assert!(last_stdin.contains("third"));
    assert!(!last_stdin.contains("parent context"));
}

#[tokio::test]
async fn run_claude_code_turn_rejects_empty_output() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-empty-claude.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\ncat >/dev/null\ncat <<'EOF'\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"empty-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":0,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"empty-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"\",\"stop_reason\":\"end_turn\",\"session_id\":\"empty-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":0},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n",
    )
    .expect("write empty mock Claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("empty mock Claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod empty mock Claude");
    }

    let error = run_claude_code_turn(
        &ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        ClaudeCliRequest {
            cwd: root.path().to_path_buf(),
            model: "claude-opus-4-6".to_string(),
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            session: ClaudeCliSession::Ephemeral,
            json_schema: None,
            tools: None,
            force_toolless: false,
            effort: None,
        },
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect_err("empty Claude output should fail");
    assert!(error.to_string().contains("empty output"));
}

#[tokio::test]
async fn interrupted_or_rejected_resume_clears_claude_session_for_future_turns() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-resume-reject.sh");
    let args_log_path = root.path().join("resume-reject.args.log");
    let stdin_log_path = root.path().join("resume-reject.stdin.log");
    let count_path = root.path().join("resume-reject.count");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r stdin_payload\nprintf '%s\\n' \"$@\" >> '{}'\nprintf -- '--\\n' >> '{}'\nprintf '%s\\n--\\n' \"$stdin_payload\" >> '{}'\ncount=0\nif [[ -f '{}' ]]; then\n  count=$(cat '{}')\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nresume_value=''\nfor ((i=1; i<=$#; i++)); do\n  if [[ \"${{!i}}\" == '--session-id' ]]; then\n    echo 'unexpected --session-id' >&2\n    exit 99\n  fi\n  if [[ \"${{!i}}\" == '--resume' ]]; then\n    next=$((i + 1))\n    resume_value=\"${{!next}}\"\n  fi\ndone\nif [[ \"$count\" == '2' && -n \"$resume_value\" ]]; then\n  echo 'resume rejected by carrier' >&2\n  exit 42\nfi\nsession_id='123e4567-e89b-12d3-a456-426614174000'\ncat <<EOF\n{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session_id\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-$count\"}}\n{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-6\",\"id\":\"msg-$count\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"run-$count\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}},\"context_management\":null}},\"parent_tool_use_id\":null,\"session_id\":\"$session_id\",\"uuid\":\"assistant-$count\"}}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"run-$count\",\"stop_reason\":\"end_turn\",\"session_id\":\"$session_id\",\"total_cost_usd\":0.0,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2}},\"modelUsage\":{{}},\"permission_denials\":[],\"uuid\":\"result-$count\"}}\nEOF\n",
            args_log_path.display(),
            args_log_path.display(),
            stdin_log_path.display(),
            count_path.display(),
            count_path.display(),
            count_path.display(),
        ),
    )
    .expect("write resume rejection mock Claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("resume rejection mock Claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod resume rejection mock Claude");
    }

    let registry = ExternalAgentRegistry::default();
    let thread_id = ThreadId::new();
    registry
        .spawn_agent(
            thread_id,
            ExternalAgentLaunchRequest {
                host_thread: host_thread(&root, "claude-opus-4-6").await,
                config_snapshot: test_snapshot(root.path(), "claude-opus-4-6"),
                developer_instructions: Some("Follow repo truth".to_string()),
                claude_cli: ClaudeCliConfig {
                    path: Some(script_path),
                    ..Default::default()
                },
                model: "claude-opus-4-6".to_string(),
                parent_context: Some("[1] user: parent context".to_string()),
            },
            text_input("first"),
        )
        .await
        .expect("spawn external agent");
    assert_eq!(
        wait_for_final_status(&registry, thread_id).await,
        AgentStatus::Completed(Some("run-1".to_string()))
    );

    registry
        .send_input(thread_id, text_input("second"))
        .await
        .expect("send second input");
    let status = wait_for_status_change(
        &registry,
        thread_id,
        &AgentStatus::Completed(Some("run-1".to_string())),
    )
    .await;
    let status = if matches!(status, AgentStatus::Errored(_)) {
        status
    } else {
        wait_for_final_status(&registry, thread_id).await
    };
    assert!(matches!(
        status,
        AgentStatus::Errored(ref message) if message.contains("resume rejected")
    ));

    registry
        .send_input(thread_id, text_input("third"))
        .await
        .expect("send third input");
    let status = wait_for_status_change(&registry, thread_id, &status).await;
    let status = if matches!(status, AgentStatus::Completed(_)) {
        status
    } else {
        wait_for_final_status(&registry, thread_id).await
    };
    assert_eq!(status, AgentStatus::Completed(Some("run-3".to_string())));

    let args_log = std::fs::read_to_string(args_log_path).expect("read args log");
    let stdin_log = std::fs::read_to_string(stdin_log_path).expect("read stdin log");
    let arg_entries = args_log
        .split("--\n")
        .filter(|entry| !entry.trim().is_empty())
        .collect::<Vec<_>>();
    let stdin_entries = stdin_log
        .split("\n--\n")
        .filter(|entry| !entry.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(arg_entries.len(), 3);
    assert!(arg_entries[1].contains("--resume"));
    assert!(!arg_entries[2].contains("--resume"));
    assert_eq!(stdin_entries.len(), 3);
    assert!(stdin_entries[2].contains("Follow repo truth"));
    assert!(stdin_entries[2].contains("parent context"));
}

#[test]
fn external_agent_continuation_prompt_only_carries_current_message() {
    let prompt = build_external_agent_continuation_prompt("follow-up task");
    assert!(prompt.contains("follow-up task"));
    assert!(!prompt.contains("Follow repo truth"));
    assert!(!prompt.contains("parent context"));
}

#[test]
fn default_claude_model_prefers_claude_names_and_falls_back_to_opus_46() {
    assert_eq!(
        default_claude_model(Some("claude-sonnet-4-6")),
        "claude-sonnet-4-6"
    );
    assert_eq!(default_claude_model(Some("haiku")), "haiku");
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
