use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request as WiremockRequest;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn test_model_client(session_source: SessionSource) -> ModelClient {
    let provider = crate::model_provider_info::create_oss_provider_with_base_url(
        "https://example.com/v1",
        crate::model_provider_info::WireApi::Responses,
    );
    ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        provider,
        crate::config::ClaudeCliConfig::default(),
        std::path::PathBuf::new(),
        crate::auth::AuthCredentialsStoreMode::File,
        session_source,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_model_info_with_slug(slug: &str) -> ModelInfo {
    let mut model_info = test_model_info();
    model_info.slug = slug.to_string();
    model_info.display_name = slug.to_string();
    model_info
}

fn test_function_tool(name: &str, description: &str) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties: std::collections::BTreeMap::new(),
            required: None,
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
}

fn test_freeform_tool(name: &str, description: &str, syntax: &str, definition: &str) -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: name.to_string(),
        description: description.to_string(),
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: syntax.to_string(),
            definition: definition.to_string(),
        },
    })
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get("x-openai-subagent")
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(
            Vec::new(),
            &model_info,
            /*effort*/ None,
            &session_telemetry,
        )
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(crate::auth::AuthMode::Chatgpt),
        &crate::api_bridge::CoreAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

fn write_mock_claude_script(
    root: &TempDir,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let script_path = root.path().join("mock-claude.sh");
    let stdin_log_path = root.path().join("stdin.log");
    let args_log_path = root.path().join("args.log");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r stdin_payload\nprintf '%s' \"$stdin_payload\" > '{}'\nprintf '%s\\n' \"$@\" > '{}'\nsession_id='mock-session'\ncat <<'EOF'\n{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"mock-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}}\n{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"claude-main-ok\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}},\"context_management\":null}},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"claude-main-ok\",\"stop_reason\":\"end_turn\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2}},\"modelUsage\":{{}},\"permission_denials\":[],\"uuid\":\"result-1\"}}\nEOF\n",
            stdin_log_path.display(),
            args_log_path.display(),
        ),
    )
    .expect("write mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod mock claude");
    }
    (script_path, stdin_log_path, args_log_path)
}

fn write_mock_claude_waits_for_stdin_shutdown_script(
    root: &TempDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let script_path = root.path().join("mock-claude-waits-for-stdin-shutdown.sh");
    let stdin_log_path = root.path().join("stdin-shutdown.log");
    std::fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
IFS= read -r stdin_payload
printf '%s\n--\n' "$stdin_payload" > '{}'
cat <<'EOF'
{{"type":"system","subtype":"init","session_id":"mock-session","tools":[],"mcp_servers":[],"model":"claude-opus-4-6","permissionMode":"bypassPermissions","slash_commands":[],"apiKeySource":"none","claude_code_version":"test","output_style":"default","agents":[],"skills":[],"plugins":[],"uuid":"init-1"}}
{{"type":"assistant","message":{{"model":"claude-opus-4-6","id":"msg-1","type":"message","role":"assistant","content":[{{"type":"text","text":"claude-main-eof-ok"}}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2,"service_tier":"standard","inference_geo":"not_available"}},"context_management":null}},"parent_tool_use_id":null,"session_id":"mock-session","uuid":"assistant-1"}}
{{"type":"result","subtype":"success","is_error":false,"duration_ms":10,"duration_api_ms":10,"num_turns":1,"result":"claude-main-eof-ok","stop_reason":"end_turn","session_id":"mock-session","total_cost_usd":0.0,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2}},"modelUsage":{{}},"permission_denials":[],"uuid":"result-1"}}
EOF
while IFS= read -r trailing_line; do
  printf '%s\n--\n' "$trailing_line" >> '{}'
done
"#,
            stdin_log_path.display(),
            stdin_log_path.display(),
        ),
    )
    .expect("write stdin-shutdown mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("stdin-shutdown mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod stdin-shutdown mock claude");
    }
    (script_path, stdin_log_path)
}

fn write_mock_claude_logs_mcp_config_script(
    root: &TempDir,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let script_path = root.path().join("mock-claude-logs-mcp-config.sh");
    let args_log_path = root.path().join("mcp-config.args.log");
    let mcp_config_log_path = root.path().join("mcp-config.json");
    std::fs::write(
        &script_path,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
IFS= read -r _stdin_payload
printf '%s\n' "$@" > '{}'
for ((i=1; i<=$#; i++)); do
  if [[ "${{!i}}" == '--mcp-config' ]]; then
    next=$((i + 1))
    cat "${{!next}}" > '{}'
    break
  fi
done
cat <<'EOF'
{{"type":"system","subtype":"init","session_id":"mock-session","tools":[],"mcp_servers":[],"model":"claude-opus-4-6","permissionMode":"bypassPermissions","slash_commands":[],"apiKeySource":"none","claude_code_version":"test","output_style":"default","agents":[],"skills":[],"plugins":[],"uuid":"init-1"}}
{{"type":"assistant","message":{{"model":"claude-opus-4-6","id":"msg-1","type":"message","role":"assistant","content":[{{"type":"text","text":"mcp-bridge-ok"}}],"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2,"service_tier":"standard","inference_geo":"not_available"}},"context_management":null}},"parent_tool_use_id":null,"session_id":"mock-session","uuid":"assistant-1"}}
{{"type":"result","subtype":"success","is_error":false,"duration_ms":10,"duration_api_ms":10,"num_turns":1,"result":"mcp-bridge-ok","stop_reason":"end_turn","session_id":"mock-session","total_cost_usd":0.0,"usage":{{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2}},"modelUsage":{{}},"permission_denials":[],"uuid":"result-1"}}
EOF
"#,
            args_log_path.display(),
            mcp_config_log_path.display(),
        ),
    )
    .expect("write mcp-config mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("mcp-config mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod mcp-config mock claude");
    }
    (script_path, args_log_path, mcp_config_log_path)
}

fn write_mock_claude_tool_call_script(root: &TempDir) -> std::path::PathBuf {
    let script_path = root.path().join("mock-claude-tool-call.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _stdin_payload\ncat <<'EOF'\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"mock-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Launching worker.\\n\\n<tool_call>{\\\"name\\\":\\\"spawn_agent\\\",\\\"arguments\\\":{\\\"model\\\":\\\"gpt-5.4\\\",\\\"message\\\":\\\"ping\\\",\\\"reasoning_effort\\\":\\\"low\\\"}}</tool_call>\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"Launching worker.\\n\\n<tool_call>{\\\"name\\\":\\\"spawn_agent\\\",\\\"arguments\\\":{\\\"model\\\":\\\"gpt-5.4\\\",\\\"message\\\":\\\"ping\\\",\\\"reasoning_effort\\\":\\\"low\\\"}}</tool_call>\",\"stop_reason\":\"end_turn\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n",
    )
    .expect("write tool-call mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("tool-call mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod tool-call mock claude");
    }
    script_path
}

fn write_mock_claude_structured_tool_use_script(root: &TempDir) -> std::path::PathBuf {
    let script_path = root.path().join("mock-claude-structured-tool-use.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _stdin_payload\ncat <<'EOF'\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"mock-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Launching worker.\"},{\"type\":\"tool_use\",\"id\":\"toolu-1\",\"name\":\"spawn_agent\",\"input\":{\"model\":\"gpt-5.4\",\"message\":\"ping\",\"reasoning_effort\":\"low\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"Launching worker.\",\"stop_reason\":\"tool_use\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n",
    )
    .expect("write structured tool-use mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("structured tool-use mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod structured tool-use mock claude");
    }
    script_path
}

fn write_mock_claude_streaming_tool_use_script(root: &TempDir) -> std::path::PathBuf {
    let script_path = root.path().join("mock-claude-streaming-tool-use.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _stdin_payload\ncat <<'EOF'\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"mock-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-1\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu-1\",\"name\":\"spawn_agent\",\"input\":{}}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-2\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"model\\\":\\\"gpt-5.4\\\",\\\"message\\\":\\\"ping\\\",\\\"reasoning_effort\\\":\\\"low\\\"}\"}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-3\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_stop\",\"index\":0},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-4\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Launching worker.\"},{\"type\":\"tool_use\",\"id\":\"toolu-1\",\"name\":\"spawn_agent\",\"input\":{\"model\":\"gpt-5.4\",\"message\":\"ping\",\"reasoning_effort\":\"low\"}}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"Launching worker.\",\"stop_reason\":\"tool_use\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n",
    )
    .expect("write streaming tool-use mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("streaming tool-use mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod streaming tool-use mock claude");
    }
    script_path
}

#[tokio::test]
async fn stream_routes_main_turns_to_claude_cli_provider() {
    let root = TempDir::new().expect("create temp dir");
    let (script_path, stdin_log_path, args_log_path) = write_mock_claude_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::model_provider_info::create_claude_cli_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let mut prompt = crate::Prompt::default();
    prompt.base_instructions.text = "Follow repo truth".to_string();
    prompt.input = vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "Say hello from Claude.".to_string(),
        }],
        end_turn: None,
        phase: None,
    }];

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    let mut saw_message = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "claude-main-ok".to_string()
                    }]
                );
                saw_message = true;
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }

    assert!(
        saw_message,
        "expected Claude stream to emit an assistant message"
    );
    let stdin_log = std::fs::read_to_string(stdin_log_path).expect("read stdin log");
    assert!(stdin_log.contains("Say hello from Claude."));
    let args_log = std::fs::read_to_string(args_log_path).expect("read args log");
    assert!(args_log.contains("Follow repo truth"));
    assert!(args_log.contains("claude-opus-4-6"));
}

#[tokio::test]
async fn stream_prepends_runtime_truth_and_tool_inventory_to_claude_user_prompt() {
    let root = TempDir::new().expect("create temp dir");
    let (script_path, stdin_log_path, args_log_path) = write_mock_claude_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::model_provider_info::create_claude_cli_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let shell = crate::shell::Shell {
        shell_type: crate::shell::ShellType::Bash,
        shell_path: std::path::PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };
    let environment_context = crate::environment_context::EnvironmentContext::new(
        Some(root.path().to_path_buf()),
        shell,
        Some("2026-04-04".to_string()),
        Some("Europe/Moscow".to_string()),
        /*network*/ None,
        Some("- explorer: claude-sonnet-4-6\n- reviewer: gpt-5.4".to_string()),
    )
    .serialize_to_xml();
    let mut prompt = crate::Prompt::default();
    prompt.base_instructions.text = "Follow repo truth".to_string();
    prompt.input = vec![
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "<collaboration_mode>\nThe `request_user_input` tool is available in Default mode.\n</collaboration_mode>".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: environment_context,
            }],
            end_turn: None,
            phase: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Какие субагенты и модели тебе доступны?".to_string(),
            }],
            end_turn: None,
            phase: None,
        },
    ];
    prompt.tools = vec![
        test_function_tool(
            "spawn_agent",
            "Spawn a sub-agent for a well-scoped task.\n\n- Claude Opus 4.6 (`claude-opus-4-6`): flagship Claude worker.\n- GPT-5.4 (`gpt-5.4`): general frontier worker.\n\n### When to delegate\nUse only when the user explicitly asks for sub-agents.",
        ),
        test_function_tool(
            "exec_command",
            "Runs a command in a PTY and returns output or a session id for continued interaction.",
        ),
    ];

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    while let Some(event) = stream.next().await {
        if matches!(
            event.expect("stream event"),
            crate::client_common::ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }

    let stdin_log = std::fs::read_to_string(stdin_log_path).expect("read stdin log");
    assert!(stdin_log.contains("<codex_runtime_truth>"));
    assert!(stdin_log.contains("The `request_user_input` tool is available in Default mode."));
    assert!(stdin_log.contains("<environment_context>"));
    assert!(stdin_log.contains("- explorer: claude-sonnet-4-6"));
    assert!(stdin_log.contains("Current direct Claudex tool inventory: spawn_agent, exec_command"));
    assert!(stdin_log.contains("Claude Opus 4.6 (`claude-opus-4-6`)"));
    assert!(stdin_log.contains("Runs a command in a PTY"));
    assert!(stdin_log.contains("Follow the exact field names and required structure shown below."));
    assert!(stdin_log.contains("<input_schema>"));
    assert!(stdin_log.contains("<example>"));
    assert!(stdin_log.contains("Какие субагенты и модели тебе доступны?"));

    let args_log = std::fs::read_to_string(args_log_path).expect("read args log");
    assert!(args_log.contains("Treat that block as authoritative for this turn."));
    assert!(!args_log.contains("<environment_context>"));
    assert!(!args_log.contains("Claude Opus 4.6 (`claude-opus-4-6`)"));
}

#[tokio::test]
async fn stream_bridge_first_runtime_truth_hides_exec_surfaces_from_direct_inventory() {
    let root = TempDir::new().expect("create temp dir");
    let (script_path, stdin_log_path, args_log_path) = write_mock_claude_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            codex_self_exe: Some(std::path::PathBuf::from("/tmp/codex")),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let mut prompt = crate::Prompt::default();
    prompt.base_instructions.text = "Use the bridge-first runtime.".to_string();
    prompt.input = vec![ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "Inspect AGENTS.md and spawn a GPT worker if needed.".to_string(),
        }],
        end_turn: None,
        phase: None,
    }];
    prompt.tools = vec![
        test_function_tool("spawn_agent", "Spawn a sub-agent for a well-scoped task."),
        test_function_tool(
            "exec_command",
            "Runs a command in a PTY and returns output or a session id for continued interaction.",
        ),
        test_freeform_tool(
            "apply_patch",
            "Apply a patch to the working tree.",
            "patch",
            "Unified patch payload.",
        ),
    ];

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    while let Some(event) = stream.next().await {
        if matches!(
            event.expect("stream event"),
            crate::client_common::ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }

    let stdin_log = std::fs::read_to_string(stdin_log_path).expect("read stdin log");
    assert!(stdin_log.contains("<codex_runtime_truth>"));
    assert!(stdin_log.contains(
        "The internal Codex bridge is authoritative for shell, filesystem, network, and other Codex-owned execution work in this Claude turn."
    ));
    assert!(stdin_log.contains("Current direct Claudex tool inventory: spawn_agent"));
    assert!(
        !stdin_log.contains("Current direct Claudex tool inventory: spawn_agent, exec_command")
    );
    assert!(!stdin_log.contains("apply_patch"));

    let args_log = std::fs::read_to_string(args_log_path).expect("read args log");
    assert!(args_log.contains("--mcp-config"));
}

#[tokio::test]
async fn stream_closes_claude_stdin_after_terminal_result() {
    let root = TempDir::new().expect("create temp dir");
    let (script_path, stdin_log_path) = write_mock_claude_waits_for_stdin_shutdown_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::model_provider_info::create_claude_cli_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Say hello from Claude after EOF.".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should start");

    let mut saw_message = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            match event.expect("stream event") {
                crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                    content,
                    ..
                }) => {
                    assert_eq!(
                        content,
                        vec![ContentItem::OutputText {
                            text: "claude-main-eof-ok".to_string()
                        }]
                    );
                    saw_message = true;
                }
                crate::client_common::ResponseEvent::Completed { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("main Claude lane should finish once terminal result arrives");

    assert!(saw_message, "expected assistant message before completion");
    let stdin_log = std::fs::read_to_string(stdin_log_path).expect("read stdin-shutdown log");
    assert!(
        stdin_log.contains("Say hello from Claude after EOF."),
        "expected initial stream-json user message to be written"
    );
}

#[tokio::test]
async fn stream_passes_codex_mcp_bridge_config_to_claude_code() {
    let root = TempDir::new().expect("create temp dir");
    let (script_path, args_log_path, mcp_config_log_path) =
        write_mock_claude_logs_mcp_config_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::model_provider_info::create_claude_cli_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            codex_self_exe: Some(std::path::PathBuf::from("/tmp/codex")),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Use the Claude MCP bridge.".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should start");

    while let Some(event) = stream.next().await {
        if matches!(
            event.expect("stream event"),
            crate::client_common::ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }

    let args_log = std::fs::read_to_string(args_log_path).expect("read mcp args log");
    assert!(args_log.contains("--mcp-config"));
    assert!(args_log.contains("mcp__codex__codex"));
    assert!(args_log.contains("mcp__codex__codex-shell"));
    assert!(args_log.contains(
        "pass `model-provider` to `mcp__codex__codex` (for example `openai` or `claude_code`)"
    ));
    let mcp_config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(mcp_config_log_path).expect("read mcp config log"),
    )
    .expect("parse mcp config json");
    assert_eq!(
        mcp_config,
        serde_json::json!({
            "mcpServers": {
                "codex": {
                    "type": "stdio",
                    "command": "/tmp/codex",
                    "args": ["mcp-server"]
                }
            }
        })
    );
}

#[tokio::test]
async fn stream_translates_claude_tool_call_markup_into_codex_tool_items() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = write_mock_claude_tool_call_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Launch a worker".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        tools: vec![test_function_tool("spawn_agent", "Spawn a worker")],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    let mut saw_visible_text = false;
    let mut saw_tool_call = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "Launching worker.".to_string(),
                    }]
                );
                saw_visible_text = true;
            }
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                ..
            }) => {
                assert_eq!(name, "spawn_agent");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&arguments)
                        .expect("tool arguments should be valid json"),
                    json!({
                        "message": "ping",
                        "model": "gpt-5.4",
                        "reasoning_effort": "low"
                    })
                );
                saw_tool_call = true;
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }

    assert!(saw_visible_text);
    assert!(saw_tool_call);
}

#[tokio::test]
async fn stream_translates_structured_claude_tool_use_into_codex_tool_items() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = write_mock_claude_structured_tool_use_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Launch a worker".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        tools: vec![test_function_tool("spawn_agent", "Spawn a worker")],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    let mut saw_visible_text = false;
    let mut saw_tool_call = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "Launching worker.".to_string(),
                    }]
                );
                saw_visible_text = true;
            }
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                ..
            }) => {
                assert_eq!(name, "spawn_agent");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&arguments)
                        .expect("tool arguments should be valid json"),
                    json!({
                        "message": "ping",
                        "model": "gpt-5.4",
                        "reasoning_effort": "low"
                    })
                );
                saw_tool_call = true;
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }

    assert!(saw_visible_text);
    assert!(saw_tool_call);
}

#[tokio::test]
async fn stream_emits_claude_tool_use_from_stream_events_before_assistant_payload() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = write_mock_claude_streaming_tool_use_script(&root);
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Launch a worker".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        tools: vec![test_function_tool("spawn_agent", "Spawn a worker")],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    let mut saw_streamed_tool_call = false;
    let mut saw_visible_text = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                ..
            }) => {
                assert_eq!(name, "spawn_agent");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&arguments)
                        .expect("tool arguments should be valid json"),
                    json!({
                        "message": "ping",
                        "model": "gpt-5.4",
                        "reasoning_effort": "low"
                    })
                );
                saw_streamed_tool_call = true;
            }
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "Launching worker.".to_string(),
                    }]
                );
                assert!(
                    saw_streamed_tool_call,
                    "tool call should surface before final assistant payload"
                );
                saw_visible_text = true;
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }

    assert!(saw_streamed_tool_call);
    assert!(saw_visible_text);
}

#[tokio::test]
async fn stream_routes_partial_claude_code_deltas() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-claude-partial.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _first_line\ncat <<'EOF'\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"mock-session\",\"tools\":[],\"mcp_servers\":[],\"model\":\"claude-opus-4-6\",\"permissionMode\":\"bypassPermissions\",\"slash_commands\":[],\"apiKeySource\":\"none\",\"claude_code_version\":\"test\",\"output_style\":\"default\",\"agents\":[],\"skills\":[],\"plugins\":[],\"uuid\":\"init-1\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"}}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-1\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-2\"}\n{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}},\"session_id\":\"mock-session\",\"parent_tool_use_id\":null,\"uuid\":\"event-3\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"hi\",\"stop_reason\":\"end_turn\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":2},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n",
    )
    .expect("write partial mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("partial mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod partial mock claude");
    }

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Say hi".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should succeed");

    let mut saw_added = false;
    let mut saw_delta = false;
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event") {
            crate::client_common::ResponseEvent::OutputItemAdded(ResponseItem::Message {
                ..
            }) => {
                saw_added = true;
            }
            crate::client_common::ResponseEvent::OutputTextDelta(delta) => {
                assert_eq!(delta, "hi");
                saw_delta = true;
            }
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "hi".to_string()
                    }]
                );
                saw_done = true;
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }

    assert!(saw_added);
    assert!(saw_delta);
    assert!(saw_done);
}

#[tokio::test]
async fn stream_cancels_in_flight_claude_cli_subprocess() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-claude-sleep.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\ncat >/dev/null\nsleep 30\n",
    )
    .expect("write slow mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("slow mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod slow mock claude");
    }

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::model_provider_info::create_claude_cli_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Wait until cancelled.".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let cancellation_token = CancellationToken::new();
    let cancel_child = cancellation_token.child_token();
    let cancel_handle = tokio::spawn({
        let cancellation_token = cancellation_token.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancellation_token.cancel();
        }
    });

    let stream_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client_session.stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            cancel_child,
        ),
    )
    .await
    .expect("Claude stream cancellation should finish promptly");
    cancel_handle.await.expect("join cancel task");

    let mut stream = stream_result.expect("stream should start before cancellation");
    let err = stream
        .next()
        .await
        .expect("expected cancellation event")
        .expect_err("cancellation event should be an error");

    let crate::error::CodexErr::Stream(message, _response_id) = err else {
        panic!("expected stream error, got {err:?}");
    };
    assert_eq!(message, "Claude CLI run cancelled");
}

#[tokio::test]
async fn stream_routes_claude_code_permission_requests_through_response_events() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-claude-permission.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r first_line\nif [[ -z \"$first_line\" ]]; then\n  echo 'missing initial user message' >&2\n  exit 12\nfi\nprintf '%s\n' '{\"type\":\"control_request\",\"request_id\":\"req-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Read\",\"input\":{\"file_path\":\"AGENTS.md\"},\"tool_use_id\":\"tool-1\"}}'\nwhile IFS= read -r line; do\n  if [[ \"$line\" == *'\"type\":\"control_response\"'* && \"$line\" == *'\"request_id\":\"req-1\"'* && \"$line\" == *'\"behavior\":\"allow\"'* ]]; then\n    cat <<'EOF'\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"approved\"}],\"stop_reason\":null,\"stop_sequence\":null,\"stop_details\":null,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1,\"service_tier\":\"standard\",\"inference_geo\":\"not_available\"},\"context_management\":null},\"parent_tool_use_id\":null,\"session_id\":\"mock-session\",\"uuid\":\"assistant-1\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":10,\"duration_api_ms\":10,\"num_turns\":1,\"result\":\"approved\",\"stop_reason\":\"end_turn\",\"session_id\":\"mock-session\",\"total_cost_usd\":0.0,\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":1},\"modelUsage\":{},\"permission_denials\":[],\"uuid\":\"result-1\"}\nEOF\n    exit 0\n  fi\ndone\nexit 13\n",
    )
    .expect("write permission mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("permission mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod permission mock claude");
    }

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Read AGENTS.md".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should start");

    let mut saw_permission_request = false;
    let mut saw_assistant_output = false;
    while let Some(event) = stream.next().await {
        match event.expect("carrier event should succeed") {
            crate::client_common::ResponseEvent::ClaudeCodePermissionRequest(request) => {
                saw_permission_request = true;
                request
                    .responder()
                    .allow_for_request(request.request_id.clone(), /*updated_input*/ None)
                    .await
                    .expect("allow Claude permission request");
            }
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                saw_assistant_output = content.iter().any(
                    |item| matches!(item, ContentItem::OutputText { text } if text == "approved"),
                );
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }
    assert!(saw_permission_request, "expected permission request event");
    assert!(
        saw_assistant_output,
        "expected assistant output after allow"
    );
}

#[tokio::test]
async fn stream_fails_closed_on_unsupported_claude_code_control_request() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-claude-unsupported-control.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _first_line\nprintf '%s\n' '{\"type\":\"control_request\",\"request_id\":\"req-1\",\"request\":{\"subtype\":\"interrupt\"}}'\nsleep 30\n",
    )
    .expect("write unsupported control mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("unsupported control mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod unsupported control mock claude");
    }

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "interrupt".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should start");

    let event = stream
        .next()
        .await
        .expect("expected unsupported control failure event");
    let crate::error::CodexErr::Stream(message, _response_id) =
        event.expect_err("unsupported control request should fail closed")
    else {
        panic!("expected stream error");
    };
    assert!(message.contains("unsupported control_request"), "{message}");
}

#[tokio::test]
async fn stream_fails_closed_on_malformed_claude_code_permission_request() {
    let root = TempDir::new().expect("create temp dir");
    let script_path = root.path().join("mock-claude-malformed-control.sh");
    std::fs::write(
        &script_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nIFS= read -r _first_line\nprintf '%s\n' '{\"type\":\"control_request\",\"request_id\":\"req-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Read\",\"input\":{\"file_path\":\"AGENTS.md\"}}}'\nsleep 30\n",
    )
    .expect("write malformed control mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("malformed control mock claude metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions)
            .expect("chmod malformed control mock claude");
    }

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_claude_code_provider(),
        crate::config::ClaudeCliConfig {
            path: Some(script_path),
            ..Default::default()
        },
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Read AGENTS.md".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("claude stream should start");

    let event = stream
        .next()
        .await
        .expect("expected malformed control failure event");
    let crate::error::CodexErr::Stream(message, _response_id) =
        event.expect_err("malformed control request should fail closed")
    else {
        panic!("expected stream error");
    };
    assert!(message.contains("malformed can_use_tool"), "{message}");
}

#[tokio::test]
async fn stream_routes_main_turns_to_native_anthropic_provider() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"usage\":{\"input_tokens\":5}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from Anthropic.\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let codex_home = TempDir::new().expect("create temp dir");
    crate::auth::login_with_anthropic_api_key(
        codex_home.path(),
        "sk-ant-test",
        crate::auth::AuthCredentialsStoreMode::File,
    )
    .expect("save anthropic auth");

    let provider = crate::ModelProviderInfo {
        name: "Anthropic".to_string(),
        base_url: Some(format!("{}/v1", server.uri())),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        wire_api: crate::WireApi::Anthropic,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    };
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        provider,
        crate::config::ClaudeCliConfig::default(),
        codex_home.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Say hello from Anthropic.".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };
    let mut stream = client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            std::path::Path::new("."),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("native anthropic stream should succeed");

    let mut saw_message = false;
    while let Some(event) = stream.next().await {
        match event.expect("stream event should succeed") {
            crate::client_common::ResponseEvent::OutputItemDone(ResponseItem::Message {
                content,
                ..
            }) => {
                saw_message = true;
                assert_eq!(
                    content,
                    vec![ContentItem::OutputText {
                        text: "Hello from Anthropic.".to_string()
                    }]
                );
            }
            crate::client_common::ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }
    assert!(
        saw_message,
        "expected Anthropic stream to emit an assistant message"
    );
}

#[tokio::test]
async fn native_anthropic_requests_preserve_input_images() {
    struct AnthropicImageResponder;

    impl Respond for AnthropicImageResponder {
        fn respond(&self, request: &WiremockRequest) -> ResponseTemplate {
            let body: serde_json::Value = request
                .body_json()
                .expect("Anthropic request body should be valid JSON");
            let content = body["messages"][0]["content"]
                .as_array()
                .expect("Anthropic message content should be an array");
            assert!(
                content.iter().any(|item| {
                    item["type"].as_str() == Some("text")
                        && item["text"].as_str() == Some("Describe this image.")
                }),
                "expected Anthropic request to preserve the user text: {body}"
            );
            assert!(
                content.iter().any(|item| {
                    item["type"].as_str() == Some("image")
                        && item["source"]["type"].as_str() == Some("base64")
                        && item["source"]["media_type"].as_str() == Some("image/png")
                        && item["source"]["data"].as_str() == Some("AAA")
                }),
                "expected Anthropic request to preserve the image payload: {body}"
            );

            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-vision\",\"usage\":{\"input_tokens\":7}}}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Looks good.\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                ))
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(AnthropicImageResponder)
        .mount(&server)
        .await;

    let codex_home = TempDir::new().expect("create temp dir");
    crate::auth::login_with_anthropic_api_key(
        codex_home.path(),
        "sk-ant-test",
        crate::auth::AuthCredentialsStoreMode::File,
    )
    .expect("save anthropic auth");

    let provider = crate::ModelProviderInfo {
        name: "Anthropic".to_string(),
        base_url: Some(format!("{}/v1", server.uri())),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        wire_api: crate::WireApi::Anthropic,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    };
    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        provider,
        crate::config::ClaudeCliConfig::default(),
        codex_home.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let mut model_info = test_model_info_with_slug("claude-opus-4-6");
    model_info.input_modalities = vec![
        codex_protocol::openai_models::InputModality::Text,
        codex_protocol::openai_models::InputModality::Image,
    ];
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "Describe this image.".to_string(),
                },
                ContentItem::InputImage {
                    image_url: "data:image/png;base64,AAA".to_string(),
                },
            ],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };
    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            std::path::Path::new("."),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
        .expect("native anthropic stream should succeed");

    while let Some(event) = stream.next().await {
        if matches!(
            event.expect("stream event should succeed"),
            crate::client_common::ResponseEvent::Completed { .. }
        ) {
            break;
        }
    }
}

#[tokio::test]
async fn native_anthropic_rejects_claude_ai_oauth_tokens() {
    let root = TempDir::new().expect("create temp dir");
    std::fs::write(
        root.path().join("anthropic-auth.json"),
        serde_json::to_string_pretty(&json!({
            "auth_mode": "oauth",
            "api_key": null,
            "oauth": {
                "access_token": "oauth-access-token",
                "refresh_token": "oauth-refresh-token",
                "expires_at": null,
                "scopes": ["user:profile", "user:inference"],
                "profile": {
                    "email": "claude@example.com",
                    "display_name": "Claude User",
                    "organization_uuid": "org-anthropic",
                    "subscription_type": "max",
                    "rate_limit_tier": "default_claude_max_5x"
                }
            },
            "last_refresh": null
        }))
        .expect("serialize oauth auth fixture"),
    )
    .expect("write oauth auth fixture");

    let client = ModelClient::new(
        /*auth_manager*/ None,
        ThreadId::new(),
        crate::create_anthropic_provider(),
        crate::config::ClaudeCliConfig::default(),
        root.path().to_path_buf(),
        crate::auth::AuthCredentialsStoreMode::File,
        SessionSource::Cli,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
    );
    let mut client_session = client.new_session();
    let prompt = crate::Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
            phase: None,
        }],
        ..Default::default()
    };

    let err = match client_session
        .stream(
            &prompt,
            &test_model_info_with_slug("claude-opus-4-6"),
            root.path(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("native anthropic should reject Claude.ai OAuth"),
        Err(err) => err,
    };

    assert!(
        err.to_string()
            .contains(crate::auth::NATIVE_ANTHROPIC_OAUTH_UNSUPPORTED_MESSAGE),
        "expected native anthropic oauth failure message, got: {err}"
    );
}
