use crate::index::IndexCoordinator;
use crate::project::ProjectProfile;
use crate::proto::DoctorReport;
use crate::proto::DoctorWorkspace;
use crate::proto::IndexState;
use crate::proto::PROTOCOL_VERSION;
use anyhow::Context;
use anyhow::Result;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Notify;

pub(crate) struct WorkspaceHandle {
    coordinator: IndexCoordinator,
}

impl WorkspaceHandle {
    async fn create(profile: ProjectProfile, auto_indexing: bool) -> Result<Arc<Self>> {
        let coordinator = IndexCoordinator::new(profile, auto_indexing).await?;
        Ok(Arc::new(Self { coordinator }))
    }

    pub(crate) fn coordinator(&self) -> &IndexCoordinator {
        &self.coordinator
    }

    pub(crate) fn shutdown(&self) {
        self.coordinator.cancel_background();
    }
}

impl Drop for WorkspaceHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct WorkspaceState {
    entries: HashMap<String, Arc<WorkspaceHandle>>,
    order: VecDeque<String>,
    waiters: HashMap<String, Arc<Notify>>,
}

pub(crate) struct WorkspaceRegistry {
    state: Mutex<WorkspaceState>,
    capacity: usize,
    auto_indexing_default: bool,
    codex_home: Option<PathBuf>,
}

impl WorkspaceRegistry {
    pub(crate) fn new(
        capacity: usize,
        auto_indexing_default: bool,
        codex_home: Option<PathBuf>,
    ) -> Self {
        let cap = capacity.max(1);
        Self {
            state: Mutex::new(WorkspaceState {
                entries: HashMap::new(),
                order: VecDeque::new(),
                waiters: HashMap::new(),
            }),
            capacity: cap,
            auto_indexing_default,
            codex_home,
        }
    }

    pub(crate) async fn checkout(&self, requested_root: &str) -> Result<Arc<WorkspaceHandle>> {
        let requested_root = requested_root.trim();
        if requested_root.is_empty() {
            anyhow::bail!("project_root must not be empty");
        }
        let root_path = Path::new(requested_root);
        let profile = ProjectProfile::detect(Some(root_path), self.codex_home.as_deref())
            .with_context(|| format!("failed to prepare workspace for {requested_root}"))?;
        let key = profile.hash().to_string();
        loop {
            let maybe_waiter = {
                let mut guard = self.state.lock().await;
                if let Some(entry) = guard.entries.get(&key).cloned() {
                    Self::touch(&mut guard.order, &key);
                    return Ok(entry);
                }
                if let Some(waiter) = guard.waiters.get(&key) {
                    Some(waiter.clone())
                } else {
                    let wait = Arc::new(Notify::new());
                    guard.waiters.insert(key.clone(), wait.clone());
                    drop(guard);
                    let handle =
                        match WorkspaceHandle::create(profile.clone(), self.auto_indexing_default)
                            .await
                        {
                            Ok(handle) => handle,
                            Err(err) => {
                                let mut guard = self.state.lock().await;
                                guard.waiters.remove(&key);
                                wait.notify_waiters();
                                return Err(err);
                            }
                        };
                    let mut guard = self.state.lock().await;
                    guard.waiters.remove(&key);
                    Self::insert_entry(&mut guard, key.clone(), handle.clone(), self.capacity);
                    wait.notify_waiters();
                    return Ok(handle);
                }
            };

            if let Some(waiter) = maybe_waiter {
                waiter.notified().await;
            }
        }
    }

    fn insert_entry(
        state: &mut WorkspaceState,
        key: String,
        handle: Arc<WorkspaceHandle>,
        capacity: usize,
    ) {
        state.entries.insert(key.clone(), handle);
        Self::touch(&mut state.order, &key);
        while state.entries.len() > capacity {
            if let Some(evicted_key) = state.order.pop_front() {
                if let Some(evicted) = state.entries.remove(&evicted_key) {
                    evicted.shutdown();
                }
            } else {
                break;
            }
        }
    }

    fn touch(order: &mut VecDeque<String>, key: &str) {
        if let Some(pos) = order.iter().position(|existing| existing == key) {
            order.remove(pos);
        }
        order.push_back(key.to_string());
    }

    pub(crate) async fn doctor_report(&self, pid: u32) -> DoctorReport {
        let handles = {
            let guard = self.state.lock().await;
            guard.entries.values().cloned().collect::<Vec<_>>()
        };
        let mut workspaces = Vec::new();
        for handle in handles {
            let coordinator = handle.coordinator();
            let status = coordinator.current_status().await;
            let diagnostics = coordinator.diagnostics().await;
            let project_root = coordinator.project_root().to_string_lossy().into_owned();
            workspaces.push(DoctorWorkspace {
                project_root,
                index: status,
                diagnostics,
            });
        }
        let actions = workspaces
            .iter()
            .filter(|ws| matches!(ws.index.state, IndexState::Building))
            .map(|ws| format!("rebuilding {}", ws.project_root))
            .collect();
        DoctorReport {
            daemon_pid: pid,
            protocol_version: PROTOCOL_VERSION,
            workspaces,
            actions,
        }
    }
}
