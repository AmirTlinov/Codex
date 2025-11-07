use std::time::Duration;
use std::time::Instant;

use codex_protocol::parse_command::ParsedCommand;

#[derive(Clone, Debug)]
pub(crate) struct CommandOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) aggregated_output: String,
    pub(crate) formatted_output: String,
    pub(crate) stdout_collapsed: bool,
    pub(crate) stderr_collapsed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecStreamKind {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecStreamAction {
    Expand,
    Collapse,
    Toggle,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecCall {
    pub(crate) call_id: String,
    pub(crate) command: Vec<String>,
    pub(crate) parsed: Vec<ParsedCommand>,
    pub(crate) output: Option<CommandOutput>,
    pub(crate) start_time: Option<Instant>,
    pub(crate) duration: Option<Duration>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecCell {
    pub(crate) calls: Vec<ExecCall>,
}

const STREAM_AUTO_COLLAPSE_LINE_THRESHOLD: usize = 20;
const STREAM_AUTO_COLLAPSE_CHAR_THRESHOLD: usize = 2_000;

impl ExecCell {
    pub(crate) fn new(call: ExecCall) -> Self {
        Self { calls: vec![call] }
    }

    pub(crate) fn with_added_call(
        &self,
        call_id: String,
        command: Vec<String>,
        parsed: Vec<ParsedCommand>,
    ) -> Option<Self> {
        let call = ExecCall {
            call_id,
            command,
            parsed,
            output: None,
            start_time: Some(Instant::now()),
            duration: None,
        };
        if self.is_exploring_cell() && Self::is_exploring_call(&call) {
            Some(Self {
                calls: [self.calls.clone(), vec![call]].concat(),
            })
        } else {
            None
        }
    }

    pub(crate) fn complete_call(
        &mut self,
        call_id: &str,
        output: CommandOutput,
        duration: Duration,
    ) {
        if let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) {
            call.output = Some(output);
            call.duration = Some(duration);
            call.start_time = None;
        }
    }

    pub(crate) fn should_flush(&self) -> bool {
        !self.is_exploring_cell() && self.calls.iter().all(|c| c.output.is_some())
    }

    pub(crate) fn mark_failed(&mut self) {
        for call in self.calls.iter_mut() {
            if call.output.is_none() {
                let elapsed = call
                    .start_time
                    .map(|st| st.elapsed())
                    .unwrap_or_else(|| Duration::from_millis(0));
                call.start_time = None;
                call.duration = Some(elapsed);
                call.output = Some(CommandOutput::new(
                    1,
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ));
            }
        }
    }

    pub(crate) fn is_exploring_cell(&self) -> bool {
        self.calls.iter().all(Self::is_exploring_call)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.calls.iter().any(|c| c.output.is_none())
    }

    pub(crate) fn active_start_time(&self) -> Option<Instant> {
        self.calls
            .iter()
            .find(|c| c.output.is_none())
            .and_then(|c| c.start_time)
    }

    pub(crate) fn iter_calls(&self) -> impl Iterator<Item = &ExecCall> {
        self.calls.iter()
    }

    pub(super) fn is_exploring_call(call: &ExecCall) -> bool {
        !call.parsed.is_empty()
            && call.parsed.iter().all(|p| {
                matches!(
                    p,
                    ParsedCommand::Read { .. }
                        | ParsedCommand::ListFiles { .. }
                        | ParsedCommand::Search { .. }
                        | ParsedCommand::Write { .. }
                        | ParsedCommand::Run { .. }
                )
            })
    }

    pub(crate) fn contains_call(&self, call_id: &str) -> bool {
        self.calls.iter().any(|call| call.call_id == call_id)
    }

    pub(crate) fn last_completed_call_id(&self) -> Option<&str> {
        self.calls
            .iter()
            .rev()
            .find(|call| call.output.is_some())
            .map(|call| call.call_id.as_str())
    }

    pub(crate) fn call_output(&self, call_id: &str) -> Option<&CommandOutput> {
        self.calls
            .iter()
            .find(|call| call.call_id == call_id)
            .and_then(|call| call.output.as_ref())
    }

    pub(crate) fn apply_stream_action(
        &self,
        call_id: &str,
        streams: &[ExecStreamKind],
        action: ExecStreamAction,
    ) -> Option<Self> {
        let mut cloned = self.clone();
        let mut changed = false;
        for call in cloned.calls.iter_mut() {
            if call.call_id != call_id {
                continue;
            }
            if let Some(output) = call.output.as_mut() {
                for stream in streams {
                    changed |= output.apply_stream_action(*stream, action);
                }
            }
        }
        if changed { Some(cloned) } else { None }
    }
}

impl CommandOutput {
    fn default_collapsed(text: &str) -> bool {
        let line_count = text.lines().count();
        line_count > STREAM_AUTO_COLLAPSE_LINE_THRESHOLD
            || text.len() > STREAM_AUTO_COLLAPSE_CHAR_THRESHOLD
    }

    pub(crate) fn new(
        exit_code: i32,
        stdout: String,
        stderr: String,
        aggregated_output: String,
        formatted_output: String,
    ) -> Self {
        let stdout_collapsed = Self::default_collapsed(&stdout);
        let stderr_collapsed = Self::default_collapsed(&stderr);
        Self {
            exit_code,
            stdout,
            stderr,
            aggregated_output,
            formatted_output,
            stdout_collapsed,
            stderr_collapsed,
        }
    }

    pub(crate) fn stream_collapsed(&self, kind: ExecStreamKind) -> bool {
        match kind {
            ExecStreamKind::Stdout => self.stdout_collapsed,
            ExecStreamKind::Stderr => self.stderr_collapsed,
        }
    }

    pub(crate) fn set_stream_collapsed(&mut self, kind: ExecStreamKind, collapsed: bool) -> bool {
        let slot = match kind {
            ExecStreamKind::Stdout => &mut self.stdout_collapsed,
            ExecStreamKind::Stderr => &mut self.stderr_collapsed,
        };
        if *slot == collapsed {
            false
        } else {
            *slot = collapsed;
            true
        }
    }

    pub(crate) fn apply_stream_action(
        &mut self,
        kind: ExecStreamKind,
        action: ExecStreamAction,
    ) -> bool {
        match action {
            ExecStreamAction::Expand => self.set_stream_collapsed(kind, false),
            ExecStreamAction::Collapse => self.set_stream_collapsed(kind, true),
            ExecStreamAction::Toggle => {
                let new_state = !self.stream_collapsed(kind);
                self.set_stream_collapsed(kind, new_state)
            }
        }
    }
}

impl ExecStreamKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ExecStreamKind::Stdout => "stdout",
            ExecStreamKind::Stderr => "stderr",
        }
    }
}
