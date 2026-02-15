use crate::history_cell::PlainHistoryCell;
use crate::render::line_utils::prefix_lines;
use crate::text_formatting::capitalize_first;
use crate::text_formatting::truncate_text;
use codex_core::protocol::AgentStatus;
use codex_core::protocol::CollabAgentInteractionEndEvent;
use codex_core::protocol::CollabAgentSpawnEndEvent;
use codex_core::protocol::CollabCloseEndEvent;
use codex_core::protocol::CollabResumeBeginEvent;
use codex_core::protocol::CollabResumeEndEvent;
use codex_core::protocol::CollabWaitingBeginEvent;
use codex_core::protocol::CollabWaitingEndEvent;
use codex_protocol::ThreadId;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

const COLLAB_PROMPT_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES: usize = 240;

const AGENT_HANDLE_PREFIX: &str = "@";

fn agent_color(thread_id: &ThreadId) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
    ];
    let mut hasher = DefaultHasher::new();
    thread_id.hash(&mut hasher);
    let idx = (hasher.finish() % PALETTE.len() as u64) as usize;
    PALETTE[idx]
}

fn agent_handle_label(agent_type: Option<&str>) -> String {
    let base = agent_type.unwrap_or("agent");
    format!("{AGENT_HANDLE_PREFIX}{base}")
}

fn agent_handle_span(thread_id: &ThreadId, agent_type: Option<&str>) -> Span<'static> {
    let label = agent_handle_label(agent_type);
    Span::styled(
        label,
        Style::default()
            .fg(agent_color(thread_id))
            .add_modifier(Modifier::BOLD),
    )
}

fn highlight_mentions(text: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (idx, token) in text.split_whitespace().enumerate() {
        if idx > 0 {
            spans.push(Span::from(" "));
        }
        let span = if token.starts_with(AGENT_HANDLE_PREFIX) && token.len() > 1 {
            Span::from(token.to_string()).cyan().underlined()
        } else {
            Span::from(token.to_string())
        };
        spans.push(span);
    }
    spans.into()
}

fn agent_type_span(agent_type: &str) -> Span<'static> {
    Span::from(capitalize_first(agent_type)).bold()
}

fn agent_ref_spans(thread_id: &ThreadId, agent_type: Option<&str>) -> Vec<Span<'static>> {
    if let Some(agent_type) = agent_type {
        vec![
            agent_type_span(agent_type),
            Span::from(" ").dim(),
            Span::from(thread_id.to_string()).dim(),
        ]
    } else {
        vec![Span::from(thread_id.to_string()).dim()]
    }
}

fn agents_list_spans(
    ids: &[ThreadId],
    agent_types: &HashMap<ThreadId, String>,
) -> Vec<Span<'static>> {
    if ids.is_empty() {
        return vec![Span::from("none").dim()];
    }

    let mut spans = Vec::new();
    for (idx, id) in ids.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::from(", ").dim());
        }
        spans.extend(agent_ref_spans(id, agent_types.get(id).map(String::as_str)));
    }
    spans
}

pub(crate) fn spawn_end(ev: CollabAgentSpawnEndEvent) -> PlainHistoryCell {
    let CollabAgentSpawnEndEvent {
        call_id,
        sender_thread_id: _,
        new_thread_id,
        agent_type,
        prompt,
        status,
    } = ev;
    let new_agent = new_thread_id
        .map(|id| detail_line_spans("agent", agent_ref_spans(&id, agent_type.as_deref())));
    let mut details = vec![
        detail_line("call", call_id),
        new_agent.unwrap_or_else(|| detail_line("agent", Span::from("not created").dim())),
        status_line(&status),
    ];
    if let Some(line) = prompt_line(&prompt) {
        details.push(line);
    }
    collab_event("Agent spawned", details)
}

pub(crate) fn interaction_end(
    ev: CollabAgentInteractionEndEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> PlainHistoryCell {
    let CollabAgentInteractionEndEvent {
        call_id,
        sender_thread_id: _,
        receiver_thread_id,
        prompt,
        status,
    } = ev;
    let mut details = vec![
        detail_line("call", call_id),
        detail_line_spans(
            "receiver",
            agent_ref_spans(
                &receiver_thread_id,
                agent_types.get(&receiver_thread_id).map(String::as_str),
            ),
        ),
        status_line(&status),
    ];
    if let Some(line) = prompt_line(&prompt) {
        details.push(line);
    }
    collab_event("Input sent", details)
}

pub(crate) fn waiting_begin(
    ev: CollabWaitingBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> PlainHistoryCell {
    let CollabWaitingBeginEvent {
        call_id,
        sender_thread_id: _,
        receiver_thread_ids,
    } = ev;
    let details = vec![
        detail_line("call", call_id),
        detail_line_spans(
            "receivers",
            agents_list_spans(&receiver_thread_ids, agent_types),
        ),
    ];
    collab_event("Waiting for agents", details)
}

pub(crate) fn waiting_end(
    ev: CollabWaitingEndEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> Vec<PlainHistoryCell> {
    let CollabWaitingEndEvent {
        call_id: _,
        sender_thread_id,
        statuses,
    } = ev;

    let sender_type = agent_types.get(&sender_thread_id).map(String::as_str);
    let mut cells = Vec::new();

    let summary = wait_summary_text(&statuses);
    cells.push(PlainHistoryCell::new(vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        highlight_mentions(&format!("Wait complete: {summary}")),
    ]));

    let mut entries: Vec<(ThreadId, &AgentStatus)> = statuses
        .iter()
        .map(|(thread_id, status)| (*thread_id, status))
        .collect();
    entries.sort_by(|(left, _), (right, _)| left.to_string().cmp(&right.to_string()));

    for (thread_id, status) in entries {
        let agent_type = agent_types.get(&thread_id).map(String::as_str);
        match status {
            AgentStatus::Completed(Some(message)) => {
                let message_preview = truncate_text(
                    &message.split_whitespace().collect::<Vec<_>>().join(" "),
                    COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES,
                );
                cells.push(agent_message_cell(
                    &thread_id,
                    agent_type,
                    status,
                    highlight_mentions(&message_preview),
                ));
            }
            AgentStatus::Errored(error) => {
                let error_preview = truncate_text(
                    &error.split_whitespace().collect::<Vec<_>>().join(" "),
                    COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES,
                );
                cells.push(agent_message_cell(
                    &thread_id,
                    agent_type,
                    status,
                    Line::from(error_preview).red().dim(),
                ));
            }
            _ => {}
        }
    }

    cells
}

pub(crate) fn close_end(
    ev: CollabCloseEndEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> PlainHistoryCell {
    let CollabCloseEndEvent {
        call_id,
        sender_thread_id: _,
        receiver_thread_id,
        status,
    } = ev;
    let details = vec![
        detail_line("call", call_id),
        detail_line_spans(
            "receiver",
            agent_ref_spans(
                &receiver_thread_id,
                agent_types.get(&receiver_thread_id).map(String::as_str),
            ),
        ),
        status_line(&status),
    ];
    collab_event("Agent closed", details)
}

pub(crate) fn resume_begin(
    ev: CollabResumeBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> PlainHistoryCell {
    let CollabResumeBeginEvent {
        call_id,
        sender_thread_id: _,
        receiver_thread_id,
    } = ev;
    let details = vec![
        detail_line("call", call_id),
        detail_line_spans(
            "receiver",
            agent_ref_spans(
                &receiver_thread_id,
                agent_types.get(&receiver_thread_id).map(String::as_str),
            ),
        ),
    ];
    collab_event("Resuming agent", details)
}

pub(crate) fn resume_end(
    ev: CollabResumeEndEvent,
    agent_types: &HashMap<ThreadId, String>,
) -> PlainHistoryCell {
    let CollabResumeEndEvent {
        call_id,
        sender_thread_id: _,
        receiver_thread_id,
        status,
    } = ev;
    let details = vec![
        detail_line("call", call_id),
        detail_line_spans(
            "receiver",
            agent_ref_spans(
                &receiver_thread_id,
                agent_types.get(&receiver_thread_id).map(String::as_str),
            ),
        ),
        status_line(&status),
    ];
    collab_event("Agent resumed", details)
}

fn collab_event(title: impl Into<String>, details: Vec<Line<'static>>) -> PlainHistoryCell {
    let title = title.into();
    let mut lines: Vec<Line<'static>> =
        vec![vec![Span::from("• ").dim(), Span::from(title).bold()].into()];
    if !details.is_empty() {
        lines.extend(prefix_lines(details, "  └ ".dim(), "    ".into()));
    }
    PlainHistoryCell::new(lines)
}

fn wait_summary_text(statuses: &HashMap<ThreadId, AgentStatus>) -> String {
    if statuses.is_empty() {
        return "none".to_string();
    }

    let mut pending_init = 0usize;
    let mut running = 0usize;
    let mut completed = 0usize;
    let mut errored = 0usize;
    let mut shutdown = 0usize;
    let mut not_found = 0usize;
    for status in statuses.values() {
        match status {
            AgentStatus::PendingInit => pending_init += 1,
            AgentStatus::Running => running += 1,
            AgentStatus::Completed(_) => completed += 1,
            AgentStatus::Errored(_) => errored += 1,
            AgentStatus::Shutdown => shutdown += 1,
            AgentStatus::NotFound => not_found += 1,
        }
    }

    let mut parts = vec![format!("{} total", statuses.len())];
    if pending_init > 0 {
        parts.push(format!("{pending_init} pending init"));
    }
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if completed > 0 {
        parts.push(format!("{completed} completed"));
    }
    if errored > 0 {
        parts.push(format!("{errored} errored"));
    }
    if shutdown > 0 {
        parts.push(format!("{shutdown} shutdown"));
    }
    if not_found > 0 {
        parts.push(format!("{not_found} not found"));
    }
    parts.join(" · ")
}

fn agent_message_cell(
    thread_id: &ThreadId,
    agent_type: Option<&str>,
    status: &AgentStatus,
    message: Line<'static>,
) -> PlainHistoryCell {
    PlainHistoryCell::new(vec![
        vec![
            agent_handle_span(thread_id, agent_type),
            Span::from(" (").dim(),
            status_span(status),
            Span::from(")").dim(),
        ]
        .into(),
        message,
    ])
}

fn detail_line(label: &str, value: impl Into<Span<'static>>) -> Line<'static> {
    vec![Span::from(format!("{label}: ")).dim(), value.into()].into()
}

fn status_line(status: &AgentStatus) -> Line<'static> {
    detail_line("status", status_span(status))
}

fn status_span(status: &AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::PendingInit => Span::from("pending init").dim(),
        AgentStatus::Running => Span::from("running").cyan().bold(),
        AgentStatus::Completed(_) => Span::from("completed").green(),
        AgentStatus::Errored(_) => Span::from("errored").red(),
        AgentStatus::Shutdown => Span::from("shutdown").dim(),
        AgentStatus::NotFound => Span::from("not found").red(),
    }
}

fn prompt_line(prompt: &str) -> Option<Line<'static>> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(detail_line(
            "prompt",
            Span::from(truncate_text(trimmed, COLLAB_PROMPT_PREVIEW_GRAPHEMES)).dim(),
        ))
    }
}

fn detail_line_spans(label: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = Vec::with_capacity(value.len() + 1);
    spans.push(Span::from(format!("{label}: ")).dim());
    spans.append(&mut value);
    spans.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::HistoryCell;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;

    #[test]
    fn waiting_end_renders_chat_like_agent_messages() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let reviewer_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();
        let devops_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000003").unwrap();
        let scout_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000004").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "orchestrator".to_string());
        agent_types.insert(reviewer_thread_id, "review-agent".to_string());
        agent_types.insert(devops_thread_id, "devops-engineer".to_string());
        agent_types.insert(scout_thread_id, "scout".to_string());

        let mut statuses = HashMap::new();
        statuses.insert(
            reviewer_thread_id,
            AgentStatus::Completed(Some(
                "I deployed the project, awaiting response from @review-agent.".to_string(),
            )),
        );
        statuses.insert(
            devops_thread_id,
            AgentStatus::Errored("Timeout while waiting for @devops-engineer".to_string()),
        );
        statuses.insert(scout_thread_id, AgentStatus::Running);

        let cells = waiting_end(
            CollabWaitingEndEvent {
                sender_thread_id,
                call_id: "call-1".to_string(),
                statuses,
            },
            &agent_types,
        );

        let summary_lines = cells[0].display_lines(80);
        let handle_span = &summary_lines[0].spans[0];
        assert!(handle_span.style.fg.is_some(), "expected handle color");
        assert!(
            handle_span.style.add_modifier.contains(Modifier::BOLD),
            "expected handle to be bold",
        );

        let message_lines = cells[1].display_lines(80);
        let mention_span = message_lines[1]
            .spans
            .iter()
            .find(|span| span.content.starts_with(AGENT_HANDLE_PREFIX))
            .expect("expected @mention span");
        assert_eq!(mention_span.style.fg, Some(Color::Cyan));
        assert!(
            mention_span
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED),
            "expected @mention to be underlined",
        );

        let mut rendered = String::new();
        for (idx, cell) in cells.iter().enumerate() {
            if idx > 0 {
                rendered.push_str("\n---\n");
            }
            for line in cell.display_lines(80) {
                let text = line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>();
                rendered.push_str(&text);
                rendered.push('\n');
            }
        }

        assert_snapshot!(rendered.trim_end());
    }
}
