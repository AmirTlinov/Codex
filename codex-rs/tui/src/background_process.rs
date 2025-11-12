use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_core::command_label::friendly_command_label_from_str;
use codex_core::protocol::BackgroundEventEvent;
use codex_core::protocol::BackgroundEventKind;
use codex_core::protocol::BackgroundShellPollEvent;
use codex_core::protocol::BackgroundShellStatus;
use codex_core::protocol::BackgroundShellSummaryEntry;
use codex_core::protocol::BackgroundStartMode;
use codex_core::protocol::BackgroundTerminationKind;
use codex_core::protocol::ShellPromotedEvent;
use indexmap::IndexMap;

#[derive(Clone, Debug)]
pub(crate) struct BackgroundProcess {
    pub shell_id: String,
    pub bookmark: Option<String>,
    pub description: Option<String>,
    pub status: BackgroundShellStatus,
    pub exit_code: Option<i32>,
    pub tail_lines: Vec<String>,
    pub command_preview: Option<String>,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
    pub ended_by: Option<String>,
    log_lines: VecDeque<String>,
    log_truncated: bool,
    last_update: Instant,
}

impl BackgroundProcess {
    fn new(shell_id: String) -> Self {
        Self {
            shell_id,
            bookmark: None,
            description: None,
            status: BackgroundShellStatus::Running,
            exit_code: None,
            tail_lines: Vec::new(),
            command_preview: None,
            started_at: None,
            finished_at: None,
            ended_by: None,
            log_lines: VecDeque::new(),
            log_truncated: false,
            last_update: Instant::now(),
        }
    }

    fn update_from_summary(&mut self, entry: &BackgroundShellSummaryEntry) {
        self.status = entry.status.clone();
        self.exit_code = entry.exit_code;
        if self.description.is_none() {
            self.description = entry.description.clone();
        }
        if entry.bookmark.is_some() {
            self.bookmark = entry.bookmark.clone();
        }
        self.command_preview = Some(entry.command_preview.clone());
        self.ended_by = entry.ended_by.clone();
        if let Some(ms) = entry.started_at_ms
            && ms >= 0
            && let Some(time) = UNIX_EPOCH.checked_add(Duration::from_millis(ms as u64))
        {
            self.started_at = Some(time);
        }
        if self.status != BackgroundShellStatus::Running {
            self.finished_at.get_or_insert_with(SystemTime::now);
        } else {
            self.finished_at = None;
        }
        self.tail_lines = entry.tail_lines.clone();
        if self.log_lines.is_empty() && !self.tail_lines.is_empty() {
            let snapshot = self.tail_lines.clone();
            self.push_log_lines(&snapshot);
        }
        self.last_update = Instant::now();
    }

    fn push_log_lines(&mut self, lines: &[String]) {
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            self.log_lines.push_back(line.clone());
        }
        while self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.pop_front();
            self.log_truncated = true;
        }
    }

    pub fn log_snapshot(&self, limit: usize) -> Vec<String> {
        self.log_lines
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

const INDICATOR_FLASH_DURATION: Duration = Duration::from_secs(3);
const MAX_LOG_LINES: usize = 200;

#[derive(Clone, Debug, Default)]
pub(crate) struct BackgroundProcessStore {
    processes: IndexMap<String, BackgroundProcess>,
    flash_deadline: Option<Instant>,
}

impl BackgroundProcessStore {
    pub fn new() -> Self {
        Self {
            processes: IndexMap::new(),
            flash_deadline: None,
        }
    }

    pub fn on_shell_promoted(&mut self, event: &ShellPromotedEvent) {
        let mut process = BackgroundProcess::new(event.shell_id.clone());
        process.description = event.description.clone();
        process.bookmark = event.bookmark.clone();
        process.command_preview = event.description.clone();
        process.tail_lines = event
            .initial_output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(std::string::ToString::to_string)
            .collect();
        process.started_at = Some(SystemTime::now());
        self.processes.insert(event.shell_id.clone(), process);
        self.flash_deadline = Some(Instant::now() + INDICATOR_FLASH_DURATION);
    }

    pub fn update_summary(&mut self, entries: &[BackgroundShellSummaryEntry]) {
        for entry in entries {
            let process = self
                .processes
                .entry(entry.shell_id.clone())
                .or_insert_with(|| BackgroundProcess::new(entry.shell_id.clone()));
            process.update_from_summary(entry);
        }
    }

    pub fn apply_event(&mut self, event: ParsedBackgroundEvent) {
        match event.kind {
            ParsedBackgroundEventKind::Started { description, .. } => {
                let process = self
                    .processes
                    .entry(event.shell_id.clone())
                    .or_insert_with(|| BackgroundProcess::new(event.shell_id.clone()));
                process.status = BackgroundShellStatus::Running;
                process.exit_code = None;
                if process.description.is_none() {
                    process.description = description;
                }
                process.started_at.get_or_insert(SystemTime::now());
                process.finished_at = None;
                process.last_update = Instant::now();
                self.flash_deadline = Some(Instant::now() + INDICATOR_FLASH_DURATION);
            }
            ParsedBackgroundEventKind::Terminated {
                exit_code,
                description,
                ..
            } => {
                if let Some(process) = self.processes.get_mut(&event.shell_id) {
                    process.status = if exit_code == 0 {
                        BackgroundShellStatus::Completed
                    } else {
                        BackgroundShellStatus::Failed
                    };
                    process.exit_code = Some(exit_code);
                    if process.description.is_none() {
                        process.description = description;
                    }
                    process.finished_at.get_or_insert(SystemTime::now());
                    process.last_update = Instant::now();
                    self.flash_deadline = Some(Instant::now() + INDICATOR_FLASH_DURATION);
                }
            }
        }
    }

    pub fn indicator(&self) -> Option<BackgroundIndicator> {
        if self.processes.is_empty() {
            return None;
        }
        let mut running = 0usize;
        let mut failed = 0usize;
        for process in self.processes.values() {
            match process.status {
                BackgroundShellStatus::Running => running += 1,
                BackgroundShellStatus::Failed => failed += 1,
                BackgroundShellStatus::Completed => {}
            }
        }
        let latest_label = self
            .processes
            .values()
            .rev()
            .find_map(BackgroundProcess::display_label);
        if running == 0 {
            return None;
        }

        let flash_deadline = self.flash_deadline.and_then(|deadline| {
            if deadline > Instant::now() {
                Some(deadline)
            } else {
                None
            }
        });

        Some(BackgroundIndicator {
            running,
            failed,
            total: self.processes.len(),
            latest_label,
            flash_deadline,
        })
    }

    pub fn snapshot(&self) -> Vec<BackgroundProcess> {
        self.processes.values().cloned().collect()
    }

    pub fn label_for(&self, shell_id: &str) -> Option<String> {
        self.processes
            .get(shell_id)
            .map(BackgroundProcess::command_display)
    }

    pub fn command_preview(&self, shell_id: &str) -> Option<String> {
        self.processes.get(shell_id).and_then(|proc| {
            proc.friendly_command_preview()
                .or_else(|| proc.description.clone().and_then(non_empty_owned))
        })
    }

    pub fn latest_output_line(&self, shell_id: &str) -> Option<String> {
        self.processes
            .get(shell_id)
            .and_then(BackgroundProcess::latest_output)
    }

    pub fn on_poll_event(&mut self, event: &BackgroundShellPollEvent) {
        let process = self
            .processes
            .entry(event.shell_id.clone())
            .or_insert_with(|| BackgroundProcess::new(event.shell_id.clone()));
        process.apply_poll_update(event);
        if matches!(
            process.status,
            BackgroundShellStatus::Completed | BackgroundShellStatus::Failed
        ) {
            process.finished_at.get_or_insert_with(SystemTime::now);
        }
        self.flash_deadline = Some(Instant::now() + INDICATOR_FLASH_DURATION);
    }

    pub fn purge_finished(&mut self) {
        self.processes
            .retain(|_, process| matches!(process.status, BackgroundShellStatus::Running));
    }
}

impl BackgroundProcess {
    pub(crate) fn label(&self) -> String {
        format!("Shell {}", short_numeric_id(&self.shell_id))
    }

    fn display_label(&self) -> Option<String> {
        Some(self.command_display())
    }

    pub fn command_display(&self) -> String {
        let base = self
            .friendly_command_preview()
            .or_else(|| self.description.clone().and_then(non_empty_owned))
            .unwrap_or_else(|| self.label());
        if let Some(bookmark) = self
            .bookmark
            .as_deref()
            .and_then(|alias| (!alias.trim().is_empty()).then(|| alias.trim()))
        {
            let bookmark_tag = format!("#{bookmark}");
            if base.contains(&bookmark_tag) {
                base
            } else if base == self.label() {
                bookmark_tag
            } else {
                format!("{bookmark_tag} · {base}")
            }
        } else {
            base
        }
    }

    pub fn runtime(&self) -> Option<Duration> {
        let start = self.started_at?;
        let end = match self.status {
            BackgroundShellStatus::Running => SystemTime::now(),
            _ => self.finished_at.unwrap_or_else(SystemTime::now),
        };
        end.duration_since(start).ok()
    }

    pub fn latest_output(&self) -> Option<String> {
        self.tail_lines
            .last()
            .cloned()
            .map(|line| line.trim().to_string())
    }

    fn friendly_command_preview(&self) -> Option<String> {
        self.command_preview
            .as_deref()
            .and_then(friendly_command_label_from_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub fn log_truncated(&self) -> bool {
        self.log_truncated
    }

    pub fn apply_poll_update(&mut self, event: &BackgroundShellPollEvent) {
        self.status = event.status.clone();
        self.exit_code = event.exit_code;
        if event.bookmark.is_some() {
            self.bookmark = event.bookmark.clone();
        }
        if matches!(self.status, BackgroundShellStatus::Running) {
            self.finished_at = None;
        } else {
            self.finished_at.get_or_insert_with(SystemTime::now);
        }
        if !event.lines.is_empty() {
            self.tail_lines = event.lines.clone();
            self.push_log_lines(&event.lines);
        }
        if event.truncated {
            self.log_truncated = true;
        }
        self.last_update = Instant::now();
    }
}

pub(crate) fn short_numeric_id(shell_id: &str) -> String {
    let mut hash: u32 = 0;
    for byte in shell_id.as_bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(*byte as u32);
    }
    format!("{:06}", hash % 1_000_000)
}

fn non_empty_owned(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackgroundIndicator {
    pub running: usize,
    pub failed: usize,
    pub total: usize,
    pub latest_label: Option<String>,
    pub flash_deadline: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedBackgroundEvent {
    pub shell_id: String,
    pub call_id: Option<String>,
    pub kind: ParsedBackgroundEventKind,
}

#[derive(Debug, Clone)]
pub(crate) enum ParsedBackgroundEventKind {
    Started {
        description: Option<String>,
        mode: Option<BackgroundStartMode>,
    },
    Terminated {
        exit_code: i32,
        description: Option<String>,
        termination: Option<BackgroundTerminationKind>,
    },
}

pub(crate) fn parse_background_event(
    event: &BackgroundEventEvent,
) -> Option<ParsedBackgroundEvent> {
    let metadata = event.metadata.as_ref()?;
    let shell_id = metadata.shell_id.clone()?;
    let kind = metadata.kind.as_ref()?;
    let parsed_kind = match kind {
        BackgroundEventKind::Started { description, mode } => ParsedBackgroundEventKind::Started {
            description: description.clone(),
            mode: Some(mode.clone()),
        },
        BackgroundEventKind::Terminated {
            exit_code,
            termination,
            description,
        } => ParsedBackgroundEventKind::Terminated {
            exit_code: *exit_code,
            description: description.clone(),
            termination: Some(termination.clone()),
        },
    };
    Some(ParsedBackgroundEvent {
        shell_id,
        call_id: metadata.call_id.clone(),
        kind: parsed_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::protocol::BackgroundEventMetadata;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_event_with_metadata() {
        let event = parse_background_event(&BackgroundEventEvent {
            message: "unused".to_string(),
            metadata: Some(BackgroundEventMetadata {
                shell_id: Some("sh-42".into()),
                call_id: Some("call-1".into()),
                kind: Some(BackgroundEventKind::Started {
                    description: Some("npm run build".into()),
                    mode: BackgroundStartMode::ManualPromotion,
                }),
            }),
        })
        .expect("event");
        assert_eq!(event.shell_id, "sh-42");
        assert_eq!(event.call_id.as_deref(), Some("call-1"));
        match event.kind {
            ParsedBackgroundEventKind::Started { description, .. } => {
                assert_eq!(description.as_deref(), Some("npm run build"));
            }
            _ => panic!("expected Started"),
        }
    }

    #[test]
    fn indicator_tracks_running_and_failed_processes() {
        let mut store = BackgroundProcessStore::new();
        store.on_shell_promoted(&ShellPromotedEvent {
            call_id: "call-1".into(),
            shell_id: "shell-1".into(),
            initial_output: "warming up".into(),
            description: Some("npm start".into()),
            bookmark: Some("build".into()),
        });

        let indicator = store.indicator().expect("indicator");
        assert_eq!(indicator.running, 1);
        assert_eq!(indicator.failed, 0);
        assert_eq!(indicator.total, 1);
        assert_eq!(
            indicator.latest_label.as_deref(),
            Some("#build · npm start")
        );
        assert!(indicator.flash_deadline.is_some());

        store.apply_event(ParsedBackgroundEvent {
            shell_id: "shell-1".into(),
            call_id: None,
            kind: ParsedBackgroundEventKind::Terminated {
                exit_code: 1,
                description: Some("npm start".into()),
                termination: Some(BackgroundTerminationKind::Natural),
            },
        });

        assert!(store.indicator().is_none());
    }

    #[test]
    fn label_for_prefers_command_preview_then_bookmark() {
        let mut store = BackgroundProcessStore::new();
        store.on_shell_promoted(&ShellPromotedEvent {
            call_id: "call-7".into(),
            shell_id: "shell-7".into(),
            initial_output: String::new(),
            description: Some("npm run dev".into()),
            bookmark: Some("devserver".into()),
        });

        assert_eq!(
            store.label_for("shell-7").as_deref(),
            Some("#devserver · npm run dev")
        );

        if let Some(process) = store.processes.get_mut("shell-7") {
            process.command_preview = None;
            process.description = None;
        }

        assert_eq!(store.label_for("shell-7").as_deref(), Some("#devserver"));

        if let Some(process) = store.processes.get_mut("shell-7") {
            process.bookmark = None;
        }

        let fallback = store.label_for("shell-7").expect("label");
        assert!(fallback.starts_with("Shell "));
    }
}
