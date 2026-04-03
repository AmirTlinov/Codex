use super::*;
use crate::CodexAuth;
use crate::CodexThread;
use crate::ThreadManager;
use crate::agent::agent_status_from_event;
use crate::config::AgentRoleConfig;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::config::Constrained;
use crate::config_loader::LoaderOverrides;
use crate::contextual_user_message::SUBAGENT_NOTIFICATION_OPEN_TAG;
use assert_matches::assert_matches;
use chrono::Utc;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;
use toml::Value as TomlValue;

async fn test_config_with_cli_overrides(
    cli_overrides: Vec<(String, TomlValue)>,
) -> (TempDir, Config) {
    let home = TempDir::new().expect("create temp dir");
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .cli_overrides(cli_overrides)
        .loader_overrides(LoaderOverrides {
            #[cfg(target_os = "macos")]
            managed_preferences_base64: Some(String::new()),
            macos_managed_config_requirements_base64: Some(String::new()),
            ..LoaderOverrides::default()
        })
        .build()
        .await
        .expect("load default test config");
    (home, config)
}

async fn test_config() -> (TempDir, Config) {
    test_config_with_cli_overrides(Vec::new()).await
}

fn text_input(text: &str) -> Op {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
    .into()
}

struct AgentControlHarness {
    _home: TempDir,
    config: Config,
    manager: ThreadManager,
    control: AgentControl,
}

struct MockClaudeScript {
    script_path: std::path::PathBuf,
    stdin_log_path: std::path::PathBuf,
}

fn mock_claude_permission_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-control-claude-permission.sh");
    let stdin_log_path = root.path().join("control-claude-permission.stdin.log");
    let script = [
        "#!/usr/bin/env bash".to_string(),
        "set -euo pipefail".to_string(),
        "IFS= read -r first_line".to_string(),
        format!("printf '%s\\n--\\n' \"$first_line\" >> '{}'", stdin_log_path.display()),
        "session_id='123e4567-e89b-12d3-a456-4266141740aa'".to_string(),
        "cat <<'EOF'".to_string(),
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740aa\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"default\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}".to_string(),
        "{\"type\":\"control_request\",\"request_id\":\"req-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Read\",\"input\":{\"file_path\":\"AGENTS.md\"},\"tool_use_id\":\"tool-1\",\"description\":\"Read AGENTS.md\"}}".to_string(),
        "EOF".to_string(),
        "while IFS= read -r line; do".to_string(),
        format!("  printf '%s\\n--\\n' \"$line\" >> '{}'", stdin_log_path.display()),
        "  if [[ \"$line\" == *'\"type\":\"control_response\"'* && \"$line\" == *'\"request_id\":\"req-1\"'* && \"$line\" == *'\"behavior\":\"allow\"'* ]]; then".to_string(),
        "    cat <<'EOF'".to_string(),
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"approved\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"123e4567-e89b-12d3-a456-4266141740aa\",\"uuid\":\"assistant-1\"}".to_string(),
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"approved\",\"stop_reason\":\"end_turn\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740aa\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}".to_string(),
        "EOF".to_string(),
        "    exit 0".to_string(),
        "  fi".to_string(),
        "done".to_string(),
        "exit 13".to_string(),
    ]
    .join("\n");
    std::fs::write(
        &script_path,
        script,
    )
    .expect("write mock Claude permission script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock Claude permission metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod mock Claude permission");
    }
    MockClaudeScript {
        script_path,
        stdin_log_path,
    }
}

fn mock_claude_bash_permission_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-control-claude-bash-permission.sh");
    let stdin_log_path = root.path().join("control-claude-bash-permission.stdin.log");
    let script = [
        "#!/usr/bin/env bash".to_string(),
        "set -euo pipefail".to_string(),
        "IFS= read -r first_line".to_string(),
        format!("printf '%s\\n--\\n' \"$first_line\" >> '{}'", stdin_log_path.display()),
        "session_id='123e4567-e89b-12d3-a456-4266141740ab'".to_string(),
        "cat <<'EOF'".to_string(),
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ab\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"default\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}".to_string(),
        "{\"type\":\"control_request\",\"request_id\":\"req-bash-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{\"command\":\"git status\"},\"tool_use_id\":\"tool-bash-1\",\"description\":\"Run git status\"}}".to_string(),
        "EOF".to_string(),
        "while IFS= read -r line; do".to_string(),
        format!("  printf '%s\\n--\\n' \"$line\" >> '{}'", stdin_log_path.display()),
        "  if [[ \"$line\" == *'\"type\":\"control_response\"'* && \"$line\" == *'\"request_id\":\"req-bash-1\"'* && \"$line\" == *'\"behavior\":\"allow\"'* ]]; then".to_string(),
        "    cat <<'EOF'".to_string(),
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"bash-approved\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ab\",\"uuid\":\"assistant-1\"}".to_string(),
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"bash-approved\",\"stop_reason\":\"end_turn\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ab\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}".to_string(),
        "EOF".to_string(),
        "    exit 0".to_string(),
        "  fi".to_string(),
        "done".to_string(),
        "exit 13".to_string(),
    ]
    .join("\n");
    std::fs::write(&script_path, script).expect("write mock Claude bash permission script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock Claude bash permission metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod mock Claude bash permission");
    }
    MockClaudeScript {
        script_path,
        stdin_log_path,
    }
}

fn mock_claude_interruptible_permission_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-control-claude-interrupt-permission.sh");
    let stdin_log_path = root.path().join("control-claude-interrupt-permission.stdin.log");
    let count_path = root.path().join("control-claude-interrupt-permission.count");
    let script = [
        "#!/usr/bin/env bash".to_string(),
        "set -euo pipefail".to_string(),
        "IFS= read -r first_line".to_string(),
        format!("printf '%s\\n--\\n' \"$first_line\" >> '{}'", stdin_log_path.display()),
        format!("count=0; if [[ -f '{}' ]]; then count=$(cat '{}'); fi", count_path.display(), count_path.display()),
        "count=$((count + 1))".to_string(),
        format!("printf '%s' \"$count\" > '{}'", count_path.display()),
        "session_id='123e4567-e89b-12d3-a456-4266141740ac'".to_string(),
        "if [[ \"$count\" == '1' ]]; then".to_string(),
        "  cat <<'EOF'".to_string(),
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ac\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"default\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}".to_string(),
        "{\"type\":\"control_request\",\"request_id\":\"req-int-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Read\",\"input\":{\"file_path\":\"AGENTS.md\"},\"tool_use_id\":\"tool-int-1\",\"description\":\"Read AGENTS.md\"}}".to_string(),
        "EOF".to_string(),
        "  sleep 30".to_string(),
        "else".to_string(),
        "  cat <<'EOF'".to_string(),
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ac\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"default\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-2\"}".to_string(),
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-2\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"after-interrupt\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ac\",\"uuid\":\"assistant-2\"}".to_string(),
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"after-interrupt\",\"stop_reason\":\"end_turn\",\"session_id\":\"123e4567-e89b-12d3-a456-4266141740ac\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-2\"}".to_string(),
        "EOF".to_string(),
        "fi".to_string(),
    ]
    .join("\n");
    std::fs::write(&script_path, script).expect("write mock Claude interrupt permission script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock Claude interrupt permission metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod mock Claude interrupt permission");
    }
    MockClaudeScript {
        script_path,
        stdin_log_path,
    }
}

impl AgentControlHarness {
    async fn new() -> Self {
        let (home, config) = test_config().await;
        let manager = ThreadManager::with_models_provider_and_home_for_tests(
            CodexAuth::from_api_key("dummy"),
            config.model_provider.clone(),
            config.codex_home.clone(),
            std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
                /*exec_server_url*/ None,
            )),
        );
        let control = manager.agent_control();
        Self {
            _home: home,
            config,
            manager,
            control,
        }
    }

    async fn start_thread(&self) -> (ThreadId, Arc<CodexThread>) {
        let new_thread = self
            .manager
            .start_thread(self.config.clone())
            .await
            .expect("start thread");
        (new_thread.thread_id, new_thread.thread)
    }
}

fn mock_claude_script(root: &TempDir) -> MockClaudeScript {
    let script_path = root.path().join("mock-control-claude.sh");
    let stdin_log_path = root.path().join("control-claude.stdin.log");
    let count_path = root.path().join("control-claude.count");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r stdin_payload\nprintf '%s\\n--\\n' \"$stdin_payload\" >> '{}'\ncount=0\nif [[ -f '{}' ]]; then\n  count=$(cat '{}')\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nsession_id='123e4567-e89b-12d3-a456-426614174001'\ncat <<EOF\n{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session_id\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-$count\"}}\n{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-6\",\"id\":\"msg-$count\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"run-$count\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}},\"context_management\":null}},\"parent_tool_use_id\":null,\"session_id\":\"$session_id\",\"uuid\":\"assistant-$count\"}}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"run-$count\",\"stop_reason\":\"end_turn\",\"session_id\":\"$session_id\",\"total_cost_usd\":0.0,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2}},\"modelUsage\":{{}},\"permission_denials\":[],\"uuid\":\"result-$count\"}}\nEOF\n",
            stdin_log_path.display(),
            count_path.display(),
            count_path.display(),
            count_path.display(),
        ),
    )
    .expect("write mock Claude script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock Claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod mock Claude");
    }
    MockClaudeScript {
        script_path,
        stdin_log_path,
    }
}

fn has_subagent_notification(history_items: &[ResponseItem]) -> bool {
    history_items.iter().any(|item| {
        let ResponseItem::Message { role, content, .. } = item else {
            return false;
        };
        if role != "user" {
            return false;
        }
        content.iter().any(|content_item| match content_item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                text.contains(SUBAGENT_NOTIFICATION_OPEN_TAG)
            }
            ContentItem::InputImage { .. } => false,
        })
    })
}

/// Returns true when any message item contains `needle` in a text span.
fn history_contains_text(history_items: &[ResponseItem], needle: &str) -> bool {
    history_items.iter().any(|item| {
        let ResponseItem::Message { content, .. } = item else {
            return false;
        };
        content.iter().any(|content_item| match content_item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                text.contains(needle)
            }
            ContentItem::InputImage { .. } => false,
        })
    })
}

fn history_contains_assistant_inter_agent_communication(
    history_items: &[ResponseItem],
    expected: &InterAgentCommunication,
) -> bool {
    history_items.iter().any(|item| {
        let ResponseItem::Message { role, content, .. } = item else {
            return false;
        };
        if role != "assistant" {
            return false;
        }
        content.iter().any(|content_item| match content_item {
            ContentItem::OutputText { text } => {
                serde_json::from_str::<InterAgentCommunication>(text)
                    .ok()
                    .as_ref()
                    == Some(expected)
            }
            ContentItem::InputText { .. } | ContentItem::InputImage { .. } => false,
        })
    })
}

async fn wait_for_subagent_notification(parent_thread: &Arc<CodexThread>) -> bool {
    let wait = async {
        loop {
            let history_items = parent_thread
                .codex
                .session
                .clone_history()
                .await
                .raw_items()
                .to_vec();
            if has_subagent_notification(&history_items) {
                return true;
            }
            sleep(Duration::from_millis(25)).await;
        }
    };
    timeout(Duration::from_secs(2), wait).await.is_ok()
}

async fn persist_thread_for_tree_resume(thread: &Arc<CodexThread>, message: &str) {
    thread
        .inject_user_message_without_turn(message.to_string())
        .await;
    thread.codex.session.ensure_rollout_materialized().await;
    thread.codex.session.flush_rollout().await;
}

async fn wait_for_live_thread_spawn_children(
    control: &AgentControl,
    parent_thread_id: ThreadId,
    expected_children: &[ThreadId],
) {
    let mut expected_children = expected_children.to_vec();
    expected_children.sort_by_key(std::string::ToString::to_string);

    timeout(Duration::from_secs(5), async {
        loop {
            let mut child_ids = control
                .open_thread_spawn_children(parent_thread_id)
                .await
                .expect("live child list should load")
                .into_iter()
                .map(|(thread_id, _)| thread_id)
                .collect::<Vec<_>>();
            child_ids.sort_by_key(std::string::ToString::to_string);
            if child_ids == expected_children {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("expected persisted child tree");
}

async fn wait_for_final_agent_status(control: &AgentControl, thread_id: ThreadId) -> AgentStatus {
    timeout(Duration::from_secs(5), async {
        loop {
            let status = control.get_status(thread_id).await;
            if matches!(
                status,
                AgentStatus::Completed(_)
                    | AgentStatus::Errored(_)
                    | AgentStatus::Interrupted
                    | AgentStatus::Shutdown
            ) {
                break status;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("agent should reach a final status")
}

async fn wait_for_agent_status_change(
    control: &AgentControl,
    thread_id: ThreadId,
    previous_status: &AgentStatus,
) -> AgentStatus {
    timeout(Duration::from_secs(5), async {
        loop {
            let status = control.get_status(thread_id).await;
            if &status != previous_status {
                break status;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("agent status should change")
}

#[tokio::test]
async fn send_input_errors_when_manager_dropped() {
    let control = AgentControl::default();
    let err = control
        .send_input(
            ThreadId::new(),
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
        )
        .await
        .expect_err("send_input should fail without a manager");
    assert_eq!(
        err.to_string(),
        "unsupported operation: thread manager dropped"
    );
}

#[tokio::test]
async fn get_status_returns_not_found_without_manager() {
    let control = AgentControl::default();
    let got = control.get_status(ThreadId::new()).await;
    assert_eq!(got, AgentStatus::NotFound);
}

#[tokio::test]
async fn on_event_updates_status_from_task_started() {
    let status = agent_status_from_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: "turn-1".to_string(),
        model_context_window: None,
        collaboration_mode_kind: ModeKind::Default,
    }));
    assert_eq!(status, Some(AgentStatus::Running));
}

#[tokio::test]
async fn on_event_updates_status_from_task_complete() {
    let status = agent_status_from_event(&EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: "turn-1".to_string(),
        last_agent_message: Some("done".to_string()),
    }));
    let expected = AgentStatus::Completed(Some("done".to_string()));
    assert_eq!(status, Some(expected));
}

#[tokio::test]
async fn on_event_updates_status_from_error() {
    let status = agent_status_from_event(&EventMsg::Error(ErrorEvent {
        message: "boom".to_string(),
        codex_error_info: None,
    }));

    let expected = AgentStatus::Errored("boom".to_string());
    assert_eq!(status, Some(expected));
}

#[tokio::test]
async fn on_event_updates_status_from_turn_aborted() {
    let status = agent_status_from_event(&EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id: Some("turn-1".to_string()),
        reason: TurnAbortReason::Interrupted,
    }));

    let expected = AgentStatus::Interrupted;
    assert_eq!(status, Some(expected));
}

#[tokio::test]
async fn on_event_updates_status_from_shutdown_complete() {
    let status = agent_status_from_event(&EventMsg::ShutdownComplete);
    assert_eq!(status, Some(AgentStatus::Shutdown));
}

#[tokio::test]
async fn spawn_agent_errors_when_manager_dropped() {
    let control = AgentControl::default();
    let (_home, config) = test_config().await;
    let err = control
        .spawn_agent(config, text_input("hello"), /*session_source*/ None)
        .await
        .expect_err("spawn_agent should fail without a manager");
    assert_eq!(
        err.to_string(),
        "unsupported operation: thread manager dropped"
    );
}

#[tokio::test]
async fn resume_agent_errors_when_manager_dropped() {
    let control = AgentControl::default();
    let (_home, config) = test_config().await;
    let err = control
        .resume_agent_from_rollout(config, ThreadId::new(), SessionSource::Exec)
        .await
        .expect_err("resume_agent should fail without a manager");
    assert_eq!(
        err.to_string(),
        "unsupported operation: thread manager dropped"
    );
}

#[tokio::test]
async fn send_input_errors_when_thread_missing() {
    let harness = AgentControlHarness::new().await;
    let thread_id = ThreadId::new();
    let err = harness
        .control
        .send_input(
            thread_id,
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
        )
        .await
        .expect_err("send_input should fail for missing thread");
    assert_matches!(err, CodexErr::ThreadNotFound(id) if id == thread_id);
}

#[tokio::test]
async fn get_status_returns_not_found_for_missing_thread() {
    let harness = AgentControlHarness::new().await;
    let status = harness.control.get_status(ThreadId::new()).await;
    assert_eq!(status, AgentStatus::NotFound);
}

#[tokio::test]
async fn get_status_returns_pending_init_for_new_thread() {
    let harness = AgentControlHarness::new().await;
    let (thread_id, _) = harness.start_thread().await;
    let status = harness.control.get_status(thread_id).await;
    assert_eq!(status, AgentStatus::PendingInit);
}

#[tokio::test]
async fn subscribe_status_errors_for_missing_thread() {
    let harness = AgentControlHarness::new().await;
    let thread_id = ThreadId::new();
    let err = harness
        .control
        .subscribe_status(thread_id)
        .await
        .expect_err("subscribe_status should fail for missing thread");
    assert_matches!(err, CodexErr::ThreadNotFound(id) if id == thread_id);
}

#[tokio::test]
async fn subscribe_status_updates_on_shutdown() {
    let harness = AgentControlHarness::new().await;
    let (thread_id, thread) = harness.start_thread().await;
    let mut status_rx = harness
        .control
        .subscribe_status(thread_id)
        .await
        .expect("subscribe_status should succeed");
    assert_eq!(status_rx.borrow().clone(), AgentStatus::PendingInit);

    let _ = thread
        .submit(Op::Shutdown {})
        .await
        .expect("shutdown should submit");

    let _ = status_rx.changed().await;
    assert_eq!(status_rx.borrow().clone(), AgentStatus::Shutdown);
}

#[tokio::test]
async fn send_input_submits_user_message() {
    let harness = AgentControlHarness::new().await;
    let (thread_id, _thread) = harness.start_thread().await;

    let submission_id = harness
        .control
        .send_input(
            thread_id,
            vec![UserInput::Text {
                text: "hello from tests".to_string(),
                text_elements: Vec::new(),
            }]
            .into(),
        )
        .await
        .expect("send_input should succeed");
    assert!(!submission_id.is_empty());
    let expected = (
        thread_id,
        Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello from tests".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        },
    );
    let captured = harness
        .manager
        .captured_ops()
        .into_iter()
        .find(|entry| *entry == expected);
    assert_eq!(captured, Some(expected));
}

#[tokio::test]
async fn send_inter_agent_communication_without_turn_queues_message_without_triggering_turn() {
    let harness = AgentControlHarness::new().await;
    let (thread_id, thread) = harness.start_thread().await;
    let communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::try_from("/root/worker").expect("agent path"),
        Vec::new(),
        "hello from tests".to_string(),
        /*trigger_turn*/ false,
    );

    let submission_id = harness
        .control
        .send_inter_agent_communication(thread_id, communication.clone())
        .await
        .expect("send_inter_agent_communication should succeed");
    assert!(!submission_id.is_empty());

    let expected = (
        thread_id,
        Op::InterAgentCommunication {
            communication: communication.clone(),
        },
    );
    let captured = harness
        .manager
        .captured_ops()
        .into_iter()
        .find(|entry| *entry == expected);
    assert_eq!(captured, Some(expected));

    timeout(Duration::from_secs(5), async {
        loop {
            if thread.codex.session.has_pending_input().await {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("inter-agent communication should stay pending");

    let history_items = thread
        .codex
        .session
        .clone_history()
        .await
        .raw_items()
        .to_vec();
    assert!(!history_contains_assistant_inter_agent_communication(
        &history_items,
        &communication
    ));
}

#[tokio::test]
async fn send_inter_agent_communication_routes_to_external_claude_agent() {
    let harness = AgentControlHarness::new().await;
    let mock_claude = mock_claude_script(&harness._home);
    let mut config = harness.config.clone();
    config.agent_backend = crate::config::AgentBackend::ClaudeCode;
    config.claude_cli.path = Some(mock_claude.script_path.clone());

    let thread_id = harness
        .control
        .spawn_agent(
            config,
            text_input("initial external task"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn external Claude agent");
    let status = wait_for_final_agent_status(&harness.control, thread_id).await;
    assert_eq!(status, AgentStatus::Completed(Some("run-1".to_string())));

    let communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::try_from("/root/claude_worker").expect("agent path"),
        Vec::new(),
        "delegated follow-up".to_string(),
        /*trigger_turn*/ false,
    );
    let submission_id = harness
        .control
        .send_inter_agent_communication(thread_id, communication)
        .await
        .expect("external inter-agent communication should succeed");
    assert!(!submission_id.is_empty());

    let status = wait_for_agent_status_change(
        &harness.control,
        thread_id,
        &AgentStatus::Completed(Some("run-1".to_string())),
    )
    .await;
    let status = if matches!(status, AgentStatus::Completed(_)) {
        status
    } else {
        wait_for_final_agent_status(&harness.control, thread_id).await
    };
    assert_eq!(status, AgentStatus::Completed(Some("run-2".to_string())));

    let stdin_log =
        std::fs::read_to_string(&mock_claude.stdin_log_path).expect("read Claude stdin log");
    assert!(stdin_log.contains("delegated follow-up"));
}

#[tokio::test]
async fn external_claude_agent_routes_permission_requests_through_child_thread() {
    let harness = AgentControlHarness::new().await;
    let mock_claude = mock_claude_permission_script(&harness._home);
    let mut config = harness.config.clone();
    config.agent_backend = crate::config::AgentBackend::ClaudeCode;
    config.claude_cli.path = Some(mock_claude.script_path.clone());
    config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);

    let thread_id = harness
        .control
        .spawn_agent(config, text_input("read AGENTS via child approval"), /*session_source*/ None)
        .await
        .expect("spawn external Claude agent");
    let child_thread = harness
        .manager
        .get_thread(thread_id)
        .await
        .expect("external child thread host should exist");

    let request = timeout(Duration::from_secs(5), async {
        loop {
            let event = child_thread.next_event().await.expect("child thread event");
            if matches!(event.msg, EventMsg::RequestPermissions(_)) {
                break event.msg;
            }
        }
    })
    .await
    .expect("permission request should arrive");
    let EventMsg::RequestPermissions(request) = request else {
        panic!("expected request_permissions event");
    };
    child_thread
        .submit(Op::RequestPermissionsResponse {
            id: request.call_id.clone(),
            response: RequestPermissionsResponse {
                permissions: request.permissions,
                scope: PermissionGrantScope::Turn,
            },
        })
        .await
        .expect("approve external child permission request");

    let status = wait_for_final_agent_status(&harness.control, thread_id).await;
    assert_eq!(status, AgentStatus::Completed(Some("approved".to_string())));

    let stdin_log =
        std::fs::read_to_string(&mock_claude.stdin_log_path).expect("read permission stdin log");
    assert!(stdin_log.contains("\"type\":\"control_response\""));
    assert!(stdin_log.contains("\"request_id\":\"req-1\""));
}

#[tokio::test]
async fn external_claude_agent_routes_bash_approval_through_child_thread() {
    let harness = AgentControlHarness::new().await;
    let mock_claude = mock_claude_bash_permission_script(&harness._home);
    let mut config = harness.config.clone();
    config.agent_backend = crate::config::AgentBackend::ClaudeCode;
    config.claude_cli.path = Some(mock_claude.script_path.clone());
    config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);

    let thread_id = harness
        .control
        .spawn_agent(
            config,
            text_input("run git status via child approval"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn external Claude agent");
    let child_thread = harness
        .manager
        .get_thread(thread_id)
        .await
        .expect("external child thread host should exist");

    let request = timeout(Duration::from_secs(5), async {
        loop {
            let event = child_thread.next_event().await.expect("child thread event");
            if matches!(event.msg, EventMsg::ExecApprovalRequest(_)) {
                break event.msg;
            }
        }
    })
    .await
    .expect("bash exec approval request should arrive");
    let EventMsg::ExecApprovalRequest(request) = request else {
        panic!("expected exec approval request");
    };
    child_thread
        .submit(Op::ExecApproval {
            id: request.effective_approval_id(),
            turn_id: Some(request.turn_id),
            decision: ReviewDecision::Approved,
        })
        .await
        .expect("approve external child bash request");

    let status = wait_for_final_agent_status(&harness.control, thread_id).await;
    assert_eq!(status, AgentStatus::Completed(Some("bash-approved".to_string())));

    let stdin_log = std::fs::read_to_string(&mock_claude.stdin_log_path)
        .expect("read bash permission stdin log");
    assert!(stdin_log.contains("\"type\":\"control_response\""));
    assert!(stdin_log.contains("\"request_id\":\"req-bash-1\""));
}

#[tokio::test]
async fn external_claude_interrupt_clears_pending_child_thread_permission_wait() {
    let harness = AgentControlHarness::new().await;
    let mock_claude = mock_claude_interruptible_permission_script(&harness._home);
    let mut config = harness.config.clone();
    config.agent_backend = crate::config::AgentBackend::ClaudeCode;
    config.claude_cli.path = Some(mock_claude.script_path.clone());
    config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);

    let thread_id = harness
        .control
        .spawn_agent(
            config,
            text_input("interrupt pending permission"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn external Claude agent");
    let child_thread = harness
        .manager
        .get_thread(thread_id)
        .await
        .expect("external child thread host should exist");

    let _request = timeout(Duration::from_secs(5), async {
        loop {
            let event = child_thread.next_event().await.expect("child thread event");
            if matches!(event.msg, EventMsg::RequestPermissions(_)) {
                break event.msg;
            }
        }
    })
    .await
    .expect("permission request should arrive before interrupt");

    harness
        .control
        .interrupt_agent(thread_id)
        .await
        .expect("interrupt external agent");
    let status = wait_for_final_agent_status(&harness.control, thread_id).await;
    assert_eq!(status, AgentStatus::Interrupted);

    harness
        .control
        .send_input(thread_id, text_input("run after interrupt"))
        .await
        .expect("send follow-up after interrupt");
    let status = wait_for_agent_status_change(&harness.control, thread_id, &AgentStatus::Interrupted)
        .await;
    let status = if matches!(status, AgentStatus::Completed(_)) {
        status
    } else {
        wait_for_final_agent_status(&harness.control, thread_id).await
    };
    assert_eq!(status, AgentStatus::Completed(Some("after-interrupt".to_string())));
}

#[tokio::test]
async fn append_message_records_assistant_message() {
    let harness = AgentControlHarness::new().await;
    let (thread_id, thread) = harness.start_thread().await;
    let message =
        "author: /root\nrecipient: /root/worker\nother_recipients: []\nContent: hello from tests";

    let submission_id = harness
        .control
        .append_message(
            thread_id,
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::InputText {
                    text: message.to_string(),
                }],
                end_turn: None,
                phase: None,
            },
        )
        .await
        .expect("append_message should succeed");
    assert!(!submission_id.is_empty());

    timeout(Duration::from_secs(5), async {
        loop {
            let history_items = thread
                .codex
                .session
                .clone_history()
                .await
                .raw_items()
                .to_vec();
            let recorded = history_items.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::Message { role, content, .. }
                        if role == "assistant"
                            && content.iter().any(|content_item| matches!(
                                content_item,
                                ContentItem::InputText { text } if text == message
                            ))
                )
            });
            if recorded {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("assistant message should be recorded");
}

#[tokio::test]
async fn spawn_agent_creates_thread_and_sends_prompt() {
    let harness = AgentControlHarness::new().await;
    let thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("spawned"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed");
    let _thread = harness
        .manager
        .get_thread(thread_id)
        .await
        .expect("thread should be registered");
    let expected = (
        thread_id,
        Op::UserInput {
            items: vec![UserInput::Text {
                text: "spawned".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        },
    );
    let captured = harness
        .manager
        .captured_ops()
        .into_iter()
        .find(|entry| *entry == expected);
    assert_eq!(captured, Some(expected));
}

#[tokio::test]
async fn spawn_agent_can_fork_parent_thread_history() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;
    parent_thread
        .inject_user_message_without_turn("parent seed context".to_string())
        .await;
    let turn_context = parent_thread.codex.session.new_default_turn().await;
    let parent_spawn_call_id = "spawn-call-history".to_string();
    let parent_spawn_call = ResponseItem::FunctionCall {
        id: None,
        name: "spawn_agent".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: parent_spawn_call_id.clone(),
    };
    parent_thread
        .codex
        .session
        .record_conversation_items(turn_context.as_ref(), &[parent_spawn_call])
        .await;
    parent_thread
        .codex
        .session
        .ensure_rollout_materialized()
        .await;
    parent_thread.codex.session.flush_rollout().await;

    let child_thread_id = harness
        .control
        .spawn_agent_with_metadata(
            harness.config.clone(),
            text_input("child task"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            SpawnAgentOptions {
                fork_parent_spawn_call_id: Some(parent_spawn_call_id),
                fork_mode: Some(SpawnAgentForkMode::FullHistory),
            },
        )
        .await
        .expect("forked spawn should succeed")
        .thread_id;

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    assert_ne!(child_thread_id, parent_thread_id);
    let history = child_thread.codex.session.clone_history().await;
    assert!(history_contains_text(
        history.raw_items(),
        "parent seed context"
    ));

    let expected = (
        child_thread_id,
        Op::UserInput {
            items: vec![UserInput::Text {
                text: "child task".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        },
    );
    let captured = harness
        .manager
        .captured_ops()
        .into_iter()
        .find(|entry| *entry == expected);
    assert_eq!(captured, Some(expected));

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should submit");
    let _ = parent_thread
        .submit(Op::Shutdown {})
        .await
        .expect("parent shutdown should submit");
}

#[tokio::test]
async fn spawn_agent_fork_injects_output_for_parent_spawn_call() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;
    let turn_context = parent_thread.codex.session.new_default_turn().await;
    let parent_spawn_call_id = "spawn-call-1".to_string();
    let parent_spawn_call = ResponseItem::FunctionCall {
        id: None,
        name: "spawn_agent".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: parent_spawn_call_id.clone(),
    };
    parent_thread
        .codex
        .session
        .record_conversation_items(turn_context.as_ref(), &[parent_spawn_call])
        .await;
    parent_thread
        .codex
        .session
        .ensure_rollout_materialized()
        .await;
    parent_thread.codex.session.flush_rollout().await;

    let child_thread_id = harness
        .control
        .spawn_agent_with_metadata(
            harness.config.clone(),
            text_input("child task"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            SpawnAgentOptions {
                fork_parent_spawn_call_id: Some(parent_spawn_call_id.clone()),
                fork_mode: Some(SpawnAgentForkMode::FullHistory),
            },
        )
        .await
        .expect("forked spawn should succeed")
        .thread_id;

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    let history = child_thread.codex.session.clone_history().await;
    let injected_output = history.raw_items().iter().find_map(|item| match item {
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == &parent_spawn_call_id =>
        {
            Some(output)
        }
        _ => None,
    });
    let injected_output =
        injected_output.expect("forked child should contain synthetic tool output");
    assert_eq!(
        injected_output.text_content(),
        Some(FORKED_SPAWN_AGENT_OUTPUT_MESSAGE)
    );
    assert_eq!(injected_output.success, Some(true));

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should submit");
    let _ = parent_thread
        .submit(Op::Shutdown {})
        .await
        .expect("parent shutdown should submit");
}

#[tokio::test]
async fn spawn_agent_fork_flushes_parent_rollout_before_loading_history() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;
    let turn_context = parent_thread.codex.session.new_default_turn().await;
    let parent_spawn_call_id = "spawn-call-unflushed".to_string();
    let parent_spawn_call = ResponseItem::FunctionCall {
        id: None,
        name: "spawn_agent".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: parent_spawn_call_id.clone(),
    };
    parent_thread
        .codex
        .session
        .record_conversation_items(turn_context.as_ref(), &[parent_spawn_call])
        .await;

    let child_thread_id = harness
        .control
        .spawn_agent_with_metadata(
            harness.config.clone(),
            text_input("child task"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            SpawnAgentOptions {
                fork_parent_spawn_call_id: Some(parent_spawn_call_id.clone()),
                fork_mode: Some(SpawnAgentForkMode::FullHistory),
            },
        )
        .await
        .expect("forked spawn should flush parent rollout before loading history")
        .thread_id;

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    let history = child_thread.codex.session.clone_history().await;

    let mut parent_call_index = None;
    let mut injected_output_index = None;
    for (idx, item) in history.raw_items().iter().enumerate() {
        match item {
            ResponseItem::FunctionCall { call_id, .. } if call_id == &parent_spawn_call_id => {
                parent_call_index = Some(idx);
            }
            ResponseItem::FunctionCallOutput { call_id, .. }
                if call_id == &parent_spawn_call_id =>
            {
                injected_output_index = Some(idx);
            }
            _ => {}
        }
    }

    let parent_call_index =
        parent_call_index.expect("forked child should include the parent spawn_agent call");
    let injected_output_index = injected_output_index
        .expect("forked child should include synthetic output for the parent spawn_agent call");
    assert!(parent_call_index < injected_output_index);

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should submit");
    let _ = parent_thread
        .submit(Op::Shutdown {})
        .await
        .expect("parent shutdown should submit");
}

#[tokio::test]
async fn spawn_agent_fork_last_n_turns_keeps_only_recent_turns() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    parent_thread
        .inject_user_message_without_turn("old parent context".to_string())
        .await;
    let queued_communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::try_from("/root/worker").expect("agent path"),
        Vec::new(),
        "queued message".to_string(),
        /*trigger_turn*/ false,
    );
    let queued_turn_context = parent_thread.codex.session.new_default_turn().await;
    parent_thread
        .codex
        .session
        .record_conversation_items(
            queued_turn_context.as_ref(),
            &[queued_communication.to_response_input_item().into()],
        )
        .await;

    let triggered_communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::try_from("/root/worker").expect("agent path"),
        Vec::new(),
        "triggered context".to_string(),
        /*trigger_turn*/ true,
    );
    let triggered_turn_context = parent_thread.codex.session.new_default_turn().await;
    parent_thread
        .codex
        .session
        .record_conversation_items(
            triggered_turn_context.as_ref(),
            &[triggered_communication.to_response_input_item().into()],
        )
        .await;

    parent_thread
        .inject_user_message_without_turn("current parent task".to_string())
        .await;
    let spawn_turn_context = parent_thread.codex.session.new_default_turn().await;
    let parent_spawn_call_id = "spawn-call-last-n".to_string();
    let parent_spawn_call = ResponseItem::FunctionCall {
        id: None,
        name: "spawn_agent".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: parent_spawn_call_id.clone(),
    };
    parent_thread
        .codex
        .session
        .record_conversation_items(spawn_turn_context.as_ref(), &[parent_spawn_call])
        .await;
    parent_thread
        .codex
        .session
        .ensure_rollout_materialized()
        .await;
    parent_thread.codex.session.flush_rollout().await;

    let child_thread_id = harness
        .control
        .spawn_agent_with_metadata(
            harness.config.clone(),
            text_input("child task"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            SpawnAgentOptions {
                fork_parent_spawn_call_id: Some(parent_spawn_call_id),
                fork_mode: Some(SpawnAgentForkMode::LastNTurns(2)),
            },
        )
        .await
        .expect("forked spawn should keep only the last two turns")
        .thread_id;

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    let history = child_thread.codex.session.clone_history().await;

    assert!(!history_contains_text(
        history.raw_items(),
        "old parent context"
    ));
    assert!(!history_contains_text(
        history.raw_items(),
        "queued message"
    ));
    assert!(history_contains_text(
        history.raw_items(),
        "triggered context"
    ));
    assert!(history_contains_text(
        history.raw_items(),
        "current parent task"
    ));

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should submit");
    let _ = parent_thread
        .submit(Op::Shutdown {})
        .await
        .expect("parent shutdown should submit");
}

#[tokio::test]
async fn spawn_agent_respects_max_threads_limit() {
    let max_threads = 1usize;
    let (_home, config) = test_config_with_cli_overrides(vec![(
        "agents.max_threads".to_string(),
        TomlValue::Integer(max_threads as i64),
    )])
    .await;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();

    let _ = manager
        .start_thread(config.clone())
        .await
        .expect("start thread");

    let first_agent_id = control
        .spawn_agent(
            config.clone(),
            text_input("hello"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed");

    let err = control
        .spawn_agent(
            config,
            text_input("hello again"),
            /*session_source*/ None,
        )
        .await
        .expect_err("spawn_agent should respect max threads");
    let CodexErr::AgentLimitReached {
        max_threads: seen_max_threads,
    } = err
    else {
        panic!("expected CodexErr::AgentLimitReached");
    };
    assert_eq!(seen_max_threads, max_threads);

    let _ = control
        .shutdown_live_agent(first_agent_id)
        .await
        .expect("shutdown agent");
}

#[tokio::test]
async fn spawn_agent_releases_slot_after_shutdown() {
    let max_threads = 1usize;
    let (_home, config) = test_config_with_cli_overrides(vec![(
        "agents.max_threads".to_string(),
        TomlValue::Integer(max_threads as i64),
    )])
    .await;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();

    let first_agent_id = control
        .spawn_agent(
            config.clone(),
            text_input("hello"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed");
    let _ = control
        .shutdown_live_agent(first_agent_id)
        .await
        .expect("shutdown agent");

    let second_agent_id = control
        .spawn_agent(
            config.clone(),
            text_input("hello again"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed after shutdown");
    let _ = control
        .shutdown_live_agent(second_agent_id)
        .await
        .expect("shutdown agent");
}

#[tokio::test]
async fn spawn_agent_limit_shared_across_clones() {
    let max_threads = 1usize;
    let (_home, config) = test_config_with_cli_overrides(vec![(
        "agents.max_threads".to_string(),
        TomlValue::Integer(max_threads as i64),
    )])
    .await;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();
    let cloned = control.clone();

    let first_agent_id = cloned
        .spawn_agent(
            config.clone(),
            text_input("hello"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed");

    let err = control
        .spawn_agent(
            config,
            text_input("hello again"),
            /*session_source*/ None,
        )
        .await
        .expect_err("spawn_agent should respect shared guard");
    let CodexErr::AgentLimitReached { max_threads } = err else {
        panic!("expected CodexErr::AgentLimitReached");
    };
    assert_eq!(max_threads, 1);

    let _ = control
        .shutdown_live_agent(first_agent_id)
        .await
        .expect("shutdown agent");
}

#[tokio::test]
async fn resume_agent_respects_max_threads_limit() {
    let max_threads = 1usize;
    let (_home, config) = test_config_with_cli_overrides(vec![(
        "agents.max_threads".to_string(),
        TomlValue::Integer(max_threads as i64),
    )])
    .await;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();

    let resumable_id = control
        .spawn_agent(
            config.clone(),
            text_input("hello"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed");
    let _ = control
        .shutdown_live_agent(resumable_id)
        .await
        .expect("shutdown resumable thread");

    let active_id = control
        .spawn_agent(
            config.clone(),
            text_input("occupy"),
            /*session_source*/ None,
        )
        .await
        .expect("spawn_agent should succeed for active slot");

    let err = control
        .resume_agent_from_rollout(config, resumable_id, SessionSource::Exec)
        .await
        .expect_err("resume should respect max threads");
    let CodexErr::AgentLimitReached {
        max_threads: seen_max_threads,
    } = err
    else {
        panic!("expected CodexErr::AgentLimitReached");
    };
    assert_eq!(seen_max_threads, max_threads);

    let _ = control
        .shutdown_live_agent(active_id)
        .await
        .expect("shutdown active thread");
}

#[tokio::test]
async fn resume_agent_releases_slot_after_resume_failure() {
    let max_threads = 1usize;
    let (_home, config) = test_config_with_cli_overrides(vec![(
        "agents.max_threads".to_string(),
        TomlValue::Integer(max_threads as i64),
    )])
    .await;
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();

    let _ = control
        .resume_agent_from_rollout(config.clone(), ThreadId::new(), SessionSource::Exec)
        .await
        .expect_err("resume should fail for missing rollout path");

    let resumed_id = control
        .spawn_agent(config, text_input("hello"), /*session_source*/ None)
        .await
        .expect("spawn should succeed after failed resume");
    let _ = control
        .shutdown_live_agent(resumed_id)
        .await
        .expect("shutdown resumed thread");
}

#[tokio::test]
async fn spawn_child_completion_notifies_parent_history() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let _ = child_thread
        .submit(Op::Shutdown {})
        .await
        .expect("child shutdown should submit");

    assert_eq!(wait_for_subagent_notification(&parent_thread).await, true);
}

#[tokio::test]
async fn multi_agent_v2_completion_ignores_dead_direct_parent() {
    let harness = AgentControlHarness::new().await;
    let (root_thread_id, root_thread) = harness.start_thread().await;
    let mut config = harness.config.clone();
    let _ = config.features.enable(Feature::MultiAgentV2);
    let worker_path = AgentPath::root().join("worker_a").expect("worker path");
    let worker_thread_id = harness
        .control
        .spawn_agent(
            config.clone(),
            text_input("hello worker"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root_thread_id,
                depth: 1,
                agent_path: Some(worker_path.clone()),
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("worker spawn should succeed");
    let tester_path = worker_path.join("tester").expect("tester path");
    let tester_thread_id = harness
        .control
        .spawn_agent(
            config,
            text_input("hello tester"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: worker_thread_id,
                depth: 2,
                agent_path: Some(tester_path.clone()),
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("tester spawn should succeed");
    harness
        .control
        .shutdown_live_agent(worker_thread_id)
        .await
        .expect("worker shutdown should succeed");

    let tester_thread = harness
        .manager
        .get_thread(tester_thread_id)
        .await
        .expect("tester thread should exist");
    let tester_turn = tester_thread.codex.session.new_default_turn().await;
    tester_thread
        .codex
        .session
        .send_event(
            tester_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: tester_turn.sub_id.clone(),
                last_agent_message: Some("done".to_string()),
            }),
        )
        .await;

    sleep(Duration::from_millis(100)).await;

    assert!(
        !harness
            .manager
            .captured_ops()
            .into_iter()
            .any(|(thread_id, op)| {
                thread_id == worker_thread_id
                    && matches!(
                        op,
                        Op::InterAgentCommunication { communication }
                            if communication.author == tester_path
                                && communication.recipient == worker_path
                                && communication.content == "done"
                    )
            })
    );

    let root_history_items = root_thread
        .codex
        .session
        .clone_history()
        .await
        .raw_items()
        .to_vec();
    assert!(!history_contains_assistant_inter_agent_communication(
        &root_history_items,
        &InterAgentCommunication::new(
            tester_path,
            AgentPath::root(),
            Vec::new(),
            "done".to_string(),
            /*trigger_turn*/ true,
        )
    ));
    assert!(!has_subagent_notification(&root_history_items));
}

#[tokio::test]
async fn multi_agent_v2_completion_queues_message_for_direct_parent() {
    let harness = AgentControlHarness::new().await;
    let (_root_thread_id, root_thread) = harness.start_thread().await;
    let (worker_thread_id, _worker_thread) = harness.start_thread().await;
    let mut tester_config = harness.config.clone();
    let _ = tester_config.features.enable(Feature::MultiAgentV2);
    let tester_thread_id = harness
        .manager
        .start_thread(tester_config)
        .await
        .expect("tester thread should start")
        .thread_id;
    let tester_thread = harness
        .manager
        .get_thread(tester_thread_id)
        .await
        .expect("tester thread should exist");
    let worker_path = AgentPath::root().join("worker_a").expect("worker path");
    let tester_path = worker_path.join("tester").expect("tester path");
    harness.control.maybe_start_completion_watcher(
        tester_thread_id,
        Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: worker_thread_id,
            depth: 2,
            agent_path: Some(tester_path.clone()),
            agent_nickname: None,
            agent_role: Some("explorer".to_string()),
        })),
        tester_path.to_string(),
        Some(tester_path.clone()),
    );
    let tester_turn = tester_thread.codex.session.new_default_turn().await;
    tester_thread
        .codex
        .session
        .send_event(
            tester_turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: tester_turn.sub_id.clone(),
                last_agent_message: Some("done".to_string()),
            }),
        )
        .await;

    let expected_message = crate::session_prefix::format_subagent_notification_message(
        tester_path.as_str(),
        &AgentStatus::Completed(Some("done".to_string())),
    );
    let expected = (
        worker_thread_id,
        Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                tester_path.clone(),
                worker_path.clone(),
                Vec::new(),
                expected_message.clone(),
                /*trigger_turn*/ false,
            ),
        },
    );

    timeout(Duration::from_secs(5), async {
        loop {
            let captured = harness
                .manager
                .captured_ops()
                .into_iter()
                .find(|entry| *entry == expected);
            if captured == Some(expected.clone()) {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completion watcher should queue a direct-parent message");

    let root_history_items = root_thread
        .codex
        .session
        .clone_history()
        .await
        .raw_items()
        .to_vec();
    assert!(!history_contains_assistant_inter_agent_communication(
        &root_history_items,
        &InterAgentCommunication::new(
            tester_path,
            AgentPath::root(),
            Vec::new(),
            expected_message,
            /*trigger_turn*/ false,
        )
    ));
}

#[tokio::test]
async fn completion_watcher_notifies_parent_when_child_is_missing() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;
    let child_thread_id = ThreadId::new();

    harness.control.maybe_start_completion_watcher(
        child_thread_id,
        Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: Some("explorer".to_string()),
        })),
        child_thread_id.to_string(),
        /*child_agent_path*/ None,
    );

    assert_eq!(wait_for_subagent_notification(&parent_thread).await, true);

    let history_items = parent_thread
        .codex
        .session
        .clone_history()
        .await
        .raw_items()
        .to_vec();
    assert_eq!(
        history_contains_text(
            &history_items,
            &format!("\"agent_path\":\"{child_thread_id}\"")
        ),
        true
    );
    assert_eq!(
        history_contains_text(&history_items, "\"status\":\"not_found\""),
        true
    );
}

#[tokio::test]
async fn spawn_thread_subagent_gets_random_nickname_in_session_source() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, _parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    let snapshot = child_thread.config_snapshot().await;

    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: seen_parent_thread_id,
        depth,
        agent_nickname,
        agent_role,
        ..
    }) = snapshot.session_source
    else {
        panic!("expected thread-spawn sub-agent source");
    };
    assert_eq!(seen_parent_thread_id, parent_thread_id);
    assert_eq!(depth, 1);
    assert!(agent_nickname.is_some());
    assert_eq!(agent_role, Some("explorer".to_string()));
}

#[tokio::test]
async fn spawn_thread_subagent_uses_role_specific_nickname_candidates() {
    let mut harness = AgentControlHarness::new().await;
    harness.config.agent_roles.insert(
        "researcher".to_string(),
        AgentRoleConfig {
            description: Some("Research role".to_string()),
            config_file: None,
            nickname_candidates: Some(vec!["Atlas".to_string()]),
        },
    );
    let (parent_thread_id, _parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("researcher".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should be registered");
    let snapshot = child_thread.config_snapshot().await;

    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_nickname, .. }) =
        snapshot.session_source
    else {
        panic!("expected thread-spawn sub-agent source");
    };
    assert_eq!(agent_nickname, Some("Atlas".to_string()));
}

#[tokio::test]
async fn resume_thread_subagent_restores_stored_nickname_and_role() {
    let (home, mut config) = test_config().await;
    config
        .features
        .enable(Feature::Sqlite)
        .expect("test config should allow sqlite");
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let control = manager.agent_control();
    let harness = AgentControlHarness {
        _home: home,
        config,
        manager,
        control,
    };
    let (parent_thread_id, _parent_thread) = harness.start_thread().await;
    let agent_path = AgentPath::from_string("/root/explorer".to_string())
        .expect("test agent path should be valid");

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: Some(agent_path.clone()),
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let mut status_rx = harness
        .control
        .subscribe_status(child_thread_id)
        .await
        .expect("status subscription should succeed");
    if matches!(status_rx.borrow().clone(), AgentStatus::PendingInit) {
        timeout(Duration::from_secs(5), async {
            loop {
                status_rx
                    .changed()
                    .await
                    .expect("child status should advance past pending init");
                if !matches!(status_rx.borrow().clone(), AgentStatus::PendingInit) {
                    break;
                }
            }
        })
        .await
        .expect("child should initialize before shutdown");
    }
    let original_snapshot = child_thread.config_snapshot().await;
    let original_nickname = original_snapshot
        .session_source
        .get_nickname()
        .expect("spawned sub-agent should have a nickname");
    let state_db = child_thread
        .state_db()
        .expect("sqlite state db should be available for nickname resume test");
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Some(metadata)) = state_db.get_thread(child_thread_id).await
                && metadata.agent_nickname.is_some()
                && metadata.agent_role.as_deref() == Some("explorer")
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child thread metadata should be persisted to sqlite before shutdown");

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should submit");

    let resumed_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            child_thread_id,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: Some(agent_path.clone()),
                agent_nickname: None,
                agent_role: None,
            }),
        )
        .await
        .expect("resume should succeed");
    assert_eq!(resumed_thread_id, child_thread_id);

    let resumed_snapshot = harness
        .manager
        .get_thread(resumed_thread_id)
        .await
        .expect("resumed child thread should exist")
        .config_snapshot()
        .await;
    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: resumed_parent_thread_id,
        depth: resumed_depth,
        agent_path: resumed_agent_path,
        agent_nickname: resumed_nickname,
        agent_role: resumed_role,
        ..
    }) = resumed_snapshot.session_source
    else {
        panic!("expected thread-spawn sub-agent source");
    };
    assert_eq!(resumed_parent_thread_id, parent_thread_id);
    assert_eq!(resumed_depth, 1);
    assert_eq!(resumed_agent_path, Some(agent_path));
    assert_eq!(resumed_nickname, Some(original_nickname));
    assert_eq!(resumed_role, Some("explorer".to_string()));

    let _ = harness
        .control
        .shutdown_live_agent(resumed_thread_id)
        .await
        .expect("resumed child shutdown should submit");
}

#[tokio::test]
async fn resume_agent_from_rollout_reads_archived_rollout_path() {
    let harness = AgentControlHarness::new().await;
    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello"),
            /*session_source*/ None,
        )
        .await
        .expect("child spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    persist_thread_for_tree_resume(&child_thread, "persist before archiving").await;
    let rollout_path = child_thread
        .rollout_path()
        .expect("thread should have rollout path");
    let state_db = child_thread
        .state_db()
        .expect("thread should have state db handle");

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("child shutdown should succeed");

    let archived_root = harness
        .config
        .codex_home
        .join(crate::ARCHIVED_SESSIONS_SUBDIR);
    tokio::fs::create_dir_all(&archived_root)
        .await
        .expect("archived root should exist");
    let archived_rollout_path = archived_root.join(
        rollout_path
            .file_name()
            .expect("rollout file name should be present"),
    );
    tokio::fs::rename(&rollout_path, &archived_rollout_path)
        .await
        .expect("rollout should move to archived path");
    state_db
        .mark_archived(child_thread_id, archived_rollout_path.as_path(), Utc::now())
        .await
        .expect("state db archive update should succeed");

    let resumed_thread_id = harness
        .control
        .resume_agent_from_rollout(harness.config.clone(), child_thread_id, SessionSource::Exec)
        .await
        .expect("resume should find archived rollout");
    assert_eq!(resumed_thread_id, child_thread_id);

    let _ = harness
        .control
        .shutdown_live_agent(child_thread_id)
        .await
        .expect("resumed child shutdown should succeed");
}

#[tokio::test]
async fn shutdown_agent_tree_closes_live_descendants() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, _parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown should succeed");

    assert_eq!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let shutdown_ids = harness
        .manager
        .captured_ops()
        .into_iter()
        .filter_map(|(thread_id, op)| matches!(op, Op::Shutdown).then_some(thread_id))
        .collect::<Vec<_>>();
    let mut expected_shutdown_ids = vec![parent_thread_id, child_thread_id, grandchild_thread_id];
    expected_shutdown_ids.sort_by_key(std::string::ToString::to_string);
    let mut shutdown_ids = shutdown_ids;
    shutdown_ids.sort_by_key(std::string::ToString::to_string);
    assert_eq!(shutdown_ids, expected_shutdown_ids);
}

#[tokio::test]
async fn shutdown_agent_tree_closes_descendants_when_started_at_child() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, _parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let _ = harness
        .control
        .close_agent(child_thread_id)
        .await
        .expect("child close should succeed");

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown should succeed");

    assert_eq!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );

    let shutdown_ids = harness
        .manager
        .captured_ops()
        .into_iter()
        .filter_map(|(thread_id, op)| matches!(op, Op::Shutdown).then_some(thread_id))
        .collect::<Vec<_>>();
    let mut expected_shutdown_ids = vec![parent_thread_id, child_thread_id, grandchild_thread_id];
    expected_shutdown_ids.sort_by_key(std::string::ToString::to_string);
    let mut shutdown_ids = shutdown_ids;
    shutdown_ids.sort_by_key(std::string::ToString::to_string);
    assert_eq!(shutdown_ids, expected_shutdown_ids);
}

#[tokio::test]
async fn resume_agent_from_rollout_does_not_reopen_closed_descendants() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&parent_thread, "parent persisted").await;
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let _ = harness
        .control
        .close_agent(child_thread_id)
        .await
        .expect("child close should succeed");
    let _ = harness
        .control
        .shutdown_live_agent(parent_thread_id)
        .await
        .expect("parent shutdown should succeed");

    let resumed_parent_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            parent_thread_id,
            SessionSource::Exec,
        )
        .await
        .expect("single-thread resume should succeed");
    assert_eq!(resumed_parent_thread_id, parent_thread_id);
    assert_ne!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown after resume should succeed");
}

#[tokio::test]
async fn resume_closed_child_reopens_open_descendants() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&parent_thread, "parent persisted").await;
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let _ = harness
        .control
        .close_agent(child_thread_id)
        .await
        .expect("child close should succeed");

    let resumed_child_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            child_thread_id,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            }),
        )
        .await
        .expect("child resume should succeed");
    assert_eq!(resumed_child_thread_id, child_thread_id);
    assert_ne!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let _ = harness
        .control
        .close_agent(child_thread_id)
        .await
        .expect("child close after resume should succeed");
    let _ = harness
        .control
        .shutdown_live_agent(parent_thread_id)
        .await
        .expect("parent shutdown should succeed");
}

#[tokio::test]
async fn resume_agent_from_rollout_reopens_open_descendants_after_manager_shutdown() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&parent_thread, "parent persisted").await;
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let report = harness
        .manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    assert_eq!(report.submit_failed, Vec::<ThreadId>::new());
    assert_eq!(report.timed_out, Vec::<ThreadId>::new());

    let resumed_parent_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            parent_thread_id,
            SessionSource::Exec,
        )
        .await
        .expect("tree resume should succeed");
    assert_eq!(resumed_parent_thread_id, parent_thread_id);
    assert_ne!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown after subtree resume should succeed");
}

#[tokio::test]
async fn resume_agent_from_rollout_uses_edge_data_when_descendant_metadata_source_is_stale() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&parent_thread, "parent persisted").await;
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let state_db = grandchild_thread
        .state_db()
        .expect("sqlite state db should be available");
    let mut stale_metadata = state_db
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild metadata query should succeed")
        .expect("grandchild metadata should exist");
    stale_metadata.source =
        serde_json::to_string(&SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::new(),
            depth: 99,
            agent_path: None,
            agent_nickname: None,
            agent_role: Some("worker".to_string()),
        }))
        .expect("stale session source should serialize");
    state_db
        .upsert_thread(&stale_metadata)
        .await
        .expect("stale grandchild metadata should persist");

    let report = harness
        .manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    assert_eq!(report.submit_failed, Vec::<ThreadId>::new());
    assert_eq!(report.timed_out, Vec::<ThreadId>::new());

    let resumed_parent_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            parent_thread_id,
            SessionSource::Exec,
        )
        .await
        .expect("tree resume should succeed");
    assert_eq!(resumed_parent_thread_id, parent_thread_id);
    assert_ne!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_ne!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let resumed_grandchild_snapshot = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("resumed grandchild thread should exist")
        .config_snapshot()
        .await;
    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: resumed_parent_thread_id,
        depth: resumed_depth,
        ..
    }) = resumed_grandchild_snapshot.session_source
    else {
        panic!("expected thread-spawn sub-agent source");
    };
    assert_eq!(resumed_parent_thread_id, child_thread_id);
    assert_eq!(resumed_depth, 2);

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown after subtree resume should succeed");
}

#[tokio::test]
async fn resume_agent_from_rollout_skips_descendants_when_parent_resume_fails() {
    let harness = AgentControlHarness::new().await;
    let (parent_thread_id, parent_thread) = harness.start_thread().await;

    let child_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello child"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("explorer".to_string()),
            })),
        )
        .await
        .expect("child spawn should succeed");
    let grandchild_thread_id = harness
        .control
        .spawn_agent(
            harness.config.clone(),
            text_input("hello grandchild"),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: child_thread_id,
                depth: 2,
                agent_path: None,
                agent_nickname: None,
                agent_role: Some("worker".to_string()),
            })),
        )
        .await
        .expect("grandchild spawn should succeed");

    let child_thread = harness
        .manager
        .get_thread(child_thread_id)
        .await
        .expect("child thread should exist");
    let grandchild_thread = harness
        .manager
        .get_thread(grandchild_thread_id)
        .await
        .expect("grandchild thread should exist");
    persist_thread_for_tree_resume(&parent_thread, "parent persisted").await;
    persist_thread_for_tree_resume(&child_thread, "child persisted").await;
    persist_thread_for_tree_resume(&grandchild_thread, "grandchild persisted").await;
    wait_for_live_thread_spawn_children(&harness.control, parent_thread_id, &[child_thread_id])
        .await;
    wait_for_live_thread_spawn_children(&harness.control, child_thread_id, &[grandchild_thread_id])
        .await;

    let child_rollout_path = child_thread
        .rollout_path()
        .expect("child thread should have rollout path");
    let report = harness
        .manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    assert_eq!(report.submit_failed, Vec::<ThreadId>::new());
    assert_eq!(report.timed_out, Vec::<ThreadId>::new());
    tokio::fs::remove_file(&child_rollout_path)
        .await
        .expect("child rollout path should be removable");

    let resumed_parent_thread_id = harness
        .control
        .resume_agent_from_rollout(
            harness.config.clone(),
            parent_thread_id,
            SessionSource::Exec,
        )
        .await
        .expect("root resume should succeed");
    assert_eq!(resumed_parent_thread_id, parent_thread_id);
    assert_ne!(
        harness.control.get_status(parent_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(child_thread_id).await,
        AgentStatus::NotFound
    );
    assert_eq!(
        harness.control.get_status(grandchild_thread_id).await,
        AgentStatus::NotFound
    );

    let _ = harness
        .control
        .shutdown_agent_tree(parent_thread_id)
        .await
        .expect("tree shutdown after partial subtree resume should succeed");
}
