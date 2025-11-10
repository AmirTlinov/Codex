use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_code_finder::client::ClientOptions;
use codex_code_finder::client::CodeFinderClient;
use codex_code_finder::client::DaemonSpawn;
use codex_code_finder::proto::IndexState;
use codex_code_finder::proto::IndexStatus;
use codex_core::config::Config;
use once_cell::sync::Lazy;
use once_cell::sync::OnceCell;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::warn;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const HEALTH_READY_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_BUILDING_INTERVAL: Duration = Duration::from_secs(2);
const HEALTH_FAILED_INTERVAL: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
struct CodeFinderContext {
    project_root: PathBuf,
    codex_home: PathBuf,
    spawn: DaemonSpawn,
}

impl CodeFinderContext {
    fn new(config: &Config) -> Result<Self> {
        let project_root = config.cwd.clone();
        if !project_root.exists() {
            anyhow::bail!(
                "code_finder cannot index missing cwd {}",
                project_root.display()
            );
        }
        let codex_home = config.codex_home.clone();
        let exe = std::env::current_exe().context("resolve current executable for code_finder")?;
        let spawn = DaemonSpawn {
            program: exe,
            args: vec![
                "code-finder-daemon".to_string(),
                "--project-root".to_string(),
                project_root.display().to_string(),
            ],
            env: vec![("CODEX_HOME".to_string(), codex_home.display().to_string())],
        };
        Ok(Self {
            project_root,
            codex_home,
            spawn,
        })
    }

    fn client_options_with_spawn(&self) -> ClientOptions {
        ClientOptions {
            project_root: Some(self.project_root.clone()),
            codex_home: Some(self.codex_home.clone()),
            spawn: Some(self.spawn.clone()),
        }
    }
}

static CONTEXT: OnceCell<Arc<CodeFinderContext>> = OnceCell::new();
static ACTIVE_CLIENT: Lazy<RwLock<Option<CodeFinderClient>>> = Lazy::new(|| RwLock::new(None));

pub fn spawn_background_indexer(config: &Config, app_event_tx: AppEventSender) {
    let ctx = match CodeFinderContext::new(config) {
        Ok(ctx) => Arc::new(ctx),
        Err(err) => {
            warn!("code_finder bootstrap skipped: {err:?}");
            return;
        }
    };
    let _ = CONTEXT.set(ctx.clone());
    tokio::spawn(async move {
        if let Err(err) = monitor_daemon(ctx, app_event_tx).await {
            warn!("code_finder bootstrap failed: {err:?}");
        }
    });
}

pub async fn request_reindex() -> Result<IndexStatus> {
    let client = {
        let guard = ACTIVE_CLIENT.read().await;
        guard.clone()
    };
    let Some(client) = client else {
        anyhow::bail!(
            "code_finder daemon is still starting; wait for the footer status to appear before reindexing."
        );
    };
    client.reindex().await
}

async fn monitor_daemon(ctx: Arc<CodeFinderContext>, app_event_tx: AppEventSender) -> Result<()> {
    let mut last_error: Option<String> = None;
    loop {
        match CodeFinderClient::new(ctx.client_options_with_spawn()).await {
            Ok(client) => {
                last_error = None;
                {
                    let mut guard = ACTIVE_CLIENT.write().await;
                    *guard = Some(client.clone());
                }
                if let Err(err) = emit_status_loop(client.clone(), app_event_tx.clone()).await {
                    warn!("code_finder status loop ended: {err:?}");
                }
                let mut guard = ACTIVE_CLIENT.write().await;
                guard.take();
            }
            Err(err) => {
                let message = err.to_string();
                if last_error.as_ref() != Some(&message) {
                    app_event_tx.send(AppEvent::CodeFinderWarning(message.clone()));
                    last_error = Some(message);
                }
                warn!("code_finder daemon init failed: {err:?}");
            }
        }
        sleep(RETRY_DELAY).await;
    }
}

async fn emit_status_loop(client: CodeFinderClient, app_event_tx: AppEventSender) -> Result<()> {
    loop {
        let status = client.health().await?;
        app_event_tx.send(AppEvent::CodeFinderStatus(status.clone()));
        let delay = match status.state {
            IndexState::Building => HEALTH_BUILDING_INTERVAL,
            IndexState::Ready => HEALTH_READY_INTERVAL,
            IndexState::Failed => HEALTH_FAILED_INTERVAL,
        };
        sleep(delay).await;
    }
}
