use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use codex_utils_pty::process_group::kill_child_process_group;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::ClaudeCliConfig;
use crate::config::ClaudeCliEffort;

#[derive(Debug, Clone)]
pub(crate) struct ClaudeCliRequest {
    pub(crate) cwd: PathBuf,
    pub(crate) model: String,
    pub(crate) system_prompt: String,
    pub(crate) user_prompt: String,
    pub(crate) json_schema: Option<serde_json::Value>,
    pub(crate) tools: Option<Vec<String>>,
    pub(crate) force_toolless: bool,
    pub(crate) effort: Option<ClaudeCliEffort>,
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
        .arg("--no-session-persistence")
        .arg("--disable-slash-commands")
        .arg("--permission-mode")
        .arg(config.permission_mode.as_cli_arg())
        .arg("--system-prompt")
        .arg(&request.system_prompt)
        .arg("--model")
        .arg(&request.model);
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
