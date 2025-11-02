use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const MAX_TRACKED_ENTRIES: usize = 8;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;

pub(crate) struct LiveExecState {
    root_cwd: PathBuf,
    entries: HashMap<String, LiveExecEntry>,
    order: VecDeque<String>,
}

impl LiveExecState {
    pub(crate) fn new(root_cwd: PathBuf) -> Self {
        Self {
            root_cwd,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(crate) fn begin(&mut self, call_id: String, command: Vec<String>, cwd: PathBuf) {
        let entry = LiveExecEntry::new(command, cwd);
        self.entries.insert(call_id.clone(), entry);
        self.order.retain(|id| id != &call_id);
        self.order.push_back(call_id);
        self.prune_finished_overflow();
    }

    pub(crate) fn append_chunk(&mut self, call_id: &str, chunk: &str) -> bool {
        if let Some(entry) = self.entries.get_mut(call_id) {
            entry.append_chunk(chunk);
            true
        } else {
            false
        }
    }

    pub(crate) fn finish(
        &mut self,
        call_id: &str,
        exit_code: i32,
        duration: Duration,
        aggregated_output: String,
    ) -> bool {
        if let Some(entry) = self.entries.get_mut(call_id) {
            entry.finish(exit_code, duration, aggregated_output);
            self.prune_finished_overflow();
            true
        } else {
            false
        }
    }

    pub(crate) fn entries(&self) -> LiveExecEntries<'_> {
        LiveExecEntries {
            state: self,
            index: 0,
        }
    }

    pub(crate) fn root_cwd(&self) -> &Path {
        &self.root_cwd
    }

    fn prune_finished_overflow(&mut self) {
        while self.order.len() > MAX_TRACKED_ENTRIES {
            let remove_id = match self.order.front() {
                Some(id) => id.clone(),
                None => break,
            };
            let should_remove = self
                .entries
                .get(&remove_id)
                .map(|entry| entry.status.is_finished())
                .unwrap_or(true);
            if should_remove {
                self.order.pop_front();
                self.entries.remove(&remove_id);
            } else {
                break;
            }
        }
    }
}

pub(crate) struct LiveExecEntries<'a> {
    state: &'a LiveExecState,
    index: usize,
}

impl<'a> Iterator for LiveExecEntries<'a> {
    type Item = &'a LiveExecEntry;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(call_id) = self.state.order.get(self.index) {
            self.index += 1;
            if let Some(entry) = self.state.entries.get(call_id) {
                return Some(entry);
            }
        }
        None
    }
}

pub(crate) struct LiveExecEntry {
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) started_at: Instant,
    pub(crate) output: String,
    pub(crate) truncated_lines: usize,
    pub(crate) truncated_partial: bool,
    pub(crate) status: LiveExecStatus,
}

impl LiveExecEntry {
    fn new(command: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            command,
            cwd,
            started_at: Instant::now(),
            output: String::new(),
            truncated_lines: 0,
            truncated_partial: false,
            status: LiveExecStatus::Running,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self.status, LiveExecStatus::Running)
    }

    fn append_chunk(&mut self, chunk: &str) {
        self.output.push_str(chunk);
        self.trim_output();
    }

    fn finish(&mut self, exit_code: i32, duration: Duration, aggregated_output: String) {
        if !aggregated_output.is_empty() {
            self.output = aggregated_output;
        }
        self.trim_output();
        self.status = LiveExecStatus::Finished {
            exit_code,
            duration,
        };
    }

    fn trim_output(&mut self) {
        if self.output.len() <= MAX_OUTPUT_BYTES {
            return;
        }
        let target_start = self.output.len() - MAX_OUTPUT_BYTES;
        let mut cut_idx = self
            .output
            .char_indices()
            .find_map(|(idx, _)| (idx >= target_start).then_some(idx))
            .unwrap_or(self.output.len());
        if let Some(rest) = self.output.get(cut_idx..)
            && let Some(pos) = rest.find('\n')
        {
            cut_idx += pos + 1;
        }
        let drained: String = self.output.drain(..cut_idx).collect();
        self.truncated_lines = self
            .truncated_lines
            .saturating_add(drained.matches('\n').count());
        self.truncated_partial = !drained.is_empty() && !drained.ends_with('\n');
    }
}

pub(crate) enum LiveExecStatus {
    Running,
    Finished { exit_code: i32, duration: Duration },
}

impl LiveExecStatus {
    fn is_finished(&self) -> bool {
        matches!(self, LiveExecStatus::Finished { .. })
    }
}
