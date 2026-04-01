use super::*;
use crate::CodexAuth;
use crate::ThreadManager;
use crate::config::AgentBackend;
use crate::config::AgentRoleConfig;
use crate::config::ClaudeCliConfig;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::config_loader::LoaderOverrides;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

fn sample_report(disposition: model::ReflectiveDisposition) -> prompt::ReflectiveReport {
    prompt::ReflectiveReport {
        observations: vec![model::ReflectiveObservation {
            category: model::ReflectiveObservationCategory::Risk,
            note: "Check non-obvious race".to_string(),
            why_it_matters: "Late maintenance results can overwrite a newer truth".to_string(),
            evidence: "Result application happens asynchronously after the main turn".to_string(),
            confidence: model::ReflectiveConfidence::High,
            disposition,
        }],
    }
}

#[test]
fn should_schedule_after_regular_turn_requires_signal() {
    assert!(!should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 0,
        /*turn_total_tokens*/ 10,
        Some("done"),
    ));
    assert!(should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 1,
        /*turn_total_tokens*/ 10,
        Some("done"),
    ));
    assert!(should_schedule_after_regular_turn(
        /*feature_enabled*/ true,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 0,
        /*turn_total_tokens*/ 3_500,
        Some("done"),
    ));
    assert!(!should_schedule_after_regular_turn(
        /*feature_enabled*/ false,
        &SessionSource::Cli,
        /*turn_tool_calls*/ 1,
        /*turn_total_tokens*/ 3_500,
        Some("done"),
    ));
}

#[test]
fn reflective_window_drops_discarded_observations() {
    let window = ReflectiveWindowState::from_report(
        "turn-1".to_string(),
        sample_report(model::ReflectiveDisposition::Discard),
    );
    assert_eq!(window, None);
}

#[test]
fn reflective_window_into_prompt_item_uses_reflective_fragment() {
    let window = ReflectiveWindowState::from_report(
        "turn-1".to_string(),
        sample_report(model::ReflectiveDisposition::Verify),
    )
    .expect("window");

    let item = window.into_prompt_item();
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message");
    };
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected input text");
    };
    assert!(text.contains("<reflective_window>"));
    assert!(text.contains("Check non-obvious race"));
}

struct ReflectiveMockClaude {
    script_path: PathBuf,
    stdin_log_path: PathBuf,
}

fn mock_reflective_claude(root: &TempDir) -> ReflectiveMockClaude {
    let script_path = root.path().join("mock-reflective-claude.sh");
    let stdin_log_path = root.path().join("reflective.stdin.log");
    std::fs::write(
        &script_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nstdin_payload=$(cat)\nprintf '%s' \"$stdin_payload\" > '{}'\nprintf '%s' '{{\"observations\":[{{\"category\":\"risk\",\"note\":\"Bounded reflective note\",\"why_it_matters\":\"Keeps Claude reflective passes usable on long sessions\",\"evidence\":\"mock reflective claude\",\"confidence\":\"high\",\"disposition\":\"verify\"}}]}}'\n",
            stdin_log_path.display(),
        ),
    )
    .expect("write mock reflective Claude");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("mock reflective Claude metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("chmod mock reflective Claude");
    }
    ReflectiveMockClaude {
        script_path,
        stdin_log_path,
    }
}

async fn reflective_test_config(home: &TempDir) -> Config {
    ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(LoaderOverrides {
            #[cfg(target_os = "macos")]
            managed_preferences_base64: Some(String::new()),
            macos_managed_config_requirements_base64: Some(String::new()),
            ..LoaderOverrides::default()
        })
        .build()
        .await
        .expect("load reflective test config")
}

#[tokio::test]
async fn reflective_claude_sidecar_uses_selected_role_and_bounds_transcript() {
    let home = TempDir::new().expect("create temp dir");
    let mock_claude = mock_reflective_claude(&home);
    let role_path = home.path().join("claude-reflector.toml");
    tokio::fs::write(
        &role_path,
        format!(
            r#"
agent_backend = "claude_cli"
model = "claude-opus-4-6"
[claude_cli]
path = "{}"
"#,
            mock_claude.script_path.display()
        ),
    )
    .await
    .expect("write reflective role");

    let mut config = reflective_test_config(&home).await;
    config.reflective_window_agent_type = Some("claude_reflector".to_string());
    config.claude_cli = ClaudeCliConfig::default();
    config.agent_roles.insert(
        "claude_reflector".to_string(),
        AgentRoleConfig {
            description: Some("Claude reflective sidecar".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
        },
    );

    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        CodexAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.codex_home.clone(),
        Arc::new(codex_exec_server::EnvironmentManager::new(
            /*exec_server_url*/ None,
        )),
    );
    let thread = manager
        .start_thread(config.clone())
        .await
        .expect("start reflective test thread")
        .thread;

    for turn in 1..=14 {
        thread
            .inject_user_message_without_turn(format!("user-turn-{turn:02}"))
            .await;
    }

    let turn_context = thread.codex.session.new_default_turn().await;
    let spawn_config = build_reflective_spawn_config(turn_context.as_ref())
        .await
        .expect("build reflective spawn config");
    assert_eq!(spawn_config.agent_backend, AgentBackend::ClaudeCli);
    assert_eq!(spawn_config.model.as_deref(), Some("claude-opus-4-6"));

    let report = run_reflective_sidecar_claude(thread.codex.session.as_ref(), &spawn_config)
        .await
        .expect("run reflective Claude sidecar")
        .expect("reflective report");
    assert_eq!(report.observations.len(), 1);

    let stdin_log =
        std::fs::read_to_string(&mock_claude.stdin_log_path).expect("read reflective stdin log");
    assert!(stdin_log.contains("user-turn-14"));
    assert!(stdin_log.contains("user-turn-03"));
    assert!(!stdin_log.contains("user-turn-01"));
    assert!(!stdin_log.contains("user-turn-02"));
    assert!(stdin_log.contains("<transcript_omission>"));
}
