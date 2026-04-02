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
            "#!/usr/bin/env bash\nset -euo pipefail\nstdin_payload=$(cat)\nprintf '%s' \"$stdin_payload\" > '{}'\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'claude-main-ok'\n",
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

    let err = match stream_result {
        Ok(_stream) => panic!("Claude stream should abort on cancellation"),
        Err(err) => err,
    };

    let crate::error::CodexErr::Stream(message, _response_id) = err else {
        panic!("expected stream error, got {err:?}");
    };
    assert_eq!(message, "Claude CLI run cancelled");
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
