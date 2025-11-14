use codex_core::ConversationManager;
use codex_core::NewConversation;
use codex_core::protocol::EventMsg;
use codex_core::protocol::Op;
use core_test_support::load_default_config_for_test;
use core_test_support::wait_for_event;
use tempfile::TempDir;

const EXPECTED_WARNING: &str =
    "Local shell commands are disabled. Ask Codex to run commands for you.";

#[tokio::test]
async fn user_shell_cmd_requests_are_rejected() {
    let codex_home = TempDir::new().unwrap();
    let config = load_default_config_for_test(&codex_home);
    let conversation_manager =
        ConversationManager::with_auth(codex_core::CodexAuth::from_api_key("dummy"));
    let NewConversation {
        conversation: codex,
        ..
    } = conversation_manager
        .new_conversation(config)
        .await
        .expect("create new conversation");

    codex
        .submit(Op::RunUserShellCommand {
            command: "echo discouraged".to_string(),
        })
        .await
        .unwrap();

    let event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning) = event else {
        unreachable!()
    };
    assert_eq!(warning.message, EXPECTED_WARNING);
}
