use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::time::Instant as StdInstant;

use anyhow::Error as AnyhowError;

use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::Instant as TokioInstant;
use tracing::warn;

use crate::exec::ExecToolCallOutput;
use crate::exec::SandboxType;
use crate::exec::StreamOutput;
use crate::exec_env::create_env;
use crate::sandboxing::ExecEnv;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::events::ToolEventStage;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::runtimes::unified_exec::UnifiedExecRequest as UnifiedExecToolRequest;
use crate::tools::runtimes::unified_exec::UnifiedExecRuntime;
use crate::tools::sandboxing::ToolCtx;

use super::DEFAULT_TIMEOUT_MS;
use super::ExecCommandRequest;
use super::MAX_TIMEOUT_MS;
use super::MIN_YIELD_TIME_MS;
use super::SessionEntry;
use super::UnifiedExecContext;
use super::UnifiedExecError;
use super::UnifiedExecKillResult;
use super::UnifiedExecOutputWindow;
use super::UnifiedExecRequest;
use super::UnifiedExecResponse;
use super::UnifiedExecResult;
use super::UnifiedExecSessionOutput;
use super::UnifiedExecSessionSnapshot;
use super::WriteStdinRequest;
use super::clamp_yield_time;
use super::generate_chunk_id;
use super::resolve_max_tokens;
use super::session::OutputBuffer;
use super::session::UnifiedExecSession;
use super::truncate_output_to_tokens;

#[derive(Debug, Default)]
pub struct UnifiedExecSessionManager {
    next_session_id: AtomicI32,
    sessions: Mutex<HashMap<i32, SessionEntry>>,
}

impl UnifiedExecSessionManager {
    pub(crate) async fn handle_request(
        &self,
        request: UnifiedExecRequest<'_>,
    ) -> Result<UnifiedExecResult, UnifiedExecError> {
        let (timeout_ms, timeout_warning) = match request.timeout_ms {
            Some(requested) if requested > MAX_TIMEOUT_MS => (
                MAX_TIMEOUT_MS,
                Some(format!(
                    "Warning: requested timeout {requested}ms exceeds maximum of {MAX_TIMEOUT_MS}ms; clamping to {MAX_TIMEOUT_MS}ms.\n"
                )),
            ),
            Some(requested) => (requested, None),
            None => (DEFAULT_TIMEOUT_MS, None),
        };

        if let Some(session_id) = request.session_id {
            let (writer_tx, output_buffer, output_notify) =
                self.prepare_session_handles(session_id).await?;

            let joined_input = request.input_chunks.join(" ");
            if !joined_input.is_empty() {
                Self::send_input(&writer_tx, joined_input.as_bytes()).await?;
            }

            let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
            let collected =
                Self::collect_output_until_deadline(&output_buffer, &output_notify, deadline).await;
            let mut output = String::from_utf8_lossy(&collected).to_string();
            if let Some(warning) = timeout_warning {
                output = format!("{warning}{output}");
            }

            let status = self.refresh_session_state(session_id).await;
            let next_session_id = match status {
                SessionStatus::Alive { .. } => Some(session_id),
                SessionStatus::Exited { .. } => None,
                SessionStatus::Unknown => {
                    return Err(UnifiedExecError::UnknownSessionId { session_id });
                }
            };

            return Ok(UnifiedExecResult {
                session_id: next_session_id,
                output,
            });
        }

        if request.input_chunks.is_empty() {
            return Err(UnifiedExecError::MissingCommandLine);
        }

        let command = request.input_chunks.to_vec();
        let (program, args) = command
            .split_first()
            .ok_or(UnifiedExecError::MissingCommandLine)?;

        let cwd = env::current_dir().map_err(|err| UnifiedExecError::create_session(err.into()))?;
        let env_map: HashMap<String, String> = env::vars().collect();

        let spawned = codex_utils_pty::spawn_pty_process(program, args, cwd.as_path(), &env_map)
            .await
            .map_err(UnifiedExecError::create_session)?;
        let session =
            UnifiedExecSession::from_spawned(command.clone(), spawned, SandboxType::None).await?;

        let (output_buffer, output_notify) = session.output_handles();
        let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
        let collected =
            Self::collect_output_until_deadline(&output_buffer, &output_notify, deadline).await;
        let mut output = String::from_utf8_lossy(&collected).to_string();
        if let Some(warning) = timeout_warning {
            output = format!("{warning}{output}");
        }

        let session_id = if session.has_exited() {
            None
        } else {
            Some(
                self.store_session_without_context(session, command, cwd)
                    .await,
            )
        };

        Ok(UnifiedExecResult { session_id, output })
    }

    pub(crate) async fn exec_command(
        &self,
        request: ExecCommandRequest<'_>,
        context: &UnifiedExecContext,
    ) -> Result<UnifiedExecResponse, UnifiedExecError> {
        let shell_flag = if request.login { "-lc" } else { "-c" };
        let command = vec![
            request.shell.to_string(),
            shell_flag.to_string(),
            request.command.to_string(),
        ];

        let session = self.open_session_with_sandbox(command, context).await?;

        let max_tokens = resolve_max_tokens(request.max_output_tokens);
        let yield_time_ms =
            clamp_yield_time(Some(request.yield_time_ms.unwrap_or(MIN_YIELD_TIME_MS)));

        let start = StdInstant::now();
        let (output_buffer, output_notify) = session.output_handles();
        let deadline = TokioInstant::now() + Duration::from_millis(yield_time_ms);
        let collected =
            Self::collect_output_until_deadline(&output_buffer, &output_notify, deadline).await;
        let wall_time = StdInstant::now().saturating_duration_since(start);

        let text = String::from_utf8_lossy(&collected).to_string();
        let (output, original_token_count) = truncate_output_to_tokens(&text, max_tokens);
        let chunk_id = generate_chunk_id();
        let exit_code = session.exit_code();
        let session_id = if session.has_exited() {
            None
        } else {
            Some(
                self.store_session(session, context, request.command, start)
                    .await,
            )
        };

        let response = UnifiedExecResponse {
            event_call_id: context.call_id.clone(),
            chunk_id,
            wall_time,
            output,
            session_id,
            exit_code,
            original_token_count,
        };

        // If the command completed during this call, emit an ExecCommandEnd via the emitter.
        if response.session_id.is_none() {
            let exit = response.exit_code.unwrap_or(-1);
            Self::emit_exec_end_from_context(
                context,
                request.command.to_string(),
                response.output.clone(),
                exit,
                response.wall_time,
            )
            .await;
        }

        Ok(response)
    }

    pub(crate) async fn write_stdin(
        &self,
        request: WriteStdinRequest<'_>,
    ) -> Result<UnifiedExecResponse, UnifiedExecError> {
        let session_id = request.session_id;

        let (writer_tx, output_buffer, output_notify) =
            self.prepare_session_handles(session_id).await?;

        if !request.input.is_empty() {
            Self::send_input(&writer_tx, request.input.as_bytes()).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let max_tokens = resolve_max_tokens(request.max_output_tokens);
        let yield_time_ms = clamp_yield_time(request.yield_time_ms);
        let start = StdInstant::now();
        let deadline = TokioInstant::now() + Duration::from_millis(yield_time_ms);
        let collected =
            Self::collect_output_until_deadline(&output_buffer, &output_notify, deadline).await;
        let wall_time = StdInstant::now().saturating_duration_since(start);

        let text = String::from_utf8_lossy(&collected).to_string();
        let (output, original_token_count) = truncate_output_to_tokens(&text, max_tokens);
        let chunk_id = generate_chunk_id();

        let status = self.refresh_session_state(session_id).await;
        let (session_id, exit_code, completion_entry, event_call_id) = match status {
            SessionStatus::Alive { exit_code, call_id } => (
                Some(session_id),
                exit_code,
                None,
                call_id.unwrap_or_default(),
            ),
            SessionStatus::Exited { exit_code, entry } => {
                let call_id = entry.call_id.clone().unwrap_or_default();
                (None, exit_code, Some(*entry), call_id)
            }
            SessionStatus::Unknown => {
                return Err(UnifiedExecError::UnknownSessionId { session_id });
            }
        };

        let response = UnifiedExecResponse {
            event_call_id,
            chunk_id,
            wall_time,
            output,
            session_id,
            exit_code,
            original_token_count,
        };

        if let (Some(exit), Some(entry)) = (response.exit_code, completion_entry) {
            let total_duration = StdInstant::now().saturating_duration_since(entry.started_at);
            Self::emit_exec_end_from_entry(entry, response.output.clone(), exit, total_duration)
                .await;
        }

        Ok(response)
    }

    async fn refresh_session_state(&self, session_id: i32) -> SessionStatus {
        let mut sessions = self.sessions.lock().await;
        let Some(entry) = sessions.get(&session_id) else {
            return SessionStatus::Unknown;
        };

        let exit_code = entry.session.exit_code();

        if entry.session.has_exited() {
            let Some(entry) = sessions.remove(&session_id) else {
                return SessionStatus::Unknown;
            };
            SessionStatus::Exited {
                exit_code,
                entry: Box::new(entry),
            }
        } else {
            SessionStatus::Alive {
                exit_code,
                call_id: entry.call_id.clone(),
            }
        }
    }

    async fn prepare_session_handles(
        &self,
        session_id: i32,
    ) -> Result<(mpsc::Sender<Vec<u8>>, OutputBuffer, Arc<Notify>), UnifiedExecError> {
        let sessions = self.sessions.lock().await;
        let (output_buffer, output_notify, writer_tx) =
            if let Some(entry) = sessions.get(&session_id) {
                let (buffer, notify) = entry.session.output_handles();
                (buffer, notify, entry.session.writer_sender())
            } else {
                return Err(UnifiedExecError::UnknownSessionId { session_id });
            };

        Ok((writer_tx, output_buffer, output_notify))
    }

    async fn send_input(
        writer_tx: &mpsc::Sender<Vec<u8>>,
        data: &[u8],
    ) -> Result<(), UnifiedExecError> {
        writer_tx
            .send(data.to_vec())
            .await
            .map_err(|_| UnifiedExecError::WriteToStdin)
    }

    pub(crate) async fn store_session(
        &self,
        session: UnifiedExecSession,
        context: &UnifiedExecContext,
        command: &str,
        started_at: StdInstant,
    ) -> i32 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        let entry = SessionEntry {
            session: Arc::new(session),
            session_ref: Some(Arc::clone(&context.session)),
            turn_ref: Some(Arc::clone(&context.turn)),
            call_id: Some(context.call_id.clone()),
            command: command.to_string(),
            cwd: context.turn.cwd.clone(),
            started_at,
        };
        self.sessions.lock().await.insert(session_id, entry);
        session_id
    }

    pub(crate) async fn store_session_without_context(
        &self,
        session: UnifiedExecSession,
        command: Vec<String>,
        cwd: PathBuf,
    ) -> i32 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        let entry = SessionEntry {
            session: Arc::new(session),
            session_ref: None,
            turn_ref: None,
            call_id: None,
            command: command.join(" "),
            cwd,
            started_at: StdInstant::now(),
        };
        self.sessions.lock().await.insert(session_id, entry);
        session_id
    }

    async fn emit_exec_end_from_entry(
        entry: SessionEntry,
        aggregated_output: String,
        exit_code: i32,
        duration: Duration,
    ) {
        if let (Some(session_ref), Some(turn_ref), Some(call_id)) = (
            entry.session_ref.as_ref(),
            entry.turn_ref.as_ref(),
            entry.call_id.as_ref(),
        ) {
            let output = ExecToolCallOutput {
                exit_code,
                stdout: StreamOutput::new(aggregated_output.clone()),
                stderr: StreamOutput::new(String::new()),
                aggregated_output: StreamOutput::new(aggregated_output),
                duration,
                timed_out: false,
            };
            let event_ctx =
                ToolEventCtx::new(session_ref.as_ref(), turn_ref.as_ref(), call_id, None);
            let emitter = ToolEmitter::unified_exec(entry.command, entry.cwd, true);
            emitter
                .emit(event_ctx, ToolEventStage::Success(output))
                .await;
        }
    }

    async fn emit_exec_end_from_context(
        context: &UnifiedExecContext,
        command: String,
        aggregated_output: String,
        exit_code: i32,
        duration: Duration,
    ) {
        let output = ExecToolCallOutput {
            exit_code,
            stdout: StreamOutput::new(aggregated_output.clone()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(aggregated_output),
            duration,
            timed_out: false,
        };
        let event_ctx = ToolEventCtx::new(
            context.session.as_ref(),
            context.turn.as_ref(),
            &context.call_id,
            None,
        );
        let emitter = ToolEmitter::unified_exec(command, context.turn.cwd.clone(), true);
        emitter
            .emit(event_ctx, ToolEventStage::Success(output))
            .await;
    }

    pub(crate) async fn open_session_with_exec_env(
        &self,
        env: &ExecEnv,
    ) -> Result<UnifiedExecSession, UnifiedExecError> {
        let (program, args) = env
            .command
            .split_first()
            .ok_or(UnifiedExecError::MissingCommandLine)?;
        let spawned =
            codex_utils_pty::spawn_pty_process(program, args, env.cwd.as_path(), &env.env)
                .await
                .map_err(UnifiedExecError::create_session)?;
        UnifiedExecSession::from_spawned(env.command.clone(), spawned, env.sandbox).await
    }

    pub(super) async fn open_session_with_sandbox(
        &self,
        command: Vec<String>,
        context: &UnifiedExecContext,
    ) -> Result<UnifiedExecSession, UnifiedExecError> {
        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = UnifiedExecRuntime::new(self);
        let req = UnifiedExecToolRequest::new(
            command,
            context.turn.cwd.clone(),
            create_env(&context.turn.shell_environment_policy),
        );
        let tool_ctx = ToolCtx {
            session: context.session.as_ref(),
            turn: context.turn.as_ref(),
            call_id: context.call_id.clone(),
            tool_name: "exec_command".to_string(),
        };
        orchestrator
            .run(
                &mut runtime,
                &req,
                &tool_ctx,
                context.turn.as_ref(),
                context.turn.approval_policy,
            )
            .await
            .map_err(|e| UnifiedExecError::create_session(AnyhowError::msg(format!("{e:?}"))))
    }

    pub(crate) async fn terminate_session(
        &self,
        session_id: i32,
    ) -> Result<UnifiedExecKillResult, UnifiedExecError> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(&session_id)
        }
        .ok_or(UnifiedExecError::UnknownSessionId { session_id })?;

        let call_id = entry.call_id.clone().unwrap_or_default();

        entry.session.kill()?;

        let (output_buffer, output_notify) = entry.session.output_handles();
        let deadline = TokioInstant::now() + Duration::from_millis(MIN_YIELD_TIME_MS);
        let collected =
            Self::collect_output_until_deadline(&output_buffer, &output_notify, deadline).await;
        let aggregated_output = String::from_utf8_lossy(&collected).to_string();
        let exit_code = entry.session.exit_code().unwrap_or(-1);
        let duration = StdInstant::now().saturating_duration_since(entry.started_at);

        Self::emit_exec_end_from_entry(entry, aggregated_output.clone(), exit_code, duration).await;

        Ok(UnifiedExecKillResult {
            exit_code,
            aggregated_output,
            call_id,
        })
    }

    pub(crate) async fn collect_output_until_deadline(
        output_buffer: &OutputBuffer,
        output_notify: &Arc<Notify>,
        deadline: TokioInstant,
    ) -> Vec<u8> {
        let mut collected: Vec<u8> = Vec::with_capacity(4096);
        loop {
            let drained_chunks;
            let mut wait_for_output = None;
            {
                let mut guard = output_buffer.lock().await;
                drained_chunks = guard.drain();
                if drained_chunks.is_empty() {
                    wait_for_output = Some(output_notify.notified());
                }
            }

            if drained_chunks.is_empty() {
                let remaining = deadline.saturating_duration_since(TokioInstant::now());
                if remaining == Duration::ZERO {
                    break;
                }

                let notified = wait_for_output.unwrap_or_else(|| output_notify.notified());
                tokio::pin!(notified);
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(remaining) => break,
                }
                continue;
            }

            for chunk in drained_chunks {
                collected.extend_from_slice(&chunk);
            }

            if TokioInstant::now() >= deadline {
                break;
            }
        }

        collected
    }

    pub(crate) async fn snapshot(&self) -> Vec<UnifiedExecSessionSnapshot> {
        let sessions = {
            let guard = self.sessions.lock().await;
            guard
                .iter()
                .map(|(id, entry)| (*id, Arc::clone(&entry.session)))
                .collect::<Vec<_>>()
        };

        let mut snapshots = Vec::with_capacity(sessions.len());
        for (id, session) in sessions {
            snapshots.push(session.snapshot(id).await);
        }
        snapshots.sort_by_key(|snapshot| snapshot.session_id);
        snapshots
    }

    pub(crate) async fn session_output_window(
        &self,
        session_id: i32,
        window: UnifiedExecOutputWindow,
    ) -> Option<UnifiedExecSessionOutput> {
        let session = {
            let guard = self.sessions.lock().await;
            guard
                .get(&session_id)
                .map(|entry| Arc::clone(&entry.session))
        }?;

        match session.output_window(session_id, window).await {
            Ok(output) => Some(output),
            Err(err) => {
                warn!(
                    error = ?err,
                    session_id,
                    "failed to load unified exec output window"
                );
                None
            }
        }
    }

    pub(crate) async fn export_session_log<P: AsRef<std::path::Path>>(
        &self,
        session_id: i32,
        destination: P,
    ) -> Result<(), UnifiedExecError> {
        let session = {
            let guard = self.sessions.lock().await;
            guard
                .get(&session_id)
                .map(|entry| Arc::clone(&entry.session))
        }
        .ok_or(UnifiedExecError::UnknownSessionId { session_id })?;

        session.export_log(session_id, destination).await
    }

    pub(crate) async fn kill_session(&self, session_id: i32) -> bool {
        let session = {
            let guard = self.sessions.lock().await;
            guard
                .get(&session_id)
                .map(|entry| Arc::clone(&entry.session))
        };

        if let Some(session) = session {
            session.kill().is_ok()
        } else {
            false
        }
    }

    pub(crate) async fn remove_session(&self, session_id: i32) -> bool {
        let entry = self.sessions.lock().await.remove(&session_id);
        entry.is_some()
    }
}

enum SessionStatus {
    Alive {
        exit_code: Option<i32>,
        call_id: Option<String>,
    },
    Exited {
        exit_code: Option<i32>,
        entry: Box<SessionEntry>,
    },
    Unknown,
}
