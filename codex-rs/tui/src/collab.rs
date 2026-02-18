use crate::history_cell::PlainHistoryCell;
use crate::history_cell::PrefixedWrappedHistoryCell;
use crate::history_cell::TranscriptFeed;
use crate::markdown::append_markdown;
use crate::text_formatting::truncate_text;
use codex_core::protocol::AgentStatus;
use codex_core::protocol::CollabAgentIdentity;
use codex_core::protocol::CollabAgentInteractionBeginEvent;
use codex_core::protocol::CollabAgentInteractionEndEvent;
use codex_core::protocol::CollabAgentSpawnBeginEvent;
use codex_core::protocol::CollabAgentSpawnEndEvent;
use codex_core::protocol::CollabCloseBeginEvent;
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
use ratatui::text::Text;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use unicode_width::UnicodeWidthStr;

const COLLAB_PROMPT_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES: usize = 160;
const COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES: usize = 240;

const AGENT_HANDLE_PREFIX: &str = "@";

fn color_from_token(token: &str) -> Option<Color> {
    match token.trim().to_ascii_lowercase().as_str() {
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        _ => None,
    }
}

fn parse_agent_identity(
    agent_type: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    Option<Color>,
    Option<String>,
) {
    let Some(raw_agent_type) = agent_type.map(str::trim).filter(|value| !value.is_empty()) else {
        return (None, None, None, None);
    };

    if let Some(identity) = CollabAgentIdentity::parse_agent_type_label(raw_agent_type) {
        let color = identity.color_token.as_deref().and_then(color_from_token);
        let display_name = identity
            .display_name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let role = identity.role_label;
        let handle = Some(identity.handle);
        return (handle, display_name, color, role);
    }

    let fallback = raw_agent_type.trim().trim_start_matches('@').trim();
    let handle = (!fallback.is_empty()).then(|| fallback.to_string());
    (handle, None, None, None)
}

fn agent_color(thread_id: &ThreadId, agent_type: Option<&str>) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
    ];

    let (handle, _display_name, explicit_color, _role) = parse_agent_identity(agent_type);
    if let Some(color) = explicit_color {
        return color;
    }

    let mut hasher = DefaultHasher::new();
    if let Some(handle) = handle {
        handle.hash(&mut hasher);
    } else {
        thread_id.hash(&mut hasher);
    }
    let idx = (hasher.finish() % PALETTE.len() as u64) as usize;
    PALETTE[idx]
}

fn short_thread_id(thread_id: &ThreadId) -> String {
    const LEN: usize = 6;
    let text = thread_id.to_string();
    let suffix = text.rsplit('-').next().unwrap_or(text.as_str());
    let start = suffix.len().saturating_sub(LEN);
    suffix[start..].to_string()
}

fn agent_handle_label(thread_id: &ThreadId, agent_type: Option<&str>) -> String {
    let (handle, display_name, _color, _role) = parse_agent_identity(agent_type);
    let base = match handle.as_deref() {
        Some("default" | "orchestrator" | "main") => "main".to_string(),
        Some(value) => value.to_string(),
        None => "agent".to_string(),
    };

    let mut label = format!("{AGENT_HANDLE_PREFIX}{base}");
    if let Some(display_name) = display_name.as_deref()
        && !display_name.eq_ignore_ascii_case(base.as_str())
    {
        label.push_str(" (");
        label.push_str(display_name);
        label.push(')');
    }
    if handle.is_none() {
        label.push('#');
        label.push_str(&short_thread_id(thread_id));
    }
    label
}

fn agent_handle_span(thread_id: &ThreadId, agent_type: Option<&str>) -> Span<'static> {
    let label = agent_handle_label(thread_id, agent_type);
    Span::styled(
        label,
        Style::default()
            .fg(agent_color(thread_id, agent_type))
            .add_modifier(Modifier::BOLD),
    )
}

fn highlight_mentions(text: &str) -> Line<'static> {
    highlight_mentions_in_line(&Line::from(text.to_string()))
}

fn is_mention_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '#')
}

fn is_mention_start_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(char::is_whitespace)
}

fn highlight_mentions_in_span(span: &Span<'static>) -> Vec<Span<'static>> {
    let text = span.content.as_ref();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut prev_char: Option<char> = None;

    for (idx, ch) in text.char_indices() {
        if ch == '@' && is_mention_start_boundary(prev_char) {
            if cursor < idx {
                out.push(Span::styled(text[cursor..idx].to_string(), span.style));
            }

            let mut end = idx + ch.len_utf8();
            for (mention_idx, mention_ch) in text[end..].char_indices() {
                if is_mention_char(mention_ch) {
                    end = idx + ch.len_utf8() + mention_idx + mention_ch.len_utf8();
                } else {
                    break;
                }
            }

            if end > idx + ch.len_utf8() {
                let mention_style = span
                    .style
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED);
                out.push(Span::styled(text[idx..end].to_string(), mention_style));
                cursor = end;
            } else {
                out.push(Span::styled(ch.to_string(), span.style));
                cursor = idx + ch.len_utf8();
            }
        }

        prev_char = Some(ch);
    }

    if cursor < text.len() {
        out.push(Span::styled(text[cursor..].to_string(), span.style));
    }

    if out.is_empty() {
        out.push(span.clone());
    }

    out
}

fn highlight_mentions_in_line(line: &Line<'static>) -> Line<'static> {
    let spans = line
        .spans
        .iter()
        .flat_map(highlight_mentions_in_span)
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style)
}

pub(crate) fn highlight_mentions_in_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(highlight_mentions_in_line)
        .collect::<Vec<_>>()
}

fn agent_handle_list_spans(
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
        spans.push(agent_handle_span(
            id,
            agent_types.get(id).map(String::as_str),
        ));
    }
    spans
}

fn sender_agent_type<'a>(
    sender_thread_id: &ThreadId,
    agent_types: &'a HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
) -> Option<&'a str> {
    if let Some(agent_type) = agent_types.get(sender_thread_id) {
        return Some(agent_type.as_str());
    }

    if main_thread_id == Some(sender_thread_id) {
        return Some("main");
    }

    None
}

fn is_private_scout(agent_type: Option<&str>) -> bool {
    let (handle, _display_name, _color, role) = parse_agent_identity(agent_type);
    matches!(
        role.as_deref().or(handle.as_deref()),
        Some("scout" | "explorer")
    )
}

fn single_line_preview(text: &str, max_graphemes: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_text(&normalized, max_graphemes)
}

fn debug_thread_field(name: &str, thread_id: ThreadId) -> String {
    format!("{name}={thread_id}")
}

fn debug_thread_list_field(name: &str, thread_ids: &[ThreadId]) -> String {
    let ids = thread_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}=[{ids}]")
}

fn maybe_push_debug_line(
    lines: &mut Vec<Line<'static>>,
    show_collab_debug_ids: bool,
    call_id: &str,
    mut fields: Vec<String>,
) {
    if !show_collab_debug_ids {
        return;
    }

    let mut parts = vec![format!("call_id={call_id}")];
    parts.append(&mut fields);
    lines.push(Line::from(format!("[debug] {}", parts.join(" "))).dim());
}

pub(crate) fn agent_message(
    thread_id: ThreadId,
    message: String,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
) -> PrefixedWrappedHistoryCell {
    let agent_type = sender_agent_type(&thread_id, agent_types, main_thread_id);
    let label = agent_handle_label(&thread_id, agent_type);
    let initial_prefix = Line::from(vec![agent_handle_span(&thread_id, agent_type), " ".into()]);
    let subsequent_prefix = " ".repeat(UnicodeWidthStr::width(label.as_str()) + 1);

    let mut lines = Vec::new();
    append_markdown(&message, None, &mut lines);
    PrefixedWrappedHistoryCell::new(
        Text::from(highlight_mentions_in_lines(lines)),
        initial_prefix,
        subsequent_prefix,
    )
    .with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn agent_message_hidden(
    thread_id: ThreadId,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
) -> PlainHistoryCell {
    let agent_type = sender_agent_type(&thread_id, agent_types, main_thread_id);
    PlainHistoryCell::new(vec![
        vec![agent_handle_span(&thread_id, agent_type)].into(),
        vec![
            Span::from("Response received").dim(),
            Span::from(" (details hidden)").dim(),
        ]
        .into(),
    ])
    .with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn agent_message_prefixes(
    thread_id: &ThreadId,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
) -> (Line<'static>, Line<'static>) {
    let agent_type = sender_agent_type(thread_id, agent_types, main_thread_id);
    let label = agent_handle_label(thread_id, agent_type);
    (
        Line::from(vec![agent_handle_span(thread_id, agent_type), " ".into()]),
        Line::from(" ".repeat(UnicodeWidthStr::width(label.as_str()) + 1)),
    )
}

pub(crate) fn spawn_begin(
    ev: CollabAgentSpawnBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabAgentSpawnBeginEvent {
        call_id,
        sender_thread_id,
        agent_type,
        prompt,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let mut lines = vec![vec![agent_handle_span(&sender_thread_id, sender_type)].into()];

    let (handle, display_name, color, _role) = parse_agent_identity(agent_type.as_deref());
    let target_span = if let Some(handle) = handle {
        let mut label = format!("{AGENT_HANDLE_PREFIX}{handle}");
        if let Some(display_name) = display_name.as_deref()
            && !display_name.eq_ignore_ascii_case(handle.as_str())
        {
            label.push_str(" (");
            label.push_str(display_name);
            label.push(')');
        }
        let style = color.map_or_else(
            || {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            },
            |color| Style::default().fg(color).add_modifier(Modifier::BOLD),
        );
        Span::styled(label, style)
    } else {
        Span::from("agent").dim()
    };

    lines.push(vec![Span::from("Spawning ").dim(), target_span].into());

    if !prompt.trim().is_empty() && !is_private_scout(agent_type.as_deref()) {
        let preview = single_line_preview(&prompt, COLLAB_PROMPT_PREVIEW_GRAPHEMES);
        lines.push(highlight_mentions(&preview).dim());
    }

    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![debug_thread_field("sender_thread_id", sender_thread_id)],
    );

    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn spawn_end(
    ev: CollabAgentSpawnEndEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabAgentSpawnEndEvent {
        call_id,
        sender_thread_id,
        new_thread_id,
        agent_type,
        prompt,
        status,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let mut lines = vec![vec![agent_handle_span(&sender_thread_id, sender_type)].into()];

    match new_thread_id {
        Some(new_thread_id) => {
            let new_type = agent_types
                .get(&new_thread_id)
                .map(String::as_str)
                .or(agent_type.as_deref());
            lines.push(
                vec![
                    Span::from("Spawned ").dim(),
                    agent_handle_span(&new_thread_id, new_type),
                    Span::from(" (").dim(),
                    status_span(&status),
                    Span::from(")").dim(),
                ]
                .into(),
            );
        }
        None => {
            lines.push(
                vec![
                    Span::from("Spawn failed: ").red().dim(),
                    Span::from("agent not created").red().dim(),
                    Span::from(" (").dim(),
                    status_span(&status),
                    Span::from(")").dim(),
                ]
                .into(),
            );
        }
    }

    if !prompt.trim().is_empty() && !is_private_scout(agent_type.as_deref()) {
        let preview = single_line_preview(&prompt, COLLAB_PROMPT_PREVIEW_GRAPHEMES);
        lines.push(highlight_mentions(&preview).dim());
    }

    let mut debug_fields = vec![debug_thread_field("sender_thread_id", sender_thread_id)];
    if let Some(new_thread_id) = new_thread_id {
        debug_fields.push(debug_thread_field("new_thread_id", new_thread_id));
    }
    maybe_push_debug_line(&mut lines, show_collab_debug_ids, &call_id, debug_fields);

    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn interaction_begin(
    ev: CollabAgentInteractionBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabAgentInteractionBeginEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
        prompt,
        ..
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = Vec::new();
    lines.push(
        vec![
            agent_handle_span(&sender_thread_id, sender_type),
            Span::from(" → ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
            Span::from(" (running)").dim(),
        ]
        .into(),
    );

    if !prompt.trim().is_empty() && !is_private_scout(receiver_type) {
        let preview = single_line_preview(&prompt, COLLAB_PROMPT_PREVIEW_GRAPHEMES);
        lines.push(highlight_mentions(&preview));
    }

    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );

    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn interaction_end(
    ev: CollabAgentInteractionEndEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabAgentInteractionEndEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
        prompt,
        status,
        ..
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = Vec::new();
    lines.push(
        vec![
            agent_handle_span(&sender_thread_id, sender_type),
            Span::from(" → ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
            Span::from(" (").dim(),
            status_span(&status),
            Span::from(")").dim(),
        ]
        .into(),
    );

    if !prompt.trim().is_empty() && !is_private_scout(receiver_type) {
        let preview = single_line_preview(&prompt, COLLAB_PROMPT_PREVIEW_GRAPHEMES);
        lines.push(highlight_mentions(&preview));
    }

    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );

    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn waiting_begin(
    ev: CollabWaitingBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabWaitingBeginEvent {
        call_id,
        sender_thread_id,
        receiver_thread_ids,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let mut lines = vec![vec![agent_handle_span(&sender_thread_id, sender_type)].into()];
    let mut message_spans = vec![Span::from("Waiting for ").dim()];
    message_spans.extend(agent_handle_list_spans(&receiver_thread_ids, agent_types));
    lines.push(message_spans.into());
    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_list_field("receiver_thread_ids", &receiver_thread_ids),
        ],
    );
    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn waiting_end(
    ev: CollabWaitingEndEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> Vec<PlainHistoryCell> {
    let CollabWaitingEndEvent {
        call_id,
        sender_thread_id,
        statuses,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let mut cells = Vec::new();

    let summary = wait_summary_text(&statuses);
    let mut status_thread_ids: Vec<ThreadId> = statuses.keys().copied().collect();
    status_thread_ids.sort_by_key(ToString::to_string);
    let mut summary_lines = vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        highlight_mentions(&format!("Wait complete: {summary}")),
    ];
    maybe_push_debug_line(
        &mut summary_lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_list_field("status_thread_ids", &status_thread_ids),
        ],
    );
    cells.push(PlainHistoryCell::new(summary_lines).with_feed(TranscriptFeed::AgentMesh));

    let mut entries: Vec<(ThreadId, &AgentStatus)> = statuses
        .iter()
        .map(|(thread_id, status)| (*thread_id, status))
        .collect();
    entries.sort_by(|(left, _), (right, _)| left.to_string().cmp(&right.to_string()));

    for (thread_id, status) in entries {
        let agent_type = agent_types.get(&thread_id).map(String::as_str);
        match status {
            AgentStatus::Completed(Some(message)) => {
                let summary_line = if is_private_scout(agent_type) {
                    Line::from("response hidden (private scout)").dim()
                } else {
                    let message_preview =
                        single_line_preview(message, COLLAB_AGENT_RESPONSE_PREVIEW_GRAPHEMES);
                    highlight_mentions(&message_preview)
                };
                cells.push(agent_message_cell(
                    &thread_id,
                    agent_type,
                    status,
                    summary_line,
                ));
            }
            AgentStatus::Errored(error) => {
                let error_preview =
                    single_line_preview(error, COLLAB_AGENT_ERROR_PREVIEW_GRAPHEMES);
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

pub(crate) fn close_begin(
    ev: CollabCloseBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabCloseBeginEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        vec![
            Span::from("Closing ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
        ]
        .into(),
    ];
    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );
    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn close_end(
    ev: CollabCloseEndEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabCloseEndEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
        status,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        vec![
            Span::from("Closed ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
            Span::from(" (").dim(),
            status_span(&status),
            Span::from(")").dim(),
        ]
        .into(),
    ];
    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );
    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn resume_begin(
    ev: CollabResumeBeginEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabResumeBeginEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        vec![
            Span::from("Resuming ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
        ]
        .into(),
    ];
    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );
    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
}

pub(crate) fn resume_end(
    ev: CollabResumeEndEvent,
    agent_types: &HashMap<ThreadId, String>,
    main_thread_id: Option<&ThreadId>,
    show_collab_debug_ids: bool,
) -> PlainHistoryCell {
    let CollabResumeEndEvent {
        call_id,
        sender_thread_id,
        receiver_thread_id,
        status,
    } = ev;

    let sender_type = sender_agent_type(&sender_thread_id, agent_types, main_thread_id);
    let receiver_type = agent_types.get(&receiver_thread_id).map(String::as_str);
    let mut lines = vec![
        vec![agent_handle_span(&sender_thread_id, sender_type)].into(),
        vec![
            Span::from("Resumed ").dim(),
            agent_handle_span(&receiver_thread_id, receiver_type),
            Span::from(" (").dim(),
            status_span(&status),
            Span::from(")").dim(),
        ]
        .into(),
    ];
    maybe_push_debug_line(
        &mut lines,
        show_collab_debug_ids,
        &call_id,
        vec![
            debug_thread_field("sender_thread_id", sender_thread_id),
            debug_thread_field("receiver_thread_id", receiver_thread_id),
        ],
    );
    PlainHistoryCell::new(lines).with_feed(TranscriptFeed::AgentMesh)
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
    .with_feed(TranscriptFeed::AgentMesh)
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
            Some(&sender_thread_id),
            false,
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

    #[test]
    fn spawn_end_renders_chat_like_message() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let scout_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(scout_thread_id, "scout".to_string());

        let cell = spawn_end(
            CollabAgentSpawnEndEvent {
                call_id: "call-1".to_string(),
                sender_thread_id,
                new_thread_id: Some(scout_thread_id),
                agent_type: Some("scout".to_string()),
                prompt: "Collect context for @scout.".to_string(),
                status: AgentStatus::Running,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        let rendered = cell
            .display_lines(80)
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.clone())
            .collect::<String>();
        assert!(rendered.contains("@main"));
        assert!(rendered.contains("Spawned"));

        assert_snapshot!(
            cell.display_lines(80)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn spawn_end_renders_debug_ids_when_enabled() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let scout_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(scout_thread_id, "scout".to_string());

        let cell = spawn_end(
            CollabAgentSpawnEndEvent {
                call_id: "call-debug".to_string(),
                sender_thread_id,
                new_thread_id: Some(scout_thread_id),
                agent_type: Some("scout".to_string()),
                prompt: String::new(),
                status: AgentStatus::Running,
            },
            &agent_types,
            Some(&sender_thread_id),
            true,
        );

        assert_snapshot!(
            cell.display_lines(120)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn spawn_end_hides_debug_ids_when_disabled() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let scout_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(scout_thread_id, "scout".to_string());

        let cell = spawn_end(
            CollabAgentSpawnEndEvent {
                call_id: "call-debug".to_string(),
                sender_thread_id,
                new_thread_id: Some(scout_thread_id),
                agent_type: Some("scout".to_string()),
                prompt: String::new(),
                status: AgentStatus::Running,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        let rendered = cell
            .display_lines(120)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!rendered.contains("[debug]"), "debug IDs must stay hidden");
    }

    #[test]
    fn interaction_end_renders_sender_to_receiver_chat() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let receiver_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();
        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(receiver_thread_id, "devops-engineer".to_string());

        let cell = interaction_end(
            CollabAgentInteractionEndEvent {
                call_id: "call-1".to_string(),
                sender_thread_id,
                receiver_thread_id,
                prompt: "Please deploy and report back to @main.".to_string(),
                message: Default::default(),
                status: AgentStatus::Running,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        assert_snapshot!(
            cell.display_lines(80)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn waiting_begin_renders_chat_like_message() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let scout_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();
        let validator_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000003").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(scout_thread_id, "scout".to_string());
        agent_types.insert(validator_thread_id, "validator".to_string());

        let cell = waiting_begin(
            CollabWaitingBeginEvent {
                sender_thread_id,
                receiver_thread_ids: vec![scout_thread_id, validator_thread_id],
                call_id: "call-1".to_string(),
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        assert_snapshot!(
            cell.display_lines(80)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn close_end_renders_chat_like_message() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let receiver_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();
        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(receiver_thread_id, "scout".to_string());

        let cell = close_end(
            CollabCloseEndEvent {
                call_id: "call-1".to_string(),
                sender_thread_id,
                receiver_thread_id,
                status: AgentStatus::Shutdown,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        assert_snapshot!(
            cell.display_lines(80)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn resume_begin_and_end_render_chat_like_message() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let receiver_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();
        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());
        agent_types.insert(receiver_thread_id, "scout".to_string());

        let begin_cell = resume_begin(
            CollabResumeBeginEvent {
                call_id: "call-1".to_string(),
                sender_thread_id,
                receiver_thread_id,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );
        let end_cell = resume_end(
            CollabResumeEndEvent {
                call_id: "call-1".to_string(),
                sender_thread_id,
                receiver_thread_id,
                status: AgentStatus::Running,
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        let rendered = [begin_cell, end_cell]
            .into_iter()
            .map(|cell| {
                cell.display_lines(80)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.clone())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        assert_snapshot!(rendered);
    }

    #[test]
    fn waiting_begin_renders_unknown_agents_with_thread_suffix() {
        let sender_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap();
        let unknown_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").unwrap();

        let mut agent_types = HashMap::new();
        agent_types.insert(sender_thread_id, "main".to_string());

        let cell = waiting_begin(
            CollabWaitingBeginEvent {
                sender_thread_id,
                receiver_thread_ids: vec![unknown_thread_id],
                call_id: "call-1".to_string(),
            },
            &agent_types,
            Some(&sender_thread_id),
            false,
        );

        assert_snapshot!(
            cell.display_lines(80)
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn agent_handle_uses_custom_name_and_color_token() {
        let thread_id = ThreadId::new();
        let span = agent_handle_span(&thread_id, Some("devops-engineer|red"));
        assert_eq!(span.content.as_ref(), "@devops-engineer");
        assert_eq!(span.style.fg, Some(Color::Red));
    }

    #[test]
    fn agent_handle_renders_display_name_when_present() {
        let thread_id = ThreadId::new();
        let span = agent_handle_span(
            &thread_id,
            Some("devops-engineer|name=DevOps%20Engineer|red"),
        );
        assert_eq!(span.content.as_ref(), "@devops-engineer (DevOps Engineer)");
        assert_eq!(span.style.fg, Some(Color::Red));
    }

    #[test]
    fn private_scout_detection_respects_role_token() {
        assert!(is_private_scout(Some("context-digger|yellow|role=scout")));
        assert!(!is_private_scout(Some(
            "context-digger|yellow|role=validator"
        )));
    }

    #[test]
    fn mention_highlighting_ignores_email_and_keeps_agent_handles() {
        let rendered = highlight_mentions_in_lines(vec![Line::from(
            "Ping @scout and @review-agent. Email test@example.com should stay plain.",
        )])
        .pop()
        .expect("line")
        .spans;

        let scout = rendered
            .iter()
            .find(|span| span.content == "@scout")
            .expect("expected @scout mention");
        assert_eq!(scout.style.fg, Some(Color::Cyan));
        assert!(
            scout.style.add_modifier.contains(Modifier::UNDERLINED),
            "expected @scout to be underlined",
        );

        let reviewer = rendered
            .iter()
            .find(|span| span.content == "@review-agent")
            .expect("expected @review-agent mention");
        assert_eq!(reviewer.style.fg, Some(Color::Cyan));
        assert!(
            reviewer.style.add_modifier.contains(Modifier::UNDERLINED),
            "expected @review-agent to be underlined",
        );

        let email = rendered
            .iter()
            .find(|span| span.content.contains("test@example.com"))
            .expect("expected email span");
        assert_ne!(email.style.fg, Some(Color::Cyan));
    }
}
