use crate::context_manager::is_user_turn_boundary;
use codex_protocol::models::ResponseItem;

pub(crate) fn render_compact_transcript(
    items: &[ResponseItem],
    last_n_user_turns: Option<usize>,
) -> String {
    let scoped_items = scope_to_last_n_user_turns(items, last_n_user_turns);
    let scoped_items_omitted = scoped_items.len() < items.len();
    let entries = crate::guardian::collect_guardian_transcript_entries(scoped_items);
    let (transcript, guardian_omission_note) =
        crate::guardian::render_guardian_transcript_entries(&entries);
    let mut lines = transcript;
    let omission_note = match (scoped_items_omitted, guardian_omission_note) {
        (true, Some(guardian_omission_note)) => Some(format!(
            "Earlier user turns were omitted to keep the transcript bounded. {guardian_omission_note}"
        )),
        (true, None) => {
            Some("Earlier user turns were omitted to keep the transcript bounded.".to_string())
        }
        (false, Some(guardian_omission_note)) => Some(guardian_omission_note),
        (false, None) => None,
    };
    if let Some(omission_note) = omission_note {
        lines.push(format!(
            "<transcript_omission>{omission_note}</transcript_omission>"
        ));
    }
    lines.join("\n")
}

fn scope_to_last_n_user_turns(
    items: &[ResponseItem],
    last_n_user_turns: Option<usize>,
) -> &[ResponseItem] {
    let Some(last_n_user_turns) = last_n_user_turns else {
        return items;
    };
    if last_n_user_turns == 0 {
        return &[];
    }

    let user_turn_positions = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| is_user_turn_boundary(item).then_some(index))
        .collect::<Vec<_>>();
    if user_turn_positions.len() <= last_n_user_turns {
        return items;
    }

    let start_index = user_turn_positions[user_turn_positions.len() - last_n_user_turns];
    &items[start_index..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;

    fn message(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn transcript_can_scope_to_last_n_user_turns() {
        let items = vec![
            message("user", "u1"),
            message("assistant", "a1"),
            message("user", "u2"),
            message("assistant", "a2"),
            message("user", "u3"),
            message("assistant", "a3"),
        ];

        let transcript = render_compact_transcript(&items, Some(2));
        assert!(transcript.contains("u2"));
        assert!(transcript.contains("u3"));
        assert!(!transcript.contains("u1"));
        assert!(transcript.contains("<transcript_omission>"));
    }
}
