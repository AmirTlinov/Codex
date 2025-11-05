#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;

use anyhow::Result;
use codex_core::features::Feature;
use codex_core::protocol::AskForApproval;
use codex_core::protocol::BackgroundEventEvent;
use codex_core::protocol::EventMsg;
use codex_core::protocol::Op;
use codex_core::protocol::SandboxPolicy;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use serde_json::Value;
use serde_json::json;
use tokio::time::Duration;
use tokio::time::sleep;

fn collect_tool_outputs(bodies: &[Value]) -> Result<HashMap<String, Value>> {
    let mut outputs = HashMap::new();
    for body in bodies {
        if let Some(items) = body.get("input").and_then(Value::as_array) {
            for item in items {
                if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
                    continue;
                }
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    let content = item
                        .get("output")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing tool output content for {call_id}")
                        })?
                        .trim();
                    if content.is_empty() {
                        continue;
                    }
                    let parsed: Value = serde_json::from_str(content).map_err(|err| {
                        anyhow::anyhow!("failed to parse tool output content {content:?}: {err}")
                    })?;
                    outputs.insert(call_id.to_string(), parsed);
                }
            }
        }
    }
    Ok(outputs)
}

async fn submit_background_turn(test: &TestCodex, prompt: &str) -> Result<()> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TaskComplete(_))
    })
    .await;
    Ok(())
}

async fn submit_foreground_turn(test: &TestCodex, prompt: &str) -> Result<()> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
        })
        .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_shell_supports_polling_and_kill() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_config(|config| {
        config.use_experimental_unified_exec_tool = true;
        config.features.enable(Feature::UnifiedExec);
    });
    let test = builder.build(&server).await?;

    let start_call_id = "bg-shell-start";
    let poll_call_id = "bg-shell-poll";
    let kill_call_id = "bg-shell-kill";

    let start_args = json!({
        "command": [
            "/bin/sh",
            "-c",
            "printf 'start\\n'; sleep 0.35; printf 'mid\\n'; sleep 0.35"
        ],
        "timeout_ms": 5_000,
        "run_in_background": true,
        "description": "demo background shell",
    });
    let poll_args = json!({
        "bash_id": "shell_0",
    });
    let kill_args = json!({
        "shell_id": "shell_0",
    });

    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(start_call_id, "shell", &serde_json::to_string(&start_args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_response_created("resp-2"),
            ev_function_call(
                poll_call_id,
                "bash_output",
                &serde_json::to_string(&poll_args)?,
            ),
            ev_completed("resp-2"),
        ]),
        sse(vec![
            ev_response_created("resp-3"),
            ev_function_call(
                kill_call_id,
                "kill_shell",
                &serde_json::to_string(&kill_args)?,
            ),
            ev_completed("resp-3"),
        ]),
        sse(vec![
            ev_response_created("resp-4"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-4"),
        ]),
    ];
    mount_sse_sequence(&server, responses).await;

    submit_background_turn(&test, "exercise background shell tools").await?;

    let start_event = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::BackgroundEvent(BackgroundEventEvent { message })
            if message.contains("shell shell_0 started") =>
        {
            Some(message.clone())
        }
        _ => None,
    })
    .await;
    assert!(start_event.contains("demo background shell"));

    let kill_event = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::BackgroundEvent(BackgroundEventEvent { message })
            if message.contains("shell shell_0 terminated") =>
        {
            Some(message.clone())
        }
        _ => None,
    })
    .await;
    assert!(kill_event.contains("demo background shell"));

    let requests = server
        .received_requests()
        .await
        .expect("recorded requests present");
    let bodies: Vec<Value> = requests
        .iter()
        .map(|req| serde_json::from_slice(&req.body))
        .collect::<Result<_, _>>()?;
    let outputs = collect_tool_outputs(&bodies)?;

    let start_output = outputs
        .get(start_call_id)
        .expect("start output missing")
        .clone();
    assert_eq!(
        start_output.get("shell_id"),
        Some(&Value::String("shell_0".into()))
    );
    assert_eq!(start_output.get("running"), Some(&Value::Bool(true)));
    assert_eq!(
        start_output.get("description"),
        Some(&Value::String("demo background shell".into()))
    );
    let initial_output = start_output
        .get("initial_output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(initial_output.contains("start"));

    let poll_output = outputs
        .get(poll_call_id)
        .expect("poll output missing")
        .clone();
    assert_eq!(
        poll_output.get("shell_id"),
        Some(&Value::String("shell_0".into()))
    );
    assert_eq!(poll_output.get("running"), Some(&Value::Bool(true)));
    let polled_text = poll_output
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(polled_text.contains("mid"));

    let kill_output = outputs
        .get(kill_call_id)
        .expect("kill output missing")
        .clone();
    assert_eq!(
        kill_output.get("shell_id"),
        Some(&Value::String("shell_0".into()))
    );
    assert_eq!(
        kill_output.get("description"),
        Some(&Value::String("demo background shell".into()))
    );
    assert!(kill_output.get("exit_code").is_some());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_foreground_shell_emits_shell_promoted_event() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_config(|config| {
        config.use_experimental_unified_exec_tool = true;
        config.features.enable(Feature::UnifiedExec);
    });
    let test = builder.build(&server).await?;

    let call_id = "fg-shell-call";
    let raw_description = "  npm run dev  ";
    let trimmed_description = "npm run dev";
    let shell_args = json!({
        "command": [
            "/bin/sh",
            "-c",
            "printf 'kickoff\n'; sleep 0.2; printf 'done\n'"
        ],
        "timeout_ms": 10_000,
        "description": raw_description,
    });

    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell", &serde_json::to_string(&shell_args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_response_created("resp-2"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    ];
    mount_sse_sequence(&server, responses).await;

    submit_foreground_turn(&test, "launch foreground command").await?;

    let _ = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::ExecCommandBegin(ev) if ev.call_id == call_id => Some(()),
        _ => None,
    })
    .await;

    test.codex
        .submit(Op::PromoteShell {
            call_id: call_id.to_string(),
            description: Some(raw_description.to_string()),
        })
        .await?;

    let (shell_id, initial_output, description) =
        wait_for_event_match(&test.codex, |msg| match msg {
            EventMsg::ShellPromoted {
                call_id: event_call_id,
                shell_id,
                initial_output,
                description,
            } if event_call_id == call_id => Some((
                shell_id.clone(),
                initial_output.clone(),
                description.clone(),
            )),
            _ => None,
        })
        .await;

    assert_eq!(description.as_deref(), Some(trimmed_description));
    assert!(initial_output.contains("kickoff"));

    let started_message = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::BackgroundEvent(BackgroundEventEvent { message })
            if message.contains("promoted from foreground") =>
        {
            Some(message.clone())
        }
        _ => None,
    })
    .await;
    assert!(started_message.contains(trimmed_description));

    sleep(Duration::from_millis(300)).await;

    test.codex
        .submit(Op::PollBackgroundShell {
            shell_id: shell_id.clone(),
        })
        .await?;

    let poll_output = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::ExecCommandOutputDelta(delta) if delta.call_id == call_id => {
            String::from_utf8(delta.chunk.clone()).ok()
        }
        _ => None,
    })
    .await;
    assert!(poll_output.contains("done"));

    let terminated_message = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::BackgroundEvent(BackgroundEventEvent { message })
            if message.contains("terminated with exit code") =>
        {
            Some(message.clone())
        }
        _ => None,
    })
    .await;
    assert!(terminated_message.contains(&shell_id));

    let requests = server
        .received_requests()
        .await
        .expect("recorded requests present");
    let bodies: Vec<Value> = requests
        .iter()
        .map(|req| serde_json::from_slice(&req.body))
        .collect::<Result<_, _>>()?;
    let outputs = collect_tool_outputs(&bodies)?;
    let shell_output = outputs.get(call_id).expect("shell output missing").clone();

    assert_eq!(
        shell_output.get("shell_id"),
        Some(&Value::String(shell_id.clone()))
    );
    assert_eq!(shell_output.get("running"), Some(&Value::Bool(true)));
    assert_eq!(
        shell_output.get("description"),
        Some(&Value::String(trimmed_description.into()))
    );
    let initial_json = shell_output
        .get("initial_output")
        .and_then(Value::as_str)
        .expect("initial output missing");
    assert!(initial_json.contains("kickoff"));

    Ok(())
}
