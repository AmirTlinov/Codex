use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Barrier;
use tokio::time::sleep;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct TestSyncHandler;

const DEFAULT_TIMEOUT_MS: u64 = 1_000;

static BARRIERS: OnceLock<tokio::sync::Mutex<HashMap<String, BarrierState>>> = OnceLock::new();

struct BarrierState {
    barrier: Arc<Barrier>,
    participants: usize,
}

#[derive(Debug, Deserialize)]
struct BarrierArgs {
    id: String,
    participants: usize,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct TestSyncArgs {
    #[serde(default)]
    sleep_before_ms: Option<u64>,
    #[serde(default)]
    sleep_after_ms: Option<u64>,
    #[serde(default)]
    barrier: Option<BarrierArgs>,
    #[serde(default)]
    record_path: Option<String>,
    #[serde(default)]
    record_label: Option<String>,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn barrier_map() -> &'static tokio::sync::Mutex<HashMap<String, BarrierState>> {
    BARRIERS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

#[async_trait]
impl ToolHandler for TestSyncHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let call_id = invocation.call_id.clone();
        let ToolInvocation { payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "test_sync_tool handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: TestSyncArgs = serde_json::from_str(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to parse function arguments: {err:?}"
            ))
        })?;
        let _record = RecordGuard::start(&args, &call_id)?;

        if let Some(delay) = args.sleep_before_ms
            && delay > 0
        {
            sleep(Duration::from_millis(delay)).await;
        }

        if let Some(barrier) = args.barrier {
            wait_on_barrier(barrier).await?;
        }

        if let Some(delay) = args.sleep_after_ms
            && delay > 0
        {
            sleep(Duration::from_millis(delay)).await;
        }

        Ok(ToolOutput::Function {
            content: "ok".to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}

struct RecordGuard {
    path: Option<PathBuf>,
    label: String,
}

impl RecordGuard {
    fn start(args: &TestSyncArgs, call_id: &str) -> Result<Self, FunctionCallError> {
        let path = args.record_path.as_ref().map(PathBuf::from);
        let label = args
            .record_label
            .clone()
            .unwrap_or_else(|| call_id.to_string());
        let guard = Self { path, label };
        guard.record("start")?;
        Ok(guard)
    }

    fn record(&self, phase: &str) -> Result<(), FunctionCallError> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        append_record(path, &self.label, phase).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to write test_sync_tool record for {}: {err}",
                path.display()
            ))
        })
    }
}

impl Drop for RecordGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.as_ref() {
            let _ = append_record(path, &self.label, "end");
        }
    }
}

fn append_record(path: &Path, label: &str, phase: &str) -> std::io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{label} {phase} {now}")?;
    Ok(())
}

async fn wait_on_barrier(args: BarrierArgs) -> Result<(), FunctionCallError> {
    if args.participants == 0 {
        return Err(FunctionCallError::RespondToModel(
            "barrier participants must be greater than zero".to_string(),
        ));
    }

    if args.timeout_ms == 0 {
        return Err(FunctionCallError::RespondToModel(
            "barrier timeout must be greater than zero".to_string(),
        ));
    }

    let barrier_id = args.id.clone();
    let barrier = {
        let mut map = barrier_map().lock().await;
        match map.entry(barrier_id.clone()) {
            Entry::Occupied(entry) => {
                let state = entry.get();
                if state.participants != args.participants {
                    let existing = state.participants;
                    return Err(FunctionCallError::RespondToModel(format!(
                        "barrier {barrier_id} already registered with {existing} participants"
                    )));
                }
                state.barrier.clone()
            }
            Entry::Vacant(entry) => {
                let barrier = Arc::new(Barrier::new(args.participants));
                entry.insert(BarrierState {
                    barrier: barrier.clone(),
                    participants: args.participants,
                });
                barrier
            }
        }
    };

    let timeout = Duration::from_millis(args.timeout_ms);
    let wait_result = tokio::time::timeout(timeout, barrier.wait())
        .await
        .map_err(|_| {
            FunctionCallError::RespondToModel("test_sync_tool barrier wait timed out".to_string())
        })?;

    if wait_result.is_leader() {
        let mut map = barrier_map().lock().await;
        if let Some(state) = map.get(&barrier_id)
            && Arc::ptr_eq(&state.barrier, &barrier)
        {
            map.remove(&barrier_id);
        }
    }

    Ok(())
}
