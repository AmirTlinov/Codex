use crate::event_mapping::parse_turn_item;
use crate::user_instructions::SkillInstructions;
use crate::user_instructions::UserInstructions;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;

const DEFAULT_MAX_USER_MESSAGES: usize = 6;

pub(crate) fn trim_history_for_workbench(
    items: Vec<ResponseItem>,
    max_user_messages: usize,
) -> Vec<ResponseItem> {
    if items.is_empty() {
        return items;
    }

    let max_user_messages = match max_user_messages {
        0 => DEFAULT_MAX_USER_MESSAGES,
        other => other,
    }
    .max(1);

    let start_index = find_tail_start_index(&items, max_user_messages).unwrap_or(0);
    let mut tail = items[start_index..].to_vec();

    let mut prefix = Vec::new();
    if let Some(item) = last_developer_instructions(&items) {
        prefix.push(item);
    }
    if let Some(item) = last_user_instructions(&items) {
        prefix.push(item);
    }
    prefix.extend(skill_instruction_items(&items));
    if let Some(item) = last_environment_context(&items) {
        prefix.push(item);
    }

    tail.retain(|item| !prefix.contains(item));
    prefix.extend(tail);
    prefix
}

fn find_tail_start_index(items: &[ResponseItem], max_user_messages: usize) -> Option<usize> {
    let mut user_indices = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if matches!(parse_turn_item(item), Some(TurnItem::UserMessage(_))) {
            user_indices.push(idx);
        }
    }

    if user_indices.is_empty() {
        return Some(0);
    }

    if user_indices.len() <= max_user_messages {
        Some(0)
    } else {
        Some(user_indices[user_indices.len() - max_user_messages])
    }
}

fn last_developer_instructions(items: &[ResponseItem]) -> Option<ResponseItem> {
    items.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, .. } if role == "developer" => Some(item.clone()),
        _ => None,
    })
}

fn last_user_instructions(items: &[ResponseItem]) -> Option<ResponseItem> {
    items.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. }
            if role == "user" && UserInstructions::is_user_instructions(content) =>
        {
            Some(item.clone())
        }
        _ => None,
    })
}

fn skill_instruction_items(items: &[ResponseItem]) -> Vec<ResponseItem> {
    let mut result = Vec::new();
    for item in items {
        if let ResponseItem::Message { role, content, .. } = item
            && role == "user"
            && SkillInstructions::is_skill_instructions(content)
            && !result.contains(item)
        {
            result.push(item.clone());
        }
    }
    result
}

fn last_environment_context(items: &[ResponseItem]) -> Option<ResponseItem> {
    items.iter().rev().find_map(|item| match item {
        ResponseItem::Message { content, .. } if is_environment_context_item(content) => {
            Some(item.clone())
        }
        _ => None,
    })
}

fn is_environment_context_item(content: &[ContentItem]) -> bool {
    content.iter().any(|item| match item {
        ContentItem::InputText { text } => text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::trim_history_for_workbench;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
    use pretty_assertions::assert_eq;

    fn msg(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn keeps_latest_pinned_prefix_and_last_user_turn() {
        let items = vec![
            msg("developer", "dev rules"),
            msg(
                "user",
                "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nX\n</INSTRUCTIONS>",
            ),
            msg("user", ENVIRONMENT_CONTEXT_OPEN_TAG),
            msg("user", "first question"),
            msg("assistant", "first answer"),
            msg("user", "second question"),
            msg("assistant", "second answer"),
        ];

        let trimmed = trim_history_for_workbench(items, 1);

        let expected = vec![
            msg("developer", "dev rules"),
            msg(
                "user",
                "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nX\n</INSTRUCTIONS>",
            ),
            msg("user", ENVIRONMENT_CONTEXT_OPEN_TAG),
            msg("user", "second question"),
            msg("assistant", "second answer"),
        ];

        assert_eq!(trimmed, expected);
    }
}
