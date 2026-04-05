use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use codex_api::common::ClaudeCodeControlResponder;
use codex_api::common::ClaudeCodeControlResponse;
use codex_api::common::ClaudeCodeControlResponseSubtype;
use codex_utils_pty::process_group::kill_child_process_group;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::claude_code_stream::ClaudeCodeStreamAccumulator;
#[cfg(test)]
use crate::claude_code_stream::completed_response_items;
use crate::config::ClaudeCliConfig;
use crate::config::ClaudeCliEffort;
use codex_protocol::models::ResponseItem;

#[derive(Debug, Clone)]
pub(crate) struct ClaudeCliRequest {
    pub(crate) cwd: PathBuf,
    pub(crate) model: String,
    pub(crate) system_prompt: String,
    pub(crate) user_prompt: String,
    pub(crate) session: ClaudeCliSession,
    pub(crate) json_schema: Option<serde_json::Value>,
    pub(crate) tools: Option<Vec<String>>,
    pub(crate) force_toolless: bool,
    pub(crate) effort: Option<ClaudeCliEffort>,
}

#[derive(Debug, Clone)]
pub(crate) enum ClaudeCliSession {
    Ephemeral,
    Persistent,
    ResumeExisting(String),
}

#[derive(Debug)]
pub(crate) struct ClaudeCodeTurnResult {
    pub(crate) output: String,
    pub(crate) session_id: Option<String>,
    pub(crate) response_items: Vec<ResponseItem>,
    pub(crate) recorded_response_items_live: bool,
}

pub(crate) struct ClaudeCliControlledStream {
    pub(crate) lines: mpsc::Receiver<anyhow::Result<String>>,
    pub(crate) control_responder: ClaudeCodeControlResponder,
}

struct ClaudeCliMcpConfigGuard {
    _tempdir: TempDir,
    path: PathBuf,
}

pub(crate) async fn run_claude_cli(
    config: &ClaudeCliConfig,
    request: ClaudeCliRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<String> {
    let executable = config
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("claude"));
    let mut command = Command::new(&executable);
    command
        .kill_on_drop(true)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("--print")
        .arg("--output-format")
        .arg("text")
        .arg("--disable-slash-commands")
        .arg("--permission-mode")
        .arg(config.permission_mode.as_cli_arg())
        .arg("--system-prompt")
        .arg(&request.system_prompt)
        .arg("--model")
        .arg(&request.model);
    match &request.session {
        ClaudeCliSession::Ephemeral => {
            command.arg("--no-session-persistence");
        }
        ClaudeCliSession::Persistent => {}
        ClaudeCliSession::ResumeExisting(session_id) => {
            command.arg("--resume").arg(session_id);
        }
    }

    apply_anthropic_runtime_auth_env(&mut command, config).await?;
    #[cfg(unix)]
    command.process_group(0);

    let effort = request.effort.or(config.effort);
    if let Some(effort) = effort {
        command.arg("--effort").arg(effort.as_cli_arg());
    }

    if request.force_toolless {
        command.arg("--tools").arg("");
    } else if let Some(tools) = request.tools.or_else(|| config.tools.clone()) {
        command.arg("--tools").arg(tools.join(","));
        for add_dir in &config.add_dirs {
            command.arg("--add-dir").arg(add_dir);
        }
    } else {
        command.arg("--tools").arg("");
    }

    if let Some(json_schema) = request.json_schema {
        command
            .arg("--json-schema")
            .arg(serde_json::to_string(&json_schema).context("serialize claude JSON schema")?);
    }
    let mcp_config_guard = create_codex_mcp_config_guard(config)?;
    if let Some(mcp_config_guard) = mcp_config_guard.as_ref() {
        command.arg("--mcp-config").arg(&mcp_config_guard.path);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "spawn Claude CLI at {} from {}",
            executable.display(),
            request.cwd.display()
        )
    })?;
    let mut stdout_reader = child.stdout.take().context("capture Claude CLI stdout")?;
    let mut stderr_reader = child.stderr.take().context("capture Claude CLI stderr")?;
    let mut stdin_writer = child.stdin.take().context("capture Claude CLI stdin")?;
    let user_prompt = request.user_prompt;
    let stdin_task = tokio::spawn(async move {
        stdin_writer.write_all(user_prompt.as_bytes()).await?;
        stdin_writer.shutdown().await
    });
    let stdout_task = tokio::spawn(async move {
        let mut stdout = Vec::new();
        stdout_reader.read_to_end(&mut stdout).await?;
        Ok::<Vec<u8>, std::io::Error>(stdout)
    });
    let stderr_task = tokio::spawn(async move {
        let mut stderr = Vec::new();
        stderr_reader.read_to_end(&mut stderr).await?;
        Ok::<Vec<u8>, std::io::Error>(stderr)
    });

    tokio::select! {
        biased;
        _ = cancellation_token.cancelled() => {
            terminate_child(&mut child).await?;
            stdin_task.abort();
            stdout_task.abort();
            stderr_task.abort();
            let _ = stdin_task.await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            anyhow::bail!("Claude CLI run cancelled")
        }
        status = child.wait() => {
            let status = status.context("wait for Claude CLI")?;
            let stdin_result = stdin_task.await.context("join Claude stdin task")?;
            stdin_result.context("write Claude CLI stdin")?;
            let stdout = stdout_task.await.context("join Claude stdout task")??;
            let stderr = stderr_task.await.context("join Claude stderr task")??;
            finalize_claude_cli_output(status, stdout, stderr)
        }
    }
}

#[cfg(test)]
pub(crate) async fn run_claude_cli_stream_json(
    config: &ClaudeCliConfig,
    request: ClaudeCliRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<mpsc::Receiver<anyhow::Result<String>>> {
    let executable = config
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("claude"));
    let mut command = Command::new(&executable);
    command
        .kill_on_drop(true)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("--print")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--include-partial-messages")
        .arg("--disable-slash-commands")
        .arg("--permission-mode")
        .arg(config.permission_mode.as_cli_arg())
        .arg("--system-prompt")
        .arg(&request.system_prompt)
        .arg("--model")
        .arg(&request.model);
    match &request.session {
        ClaudeCliSession::Ephemeral => {
            command.arg("--no-session-persistence");
        }
        ClaudeCliSession::Persistent => {}
        ClaudeCliSession::ResumeExisting(session_id) => {
            command.arg("--resume").arg(session_id);
        }
    }

    apply_anthropic_runtime_auth_env(&mut command, config).await?;
    #[cfg(unix)]
    command.process_group(0);

    let effort = request.effort.or(config.effort);
    if let Some(effort) = effort {
        command.arg("--effort").arg(effort.as_cli_arg());
    }

    if request.force_toolless {
        command.arg("--tools").arg("");
    } else if let Some(tools) = request.tools.or_else(|| config.tools.clone()) {
        command.arg("--tools").arg(tools.join(","));
        for add_dir in &config.add_dirs {
            command.arg("--add-dir").arg(add_dir);
        }
    } else {
        command.arg("--tools").arg("");
    }

    if let Some(json_schema) = request.json_schema {
        command
            .arg("--json-schema")
            .arg(serde_json::to_string(&json_schema).context("serialize claude JSON schema")?);
    }
    let mcp_config_guard = create_codex_mcp_config_guard(config)?;
    if let Some(mcp_config_guard) = mcp_config_guard.as_ref() {
        command.arg("--mcp-config").arg(&mcp_config_guard.path);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "spawn Claude CLI at {} from {}",
            executable.display(),
            request.cwd.display()
        )
    })?;
    let stdout = child.stdout.take().context("capture Claude CLI stdout")?;
    let mut stderr_reader = child.stderr.take().context("capture Claude CLI stderr")?;
    let mut stdin_writer = child.stdin.take().context("capture Claude CLI stdin")?;
    let user_prompt = request.user_prompt;
    let (tx_line, rx_line) = mpsc::channel(1600);

    tokio::spawn(async move {
        let _mcp_config_guard = mcp_config_guard;
        let stdin_task = tokio::spawn(async move {
            stdin_writer.write_all(user_prompt.as_bytes()).await?;
            stdin_writer.shutdown().await
        });
        let stderr_task = tokio::spawn(async move {
            let mut stderr = Vec::new();
            stderr_reader.read_to_end(&mut stderr).await?;
            Ok::<Vec<u8>, std::io::Error>(stderr)
        });
        let mut lines = BufReader::new(stdout).lines();

        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    let _ = terminate_child(&mut child).await;
                    stdin_task.abort();
                    stderr_task.abort();
                    let _ = tx_line.send(Err(anyhow::anyhow!("Claude CLI run cancelled"))).await;
                    return;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if is_permission_request_line(trimmed) {
                                let _ = terminate_child(&mut child).await;
                                let _ = tx_line.send(Err(anyhow::anyhow!(
                                    "Claude Code carrier requested an interactive permission decision that Claudex has not bridged yet"
                                ))).await;
                                return;
                            }
                            if tx_line.send(Ok(line)).await.is_err() {
                                let _ = terminate_child(&mut child).await;
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(err) => {
                            let _ = tx_line.send(Err(err.into())).await;
                            return;
                        }
                    }
                }
            }
        }

        let status = match child.wait().await {
            Ok(status) => status,
            Err(err) => {
                let _ = tx_line.send(Err(err.into())).await;
                return;
            }
        };
        let stdin_result = stdin_task.await;
        let stderr = match stderr_task.await {
            Ok(Ok(stderr)) => stderr,
            Ok(Err(err)) => {
                let _ = tx_line.send(Err(err.into())).await;
                return;
            }
            Err(err) => {
                let _ = tx_line.send(Err(err.into())).await;
                return;
            }
        };
        if let Ok(Err(err)) = stdin_result {
            let _ = tx_line.send(Err(err.into())).await;
            return;
        }
        if !status.success() {
            let stdout_summary = String::new();
            let stderr_summary = String::from_utf8_lossy(&stderr).trim().to_string();
            let message = if stderr_summary.is_empty() {
                stdout_summary
            } else {
                stderr_summary
            };
            let _ = tx_line
                .send(Err(anyhow::anyhow!("Claude CLI failed: {message}")))
                .await;
        }
    });

    Ok(rx_line)
}

pub(crate) async fn run_claude_cli_stream_json_controlled(
    config: &ClaudeCliConfig,
    request: ClaudeCliRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<ClaudeCliControlledStream> {
    let executable = config
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("claude"));
    let mut command = Command::new(&executable);
    command
        .kill_on_drop(true)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("--print")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--include-partial-messages")
        .arg("--disable-slash-commands")
        .arg("--permission-mode")
        .arg(config.permission_mode.as_cli_arg())
        .arg("--system-prompt")
        .arg(&request.system_prompt)
        .arg("--model")
        .arg(&request.model);
    match &request.session {
        ClaudeCliSession::Ephemeral => {
            command.arg("--no-session-persistence");
        }
        ClaudeCliSession::Persistent => {}
        ClaudeCliSession::ResumeExisting(session_id) => {
            command.arg("--resume").arg(session_id);
        }
    }

    apply_anthropic_runtime_auth_env(&mut command, config).await?;
    #[cfg(unix)]
    command.process_group(0);

    let effort = request.effort.or(config.effort);
    if let Some(effort) = effort {
        command.arg("--effort").arg(effort.as_cli_arg());
    }

    if request.force_toolless {
        command.arg("--tools").arg("");
    } else if let Some(tools) = request.tools.or_else(|| config.tools.clone()) {
        command.arg("--tools").arg(tools.join(","));
        for add_dir in &config.add_dirs {
            command.arg("--add-dir").arg(add_dir);
        }
    } else {
        command.arg("--tools").arg("");
    }

    if let Some(json_schema) = request.json_schema {
        command
            .arg("--json-schema")
            .arg(serde_json::to_string(&json_schema).context("serialize claude JSON schema")?);
    }
    let mcp_config_guard = create_codex_mcp_config_guard(config)?;
    if let Some(mcp_config_guard) = mcp_config_guard.as_ref() {
        command.arg("--mcp-config").arg(&mcp_config_guard.path);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "spawn Claude CLI at {} from {}",
            executable.display(),
            request.cwd.display()
        )
    })?;
    let stdout = child.stdout.take().context("capture Claude CLI stdout")?;
    let mut stderr_reader = child.stderr.take().context("capture Claude CLI stderr")?;
    let stdin_writer = child.stdin.take().context("capture Claude CLI stdin")?;
    let (tx_line, rx_line) = mpsc::channel(1600);
    let (tx_control, mut rx_control) = mpsc::channel::<ClaudeCodeControlResponse>(64);
    let initial_user_message = build_stream_json_user_message(&request.user_prompt);
    let stdin_shutdown = CancellationToken::new();

    tokio::spawn(async move {
        let _mcp_config_guard = mcp_config_guard;
        let writer_shutdown = stdin_shutdown.clone();
        let writer_task = tokio::spawn(async move {
            let mut stdin_writer = stdin_writer;
            write_stream_json_line(&mut stdin_writer, &initial_user_message).await?;
            loop {
                tokio::select! {
                    _ = writer_shutdown.cancelled() => break,
                    response = rx_control.recv() => match response {
                        Some(response) => {
                            let payload = build_control_response_line(response);
                            write_stream_json_line(&mut stdin_writer, &payload).await?;
                        }
                        None => break,
                    },
                }
            }
            stdin_writer
                .shutdown()
                .await
                .context("shutdown Claude CLI stdin")?;
            Ok::<(), anyhow::Error>(())
        });
        let stderr_task = tokio::spawn(async move {
            let mut stderr = Vec::new();
            stderr_reader.read_to_end(&mut stderr).await?;
            Ok::<Vec<u8>, std::io::Error>(stderr)
        });
        let mut lines = BufReader::new(stdout).lines();

        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    let _ = terminate_child(&mut child).await;
                    writer_task.abort();
                    stderr_task.abort();
                    let _ = tx_line.send(Err(anyhow::anyhow!("Claude CLI run cancelled"))).await;
                    return;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            let closes_stdin = is_terminal_result_line(&line);
                            if tx_line.send(Ok(line)).await.is_err() {
                                let _ = terminate_child(&mut child).await;
                                writer_task.abort();
                                stderr_task.abort();
                                return;
                            }
                            if closes_stdin {
                                stdin_shutdown.cancel();
                            }
                        }
                        Ok(None) => {
                            stdin_shutdown.cancel();
                            break;
                        }
                        Err(err) => {
                            let _ = tx_line.send(Err(err.into())).await;
                            writer_task.abort();
                            stderr_task.abort();
                            return;
                        }
                    }
                }
            }
        }

        let status = match child.wait().await {
            Ok(status) => status,
            Err(err) => {
                let _ = tx_line.send(Err(err.into())).await;
                writer_task.abort();
                stderr_task.abort();
                return;
            }
        };
        writer_task.abort();
        let stderr = match stderr_task.await {
            Ok(Ok(stderr)) => stderr,
            Ok(Err(err)) => {
                let _ = tx_line.send(Err(err.into())).await;
                return;
            }
            Err(err) => {
                let _ = tx_line.send(Err(err.into())).await;
                return;
            }
        };
        if !status.success() {
            let stderr_summary = String::from_utf8_lossy(&stderr).trim().to_string();
            let message = if stderr_summary.is_empty() {
                "Claude CLI failed".to_string()
            } else {
                stderr_summary
            };
            let _ = tx_line
                .send(Err(anyhow::anyhow!("Claude CLI failed: {message}")))
                .await;
        }
    });

    Ok(ClaudeCliControlledStream {
        lines: rx_line,
        control_responder: ClaudeCodeControlResponder::new(tx_control),
    })
}

#[cfg(test)]
pub(crate) async fn run_claude_code_turn(
    config: &ClaudeCliConfig,
    request: ClaudeCliRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<ClaudeCodeTurnResult> {
    let mut raw_lines = run_claude_cli_stream_json(config, request, cancellation_token).await?;
    let mut accumulator = ClaudeCodeStreamAccumulator::default();
    let mut response_items = Vec::new();
    while let Some(line) = raw_lines.recv().await {
        match line {
            Ok(line) => {
                let events = accumulator.push_line(&line)?;
                response_items.extend(completed_response_items(&events));
            }
            Err(err) => return Err(err),
        }
    }
    let summary = accumulator.finish();
    if summary.assistant_text.trim().is_empty() && response_items.is_empty() {
        anyhow::bail!("Claude Code returned empty output");
    }
    Ok(ClaudeCodeTurnResult {
        output: summary.assistant_text,
        session_id: summary.session_id,
        response_items,
        recorded_response_items_live: false,
    })
}

#[cfg(test)]
fn is_permission_request_line(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some("control_request")
}

fn build_stream_json_user_message(user_prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": user_prompt,
        },
        "parent_tool_use_id": null,
        "session_id": "",
    })
}

fn create_codex_mcp_config_guard(
    config: &ClaudeCliConfig,
) -> anyhow::Result<Option<ClaudeCliMcpConfigGuard>> {
    let Some(codex_self_exe) = config.codex_self_exe.as_ref() else {
        return Ok(None);
    };
    let tempdir = tempfile::tempdir().context("create Claude MCP config tempdir")?;
    let path = tempdir.path().join("claude-codex-mcp.json");
    let config_json = serde_json::json!({
        "mcpServers": {
            "codex": {
                "type": "stdio",
                "command": codex_self_exe,
                "args": ["mcp-server"]
            }
        }
    });
    std::fs::write(
        &path,
        serde_json::to_vec(&config_json).context("serialize Claude MCP config")?,
    )
    .context("write Claude MCP config")?;
    Ok(Some(ClaudeCliMcpConfigGuard {
        _tempdir: tempdir,
        path,
    }))
}

fn build_control_response_line(response: ClaudeCodeControlResponse) -> serde_json::Value {
    let response_body = match response.subtype {
        ClaudeCodeControlResponseSubtype::Allow { updated_input } => {
            let mut body = serde_json::Map::new();
            body.insert(
                "behavior".to_string(),
                serde_json::Value::String("allow".to_string()),
            );
            if let Some(updated_input) = updated_input {
                body.insert("updatedInput".to_string(), updated_input);
            }
            serde_json::Value::Object(body)
        }
        ClaudeCodeControlResponseSubtype::Deny { message } => serde_json::json!({
            "behavior": "deny",
            "message": message,
        }),
    };
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": response.request_id,
            "response": response_body,
        },
    })
}

fn is_terminal_result_line(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        == Some("result")
}

async fn write_stream_json_line(
    stdin_writer: &mut tokio::process::ChildStdin,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let line = serde_json::to_string(value).context("serialize Claude stream-json stdin")?;
    stdin_writer
        .write_all(line.as_bytes())
        .await
        .context("write Claude stream-json stdin line")?;
    stdin_writer
        .write_all(b"\n")
        .await
        .context("write Claude stream-json stdin newline")?;
    stdin_writer.flush().await.context("flush Claude CLI stdin")
}

async fn apply_anthropic_runtime_auth_env(
    command: &mut Command,
    config: &ClaudeCliConfig,
) -> anyhow::Result<()> {
    let Some(auth_home) = config.auth_home.as_deref() else {
        return Ok(());
    };
    command.env("CLAUDE_CONFIG_DIR", auth_home);
    match crate::auth::resolve_anthropic_runtime_auth(auth_home, config.auth_credentials_store_mode)
        .await
    {
        Ok(Some(crate::auth::AnthropicRuntimeAuth::ApiKey(api_key))) => {
            command.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
            command.env_remove("ANTHROPIC_AUTH_TOKEN");
            command.env("ANTHROPIC_API_KEY", api_key);
        }
        Ok(Some(crate::auth::AnthropicRuntimeAuth::OauthAccessToken(access_token))) => {
            command.env_remove("ANTHROPIC_API_KEY");
            command.env_remove("ANTHROPIC_AUTH_TOKEN");
            command.env("CLAUDE_CODE_OAUTH_TOKEN", access_token);
        }
        Ok(None) => {
            command.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
            command.env_remove("ANTHROPIC_AUTH_TOKEN");
            command.env_remove("ANTHROPIC_API_KEY");
        }
        Err(err) => {
            anyhow::bail!("resolve Anthropic auth for Claude CLI: {err}");
        }
    }
    Ok(())
}

fn finalize_claude_cli_output(
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> anyhow::Result<String> {
    let stdout = String::from_utf8_lossy(&stdout).trim().to_string();
    if status.success() {
        if stdout.is_empty() {
            anyhow::bail!("Claude CLI returned empty output")
        }
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    let message = if stderr.is_empty() {
        stdout
    } else if stdout.is_empty() {
        stderr
    } else {
        format!("{stderr}\n{stdout}")
    };
    anyhow::bail!("Claude CLI failed: {message}")
}

async fn terminate_child(child: &mut tokio::process::Child) -> anyhow::Result<()> {
    kill_child_process_group(child).context("kill Claude CLI process group")?;
    child.start_kill().context("kill Claude CLI")?;
    let _ = child.wait().await;
    Ok(())
}
