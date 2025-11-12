use codex_core::ConversationManager;
use codex_core::NewConversation;
use codex_core::protocol::BackgroundShellStatus;
use codex_core::protocol::EventMsg;
use codex_core::protocol::ExecCommandEndEvent;
use codex_core::protocol::Op;
use codex_core::protocol::TurnAbortReason;
use core_test_support::load_default_config_for_test;
use core_test_support::wait_for_event;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::sleep;

fn detect_python_executable() -> Option<String> {
    let candidates = ["python3", "python"];
    candidates.iter().find_map(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .and_then(|status| status.success().then(|| (*candidate).to_string()))
    })
}

#[tokio::test]
async fn user_shell_cmd_ls_and_cat_in_temp_dir() {
    let Some(python) = detect_python_executable() else {
        eprintln!("skipping test: python3 not found in PATH");
        return;
    };

    // Create a temporary working directory with a known file.
    let cwd = TempDir::new().unwrap();
    let file_name = "hello.txt";
    let file_path: PathBuf = cwd.path().join(file_name);
    let contents = "hello from bang test\n";
    tokio::fs::write(&file_path, contents)
        .await
        .expect("write temp file");

    // Load config and pin cwd to the temp dir so ls/cat operate there.
    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    config.cwd = cwd.path().to_path_buf();

    let conversation_manager =
        ConversationManager::with_auth(codex_core::CodexAuth::from_api_key("dummy"));
    let NewConversation {
        conversation: codex,
        ..
    } = conversation_manager
        .new_conversation(config)
        .await
        .expect("create new conversation");

    // 1) python should list the file
    let list_cmd = format!(
        "{python} -c \"import pathlib; print('\\n'.join(sorted(p.name for p in pathlib.Path('.').iterdir())))\""
    );
    codex
        .submit(Op::RunUserShellCommand { command: list_cmd })
        .await
        .unwrap();
    let msg = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExecCommandEnd(_))).await;
    let EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        stdout, exit_code, ..
    }) = msg
    else {
        unreachable!()
    };
    assert_eq!(exit_code, 0);
    let listing = stdout.replace("\r\n", "\n");
    assert!(
        listing.contains(file_name),
        "ls output should include {file_name}, got: {stdout:?}"
    );

    // 2) python should print the file contents verbatim
    let cat_cmd = format!(
        "{python} -c \"import pathlib; print(pathlib.Path('{file_name}').read_text(), end='')\""
    );
    codex
        .submit(Op::RunUserShellCommand { command: cat_cmd })
        .await
        .unwrap();
    let msg = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExecCommandEnd(_))).await;
    let EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        mut stdout,
        exit_code,
        ..
    }) = msg
    else {
        unreachable!()
    };
    assert_eq!(exit_code, 0);
    // Python normalizes line endings to CRLF on some platforms; normalize for portability.
    stdout = stdout.replace("\r\n", "\n");
    assert_eq!(stdout, contents);
}

#[tokio::test]
async fn user_shell_cmd_can_be_interrupted() {
    let Some(python) = detect_python_executable() else {
        eprintln!("skipping test: python3 not found in PATH");
        return;
    };
    // Set up isolated config and conversation.
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

    // Start a long-running command and then interrupt it.
    let sleep_cmd = format!("{python} -c \"import time; time.sleep(5)\"");
    codex
        .submit(Op::RunUserShellCommand { command: sleep_cmd })
        .await
        .unwrap();

    // Wait until it has started (ExecCommandBegin), then interrupt.
    let _ = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExecCommandBegin(_))).await;
    codex.submit(Op::Interrupt).await.unwrap();

    // Expect a TurnAborted(Interrupted) notification.
    let msg = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnAborted(_))).await;
    let EventMsg::TurnAborted(ev) = msg else {
        unreachable!()
    };
    assert_eq!(ev.reason, TurnAbortReason::Interrupted);
}

#[tokio::test]
async fn user_shell_background_summary_log_and_kill() {
    let Some(python) = detect_python_executable() else {
        eprintln!("skipping test: python3 not found in PATH");
        return;
    };

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

    let script = "import sys,time;print('tick-0', flush=True);time.sleep(0.2);print('tick-1', flush=True);time.sleep(5)";
    let command = format!(
        "{python} -u -c \"{script}\" run_in_background: true bookmark=bgtest description=smoke"
    );
    codex
        .submit(Op::RunUserShellCommand {
            command: command.clone(),
        })
        .await
        .unwrap();

    // Request a summary so we can capture the shell id (retry because the background
    // manager registers the entry asynchronously).
    let entry = {
        let mut attempts = 0;
        loop {
            codex
                .submit(Op::BackgroundShellSummary { limit: Some(10) })
                .await
                .unwrap();
            let summary_event = wait_for_event(&codex, |ev| {
                matches!(ev, EventMsg::BackgroundShellSummary(_))
            })
            .await;
            let EventMsg::BackgroundShellSummary(summary) = summary_event else {
                unreachable!()
            };
            if let Some(entry) = summary
                .entries
                .iter()
                .find(|entry| entry.bookmark.as_deref() == Some("bgtest"))
            {
                break entry.clone();
            }
            attempts += 1;
            if attempts > 10 {
                panic!("background shell entry not found");
            }
            sleep(Duration::from_millis(100)).await;
        }
    };
    assert_eq!(entry.status, BackgroundShellStatus::Running);
    assert!(entry.command_preview.contains(&python));
    let shell_id = entry.shell_id.clone();

    // Poll to inspect live output.
    codex
        .submit(Op::PollBackgroundShell {
            shell_id: shell_id.clone(),
        })
        .await
        .unwrap();
    let poll_event =
        wait_for_event(&codex, |ev| matches!(ev, EventMsg::BackgroundShellPoll(_))).await;
    let EventMsg::BackgroundShellPoll(poll) = poll_event else {
        unreachable!()
    };
    assert_eq!(poll.shell_id, shell_id);
    assert_eq!(poll.status, BackgroundShellStatus::Running);
    assert!(
        poll.lines.iter().any(|line| line.contains("tick-0")),
        "poll should include stdout tail"
    );

    // Kill the shell and verify it is no longer running.
    codex
        .submit(Op::KillBackgroundShell {
            shell_id: shell_id.clone(),
        })
        .await
        .unwrap();

    // A follow-up poll should report a non-running status with exit code. Poll in a loop because
    // termination is asynchronous.
    let killed = {
        let mut attempts = 0;
        loop {
            codex
                .submit(Op::PollBackgroundShell {
                    shell_id: shell_id.clone(),
                })
                .await
                .unwrap();
            let killed_event =
                wait_for_event(&codex, |ev| matches!(ev, EventMsg::BackgroundShellPoll(_))).await;
            let EventMsg::BackgroundShellPoll(killed) = killed_event else {
                unreachable!()
            };
            if killed.status != BackgroundShellStatus::Running {
                break killed;
            }
            attempts += 1;
            if attempts > 20 {
                panic!("background shell failed to report terminal status");
            }
            sleep(Duration::from_millis(100)).await;
        }
    };
    assert_eq!(killed.shell_id, shell_id);
    assert!(killed.exit_code.is_some());

    // The summary entries should now report a terminal status for this shell id.
    let mut attempts = 0;
    loop {
        codex
            .submit(Op::BackgroundShellSummary { limit: Some(10) })
            .await
            .unwrap();
        let final_summary = wait_for_event(&codex, |ev| {
            matches!(ev, EventMsg::BackgroundShellSummary(_))
        })
        .await;
        let EventMsg::BackgroundShellSummary(final_entries) = final_summary else {
            unreachable!()
        };
        if let Some(entry) = final_entries
            .entries
            .iter()
            .find(|entry| entry.shell_id == shell_id)
        {
            assert_ne!(entry.status, BackgroundShellStatus::Running);
            break;
        }
        attempts += 1;
        if attempts > 10 {
            panic!("background shell summary never reported the killed shell");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn background_summary_marks_completion_without_manual_poll() {
    let Some(python) = detect_python_executable() else {
        eprintln!("skipping test: python3 not found in PATH");
        return;
    };

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

    let script = "import time; print('start', flush=True); time.sleep(0.2)";
    let command = format!(
        "{python} -u -c \"{script}\" run_in_background: true bookmark=autodone description=quick"
    );
    codex
        .submit(Op::RunUserShellCommand { command })
        .await
        .unwrap();

    sleep(Duration::from_millis(400)).await;

    let mut attempts = 0;
    loop {
        codex
            .submit(Op::BackgroundShellSummary { limit: Some(5) })
            .await
            .unwrap();
        let summary_event = wait_for_event(&codex, |ev| {
            matches!(ev, EventMsg::BackgroundShellSummary(_))
        })
        .await;
        let EventMsg::BackgroundShellSummary(summary) = summary_event else {
            unreachable!()
        };
        if let Some(entry) = summary
            .entries
            .iter()
            .find(|entry| entry.bookmark.as_deref() == Some("autodone"))
        {
            assert_ne!(entry.status, BackgroundShellStatus::Running);
            break;
        }
        attempts += 1;
        if attempts > 10 {
            panic!("background shell summary never reported the completed shell");
        }
        sleep(Duration::from_millis(100)).await;
    }
}
