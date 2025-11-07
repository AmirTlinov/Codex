use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use codex_protocol::protocol::UnifiedExecSessionState;
use codex_protocol::protocol::UnifiedExecSessionStatus;
use codex_utils_pty::ExecCommandSession;
use codex_utils_pty::SpawnedPty;
use tempfile::tempfile;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::error;
use tracing::warn;

use crate::exec::ExecToolCallOutput;
use crate::exec::SandboxType;
use crate::exec::StreamOutput;
use crate::exec::is_likely_sandbox_denied;
use crate::truncate::truncate_middle;

use super::UnifiedExecError;

pub const UNIFIED_EXEC_OUTPUT_MAX_BYTES: usize = 128 * 1024; // 128 KiB
pub const UNIFIED_EXEC_WINDOW_DEFAULT_BYTES: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES;
pub const UNIFIED_EXEC_WINDOW_MAX_BYTES: usize = 2 * 1024 * 1024; // 2 MiB
pub const UNIFIED_EXEC_PREVIEW_MAX_BYTES: usize = 4 * 1024;

#[derive(Debug)]
pub(crate) struct UnifiedExecSession {
    command: Vec<String>,
    started_at: SystemTime,
    session: ExecCommandSession,
    sandbox_type: SandboxType,
    output_buffer: OutputBuffer,
    output_notify: Arc<Notify>,
    output_task: JoinHandle<()>,
    output_spool: Option<Arc<OutputSpool>>,
}

pub(crate) type OutputBuffer = Arc<Mutex<OutputBufferState>>;
pub(crate) type OutputHandles = (OutputBuffer, Arc<Notify>);

#[derive(Debug, Default)]
pub(crate) struct OutputBufferState {
    chunks: VecDeque<Vec<u8>>,
    total_bytes: usize,
    last_output_at: Option<SystemTime>,
    truncated_prefix: bool,
}

impl OutputBufferState {
    fn push_chunk(&mut self, chunk: Vec<u8>) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        self.chunks.push_back(chunk);
        self.last_output_at = Some(SystemTime::now());

        let mut excess = self
            .total_bytes
            .saturating_sub(UNIFIED_EXEC_OUTPUT_MAX_BYTES);

        while excess > 0 {
            match self.chunks.front_mut() {
                Some(front) if excess >= front.len() => {
                    excess -= front.len();
                    self.total_bytes = self.total_bytes.saturating_sub(front.len());
                    self.chunks.pop_front();
                    self.truncated_prefix = true;
                }
                Some(front) => {
                    front.drain(..excess);
                    self.total_bytes = self.total_bytes.saturating_sub(excess);
                    self.truncated_prefix = true;
                    break;
                }
                None => break,
            }
        }
    }

    pub(super) fn drain(&mut self) -> Vec<Vec<u8>> {
        let drained: Vec<Vec<u8>> = self.chunks.drain(..).collect();
        self.total_bytes = 0;
        drained
    }

    fn snapshot_bytes(&self) -> Vec<u8> {
        let mut aggregated = Vec::with_capacity(self.total_bytes);
        for chunk in &self.chunks {
            aggregated.extend_from_slice(chunk);
        }
        aggregated
    }

    fn was_truncated(&self) -> bool {
        self.truncated_prefix
    }

    fn last_output_at(&self) -> Option<SystemTime> {
        self.last_output_at
    }
}

#[derive(Debug)]
struct OutputSpool {
    file: Arc<StdMutex<std::fs::File>>,
    total_bytes: AtomicU64,
    failed: AtomicBool,
}

impl OutputSpool {
    fn new() -> Result<Self, std::io::Error> {
        let file = tempfile()?;
        Ok(Self {
            file: Arc::new(StdMutex::new(file)),
            total_bytes: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        })
    }

    fn append(&self, chunk: &[u8]) -> Result<(), std::io::Error> {
        if self.failed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut guard = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("spool poisoned"))?;
        if let Err(err) = guard.write_all(chunk) {
            self.failed.store(true, Ordering::SeqCst);
            return Err(err);
        }
        if let Err(err) = guard.flush() {
            self.failed.store(true, Ordering::SeqCst);
            return Err(err);
        }
        self.total_bytes
            .fetch_add(chunk.len() as u64, Ordering::SeqCst);
        Ok(())
    }

    fn len(&self) -> u64 {
        self.total_bytes.load(Ordering::SeqCst)
    }

    fn read_range(&self, start: u64, max_bytes: usize) -> Result<Vec<u8>, std::io::Error> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("spool unavailable"));
        }
        let file = {
            let guard = self
                .file
                .lock()
                .map_err(|_| std::io::Error::other("spool poisoned"))?;
            guard.try_clone().inspect_err(|_| {
                self.failed.store(true, Ordering::SeqCst);
            })?
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(start)).inspect_err(|_| {
            self.failed.store(true, Ordering::SeqCst);
        })?;
        let mut take = reader.take(max_bytes as u64);
        let mut buf = Vec::with_capacity(max_bytes);
        take.read_to_end(&mut buf).inspect_err(|_| {
            self.failed.store(true, Ordering::SeqCst);
        })?;
        Ok(buf)
    }

    fn copy_to_path<P: AsRef<std::path::Path>>(
        &self,
        destination: P,
    ) -> Result<(), std::io::Error> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("spool unavailable"));
        }
        let file = {
            let guard = self
                .file
                .lock()
                .map_err(|_| std::io::Error::other("spool poisoned"))?;
            guard.try_clone().inspect_err(|_| {
                self.failed.store(true, Ordering::SeqCst);
            })?
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(0)).inspect_err(|_| {
            self.failed.store(true, Ordering::SeqCst);
        })?;

        if let Some(parent) = destination.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut writer = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(destination)?;
        std::io::copy(&mut reader, &mut writer).inspect_err(|_| {
            self.failed.store(true, Ordering::SeqCst);
        })?;
        writer.flush().inspect_err(|_| {
            self.failed.store(true, Ordering::SeqCst);
        })?;
        Ok(())
    }

    fn is_available(&self) -> bool {
        !self.failed.load(Ordering::SeqCst)
    }
}

impl UnifiedExecSession {
    pub(super) async fn from_spawned(
        command: Vec<String>,
        spawned: SpawnedPty,
        sandbox_type: SandboxType,
    ) -> Result<Self, UnifiedExecError> {
        let SpawnedPty {
            session,
            output_rx,
            mut exit_rx,
        } = spawned;

        let output_buffer = Arc::new(Mutex::new(OutputBufferState::default()));
        let output_notify = Arc::new(Notify::new());
        let output_spool = match OutputSpool::new() {
            Ok(spool) => Some(Arc::new(spool)),
            Err(err) => {
                warn!(
                    error = ?err,
                    "failed to initialize unified exec spool; falling back to in-memory buffer only"
                );
                None
            }
        };

        let mut receiver = output_rx;
        let buffer_clone = Arc::clone(&output_buffer);
        let notify_clone = Arc::clone(&output_notify);
        let spool_clone = output_spool.clone();
        let output_task = tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(chunk) => {
                        if let Some(spool) = spool_clone.as_ref()
                            && let Err(err) = spool.append(&chunk)
                        {
                            error!(
                                error = ?err,
                                "failed to persist unified exec output; continuing without spool"
                            );
                        }
                        let mut guard = buffer_clone.lock().await;
                        guard.push_chunk(chunk);
                        drop(guard);
                        notify_clone.notify_waiters();
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let managed = Self {
            command,
            started_at: SystemTime::now(),
            session,
            sandbox_type,
            output_buffer,
            output_notify,
            output_task,
            output_spool,
        };

        let exit_ready = match timeout(Duration::from_millis(5), &mut exit_rx).await {
            Ok(res) => res.is_ok(),
            Err(_) => false,
        };

        if exit_ready {
            managed.check_for_sandbox_denial().await?;
        }

        Ok(managed)
    }

    pub(crate) fn writer_sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.session.writer_sender()
    }

    pub(crate) fn output_handles(&self) -> OutputHandles {
        (
            Arc::clone(&self.output_buffer),
            Arc::clone(&self.output_notify),
        )
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.session.has_exited()
    }

    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.session.exit_code()
    }

    #[allow(clippy::result_large_err)] // UnifiedExecError carries detailed sandbox diagnostics required by clients.
    pub(crate) fn kill(&self) -> Result<(), UnifiedExecError> {
        self.session
            .kill()
            .map_err(|err| UnifiedExecError::kill_failed(err.to_string()))
    }

    pub(crate) async fn snapshot(&self, session_id: i32) -> UnifiedExecSessionSnapshot {
        let (aggregated, last_output_at, truncated_prefix) = {
            let guard = self.output_buffer.lock().await;
            (
                guard.snapshot_bytes(),
                guard.last_output_at(),
                guard.was_truncated(),
            )
        };
        let preview_raw = String::from_utf8_lossy(&aggregated);
        let (preview, maybe_tokens) = truncate_middle(&preview_raw, UNIFIED_EXEC_PREVIEW_MAX_BYTES);
        let output_truncated = truncated_prefix || maybe_tokens.is_some();

        UnifiedExecSessionSnapshot {
            session_id,
            command: self.command.clone(),
            started_at: self.started_at,
            last_output_at,
            has_exited: self.has_exited(),
            output_preview: preview,
            output_truncated,
        }
    }

    pub(crate) async fn output_window(
        &self,
        session_id: i32,
        window: UnifiedExecOutputWindow,
    ) -> Result<UnifiedExecSessionOutput, UnifiedExecError> {
        let (tail_bytes, last_output_at, ring_truncated) = {
            let guard = self.output_buffer.lock().await;
            (
                guard.snapshot_bytes(),
                guard.last_output_at(),
                guard.was_truncated(),
            )
        };

        let status = if self.has_exited() {
            UnifiedExecSessionStatus::Exited
        } else {
            UnifiedExecSessionStatus::Running
        };

        if let Some(spool) = &self.output_spool
            && spool.is_available()
        {
            let total_bytes = spool.len();
            let (range_start, range_end, truncated_prefix, truncated_suffix, window_bytes) =
                resolve_window_bounds(total_bytes, window);

            if window_bytes == 0 {
                return Ok(UnifiedExecSessionOutput {
                    session_id,
                    command: self.command.clone(),
                    started_at: self.started_at,
                    last_output_at,
                    status,
                    content: String::new(),
                    truncated: truncated_prefix || ring_truncated,
                    truncated_suffix,
                    expandable_prefix: truncated_prefix || ring_truncated,
                    expandable_suffix: truncated_suffix,
                    range_start,
                    range_end,
                    total_bytes,
                    window_bytes,
                });
            }

            let spool_clone = Arc::clone(spool);
            let bytes = tokio::task::spawn_blocking(move || {
                spool_clone.read_range(range_start, window_bytes)
            })
            .await
            .map_err(|err| UnifiedExecError::read_output(std::io::Error::other(err.to_string())))?
            .map_err(UnifiedExecError::read_output)?;

            let content = String::from_utf8_lossy(&bytes).into_owned();
            return Ok(UnifiedExecSessionOutput {
                session_id,
                command: self.command.clone(),
                started_at: self.started_at,
                last_output_at,
                status,
                content,
                truncated: truncated_prefix || ring_truncated,
                truncated_suffix,
                expandable_prefix: truncated_prefix || ring_truncated,
                expandable_suffix: truncated_suffix,
                range_start,
                range_end,
                total_bytes,
                window_bytes,
            });
        }

        let total_bytes = tail_bytes.len() as u64;
        let (range_start, range_end, truncated_prefix, truncated_suffix, window_bytes) =
            resolve_window_bounds(total_bytes, window);
        let slice_start = range_start as usize;
        let slice_end = slice_start + window_bytes;
        let content = String::from_utf8_lossy(
            &tail_bytes[slice_start.min(tail_bytes.len())..slice_end.min(tail_bytes.len())],
        )
        .into_owned();

        Ok(UnifiedExecSessionOutput {
            session_id,
            command: self.command.clone(),
            started_at: self.started_at,
            last_output_at,
            status,
            content,
            truncated: truncated_prefix || ring_truncated,
            truncated_suffix,
            expandable_prefix: truncated_prefix || ring_truncated,
            expandable_suffix: truncated_suffix,
            range_start,
            range_end,
            total_bytes,
            window_bytes,
        })
    }

    pub(crate) async fn export_log<P: AsRef<std::path::Path>>(
        &self,
        session_id: i32,
        destination: P,
    ) -> Result<(), UnifiedExecError> {
        if let Some(spool) = self.output_spool.as_ref()
            && spool.is_available()
        {
            let destination = destination.as_ref().to_path_buf();
            let spool_clone = Arc::clone(spool);
            tokio::task::spawn_blocking(move || spool_clone.copy_to_path(destination))
                .await
                .map_err(|err| {
                    UnifiedExecError::export_log(std::io::Error::other(err.to_string()))
                })?
                .map_err(UnifiedExecError::export_log)?;
            return Ok(());
        }

        let output = self
            .output_window(
                session_id,
                UnifiedExecOutputWindow::Range {
                    start: 0,
                    max_bytes: UNIFIED_EXEC_WINDOW_MAX_BYTES,
                },
            )
            .await?;

        if let Some(parent) = destination.as_ref().parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(UnifiedExecError::export_log)?;
        }

        let mut file = tokio::fs::File::create(destination)
            .await
            .map_err(UnifiedExecError::export_log)?;
        file.write_all(output.content.as_bytes())
            .await
            .map_err(UnifiedExecError::export_log)?;
        file.flush().await.map_err(UnifiedExecError::export_log)?;
        Ok(())
    }

    async fn check_for_sandbox_denial(&self) -> Result<(), UnifiedExecError> {
        if self.sandbox_type == SandboxType::None || !self.has_exited() {
            return Ok(());
        }

        let _ = timeout(Duration::from_millis(20), self.output_notify.notified()).await;

        let collected_chunks = {
            let guard = self.output_buffer.lock().await;
            guard.snapshot_bytes()
        };

        let aggregated_text = String::from_utf8_lossy(&collected_chunks).to_string();
        let exit_code = self.exit_code().unwrap_or(-1);

        let exec_output = ExecToolCallOutput {
            exit_code,
            stdout: StreamOutput::new(aggregated_text.clone()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(aggregated_text.clone()),
            duration: Duration::ZERO,
            timed_out: false,
        };

        if is_likely_sandbox_denied(self.sandbox_type, &exec_output) {
            let (snippet, _) = truncate_middle(&aggregated_text, UNIFIED_EXEC_OUTPUT_MAX_BYTES);
            let message = if snippet.is_empty() {
                format!("exit code {exit_code}")
            } else {
                snippet
            };
            return Err(UnifiedExecError::sandbox_denied(message, exec_output));
        }

        Ok(())
    }
}

impl Drop for UnifiedExecSession {
    fn drop(&mut self) {
        self.output_task.abort();
    }
}

#[derive(Debug, Clone)]
pub struct UnifiedExecSessionSnapshot {
    pub session_id: i32,
    pub command: Vec<String>,
    pub started_at: SystemTime,
    pub last_output_at: Option<SystemTime>,
    pub has_exited: bool,
    pub output_preview: String,
    pub output_truncated: bool,
}

impl UnifiedExecSessionSnapshot {
    fn status(&self) -> UnifiedExecSessionStatus {
        if self.has_exited {
            UnifiedExecSessionStatus::Exited
        } else {
            UnifiedExecSessionStatus::Running
        }
    }
}

impl From<UnifiedExecSessionSnapshot> for UnifiedExecSessionState {
    fn from(snapshot: UnifiedExecSessionSnapshot) -> Self {
        let status = snapshot.status();
        let UnifiedExecSessionSnapshot {
            session_id,
            command,
            started_at,
            last_output_at,
            has_exited: _,
            output_preview,
            output_truncated,
        } = snapshot;
        Self {
            session_id,
            command,
            status,
            started_at_ms: system_time_to_millis(started_at),
            last_output_at_ms: last_output_at.map(system_time_to_millis),
            output_preview,
            output_truncated,
        }
    }
}

#[derive(Debug, Clone)]
pub enum UnifiedExecOutputWindow {
    Tail { max_bytes: usize },
    Range { start: u64, max_bytes: usize },
}

impl UnifiedExecOutputWindow {
    pub fn tail_default() -> Self {
        Self::Tail {
            max_bytes: UNIFIED_EXEC_WINDOW_DEFAULT_BYTES,
        }
    }

    fn clamp_bytes(&self) -> usize {
        let requested = match *self {
            UnifiedExecOutputWindow::Tail { max_bytes }
            | UnifiedExecOutputWindow::Range { max_bytes, .. } => max_bytes,
        };
        requested.clamp(1, UNIFIED_EXEC_WINDOW_MAX_BYTES)
    }
}

#[derive(Debug, Clone)]
pub struct UnifiedExecSessionOutput {
    pub session_id: i32,
    pub command: Vec<String>,
    pub started_at: SystemTime,
    pub last_output_at: Option<SystemTime>,
    pub status: UnifiedExecSessionStatus,
    pub content: String,
    pub truncated: bool,
    pub truncated_suffix: bool,
    pub expandable_prefix: bool,
    pub expandable_suffix: bool,
    pub range_start: u64,
    pub range_end: u64,
    pub total_bytes: u64,
    pub window_bytes: usize,
}

fn resolve_window_bounds(
    total_bytes: u64,
    window: UnifiedExecOutputWindow,
) -> (u64, u64, bool, bool, usize) {
    if total_bytes == 0 {
        return (0, 0, false, false, 0);
    }

    let max_bytes = window.clamp_bytes().min(total_bytes as usize);

    match window {
        UnifiedExecOutputWindow::Tail { .. } => {
            let end = total_bytes;
            let start = end.saturating_sub(max_bytes as u64);
            let actual = (end - start) as usize;
            (start, end, start > 0, false, actual)
        }
        UnifiedExecOutputWindow::Range { start, .. } => {
            let clamped_start = start.min(total_bytes);
            let end = (clamped_start + max_bytes as u64).min(total_bytes);
            let actual = (end - clamped_start) as usize;
            (
                clamped_start,
                end,
                clamped_start > 0,
                end < total_bytes,
                actual,
            )
        }
    }
}

fn system_time_to_millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::truncate::truncate_middle;

    #[tokio::test]
    async fn output_buffer_state_trims_excess_bytes() {
        let mut buffer = OutputBufferState::default();
        buffer.push_chunk(vec![b'a'; UNIFIED_EXEC_OUTPUT_MAX_BYTES]);
        buffer.push_chunk(vec![b'b']);
        buffer.push_chunk(vec![b'c']);

        assert_eq!(buffer.total_bytes, UNIFIED_EXEC_OUTPUT_MAX_BYTES);
        assert_eq!(buffer.chunks.len(), 3);
        let drained = buffer.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(buffer.total_bytes, 0);
    }

    #[tokio::test]
    async fn resolve_window_bounds_handles_tail() {
        let (start, end, truncated_prefix, truncated_suffix, window_bytes) =
            resolve_window_bounds(10, UnifiedExecOutputWindow::Tail { max_bytes: 4 });
        assert_eq!((start, end), (6, 10));
        assert!(truncated_prefix);
        assert!(!truncated_suffix);
        assert_eq!(window_bytes, 4);
    }

    #[test]
    fn truncate_middle_keeps_marker() {
        let s = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ*";
        let max_bytes = 32;
        let (out, original) = truncate_middle(s, max_bytes);
        assert!(out.contains("tokens truncated"));
        assert!(original.is_some());
    }
}
