use anyhow::Context;
use codex_api::common::ResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct ClaudeCodeStreamAccumulator {
    created_sent: bool,
    response_id: Option<String>,
    token_usage: Option<TokenUsage>,
    session_id: Option<String>,
    active_message_id: Option<String>,
    partial_text: String,
    assistant_text: Option<String>,
}

pub(crate) struct ClaudeCodeTurnSummary {
    pub(crate) response_id: String,
    pub(crate) token_usage: Option<TokenUsage>,
    pub(crate) session_id: Option<String>,
    pub(crate) assistant_text: String,
}

pub(crate) fn completed_response_items(events: &[ResponseEvent]) -> Vec<ResponseItem> {
    events
        .iter()
        .filter_map(|event| match event {
            ResponseEvent::OutputItemDone(item) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

impl ClaudeCodeStreamAccumulator {
    pub(crate) fn push_line(&mut self, line: &str) -> anyhow::Result<Vec<ResponseEvent>> {
        let value: serde_json::Value =
            serde_json::from_str(line).context("parse Claude Code stream-json line")?;
        self.capture_session_id(&value);
        let Some(message_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            return Ok(Vec::new());
        };

        match message_type {
            "system" => self.handle_system(&value),
            "stream_event" => self.handle_stream_event(&value),
            "assistant" => self.handle_assistant(&value),
            "result" => {
                self.handle_result(&value);
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    pub(crate) fn finish(self) -> ClaudeCodeTurnSummary {
        let response_id = self
            .response_id
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
        let assistant_text = self
            .assistant_text
            .filter(|text| !text.is_empty())
            .unwrap_or(self.partial_text);
        ClaudeCodeTurnSummary {
            response_id,
            token_usage: self.token_usage,
            session_id: self.session_id,
            assistant_text,
        }
    }

    fn handle_system(&mut self, value: &serde_json::Value) -> anyhow::Result<Vec<ResponseEvent>> {
        if value.get("subtype").and_then(serde_json::Value::as_str) == Some("init")
            && !self.created_sent
        {
            self.created_sent = true;
            return Ok(vec![ResponseEvent::Created]);
        }
        Ok(Vec::new())
    }

    fn handle_stream_event(
        &mut self,
        value: &serde_json::Value,
    ) -> anyhow::Result<Vec<ResponseEvent>> {
        let mut events = Vec::new();
        if !self.created_sent {
            self.created_sent = true;
            events.push(ResponseEvent::Created);
        }

        let Some(event) = value.get("event").and_then(serde_json::Value::as_object) else {
            return Ok(events);
        };
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("message_start") => {
                self.response_id = event
                    .get("message")
                    .and_then(|message| message.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
            Some("content_block_start") => {
                let block_type = event
                    .get("content_block")
                    .and_then(|block| block.get("type"))
                    .and_then(serde_json::Value::as_str);
                if matches!(block_type, Some("text" | "thinking" | "redacted_thinking")) {
                    self.ensure_active_message_started(&mut events);
                }
            }
            Some("content_block_delta") => {
                let Some(delta) = event.get("delta").and_then(serde_json::Value::as_object) else {
                    return Ok(events);
                };
                match delta.get("type").and_then(serde_json::Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(serde_json::Value::as_str) {
                            self.ensure_active_message_started(&mut events);
                            self.partial_text.push_str(text);
                            events.push(ResponseEvent::OutputTextDelta(text.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) =
                            delta.get("thinking").and_then(serde_json::Value::as_str)
                        {
                            self.ensure_active_message_started(&mut events);
                            events.push(ResponseEvent::ReasoningContentDelta {
                                delta: text.to_string(),
                                content_index: 0,
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(events)
    }

    fn handle_assistant(
        &mut self,
        value: &serde_json::Value,
    ) -> anyhow::Result<Vec<ResponseEvent>> {
        let mut events = Vec::new();
        if !self.created_sent {
            self.created_sent = true;
            events.push(ResponseEvent::Created);
        }
        let Some(message) = value.get("message").and_then(serde_json::Value::as_object) else {
            return Ok(events);
        };
        let message_id = message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| self.active_message_id.clone())
            .or_else(|| self.response_id.clone())
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
        self.response_id = Some(message_id.clone());
        let parsed = parse_assistant_content_blocks(message.get("content"), &message_id)
            .with_context(|| format!("parse Claude assistant payload for {message_id}"))?;
        if parsed.events.is_empty() {
            let text = if self.partial_text.is_empty() {
                String::new()
            } else {
                self.partial_text.clone()
            };
            self.assistant_text = Some(text.clone());
            events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: Some(message_id),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text }],
                end_turn: Some(true),
                phase: None,
            }));
        } else {
            self.assistant_text = Some(parsed.visible_text);
            events.extend(parsed.events);
        }
        self.active_message_id = None;
        Ok(events)
    }

    fn handle_result(&mut self, value: &serde_json::Value) {
        if let Some(session_id) = value.get("session_id").and_then(serde_json::Value::as_str) {
            self.session_id = Some(session_id.to_string());
            self.response_id
                .get_or_insert_with(|| session_id.to_string());
        }
        if self.assistant_text.is_none()
            && let Some(text) = value.get("result").and_then(serde_json::Value::as_str)
            && !text.is_empty()
        {
            self.assistant_text = Some(text.to_string());
        }
        self.token_usage = parse_token_usage(value.get("usage"));
    }

    fn capture_session_id(&mut self, value: &serde_json::Value) {
        if let Some(session_id) = value.get("session_id").and_then(serde_json::Value::as_str) {
            self.session_id = Some(session_id.to_string());
        }
    }

    fn ensure_active_message_started(&mut self, events: &mut Vec<ResponseEvent>) {
        if self.active_message_id.is_some() {
            return;
        }

        self.partial_text.clear();
        let item_id = self
            .response_id
            .clone()
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
        self.active_message_id = Some(item_id.clone());
        events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
            id: Some(item_id),
            role: "assistant".to_string(),
            content: Vec::new(),
            end_turn: None,
            phase: None,
        }));
    }
}

struct ParsedAssistantContent {
    visible_text: String,
    events: Vec<ResponseEvent>,
}

fn parse_assistant_content_blocks(
    content: Option<&Value>,
    message_id: &str,
) -> anyhow::Result<ParsedAssistantContent> {
    let Some(content) = content.and_then(Value::as_array) else {
        return Ok(ParsedAssistantContent {
            visible_text: String::new(),
            events: Vec::new(),
        });
    };

    let mut visible_text = String::new();
    let mut events = Vec::new();
    let mut pending_text = String::new();
    let mut emitted_text_segments = 0usize;
    let mut emitted_tool_calls = 0usize;

    let flush_pending_text = |events: &mut Vec<ResponseEvent>,
                              pending_text: &mut String,
                              emitted_text_segments: &mut usize,
                              message_id: &str| {
        if pending_text.is_empty() {
            return;
        }
        *emitted_text_segments += 1;
        let item_id = if *emitted_text_segments == 1 {
            message_id.to_string()
        } else {
            format!("{message_id}-text-{emitted_text_segments}")
        };
        let text = std::mem::take(pending_text);
        events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: Some(item_id),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText { text }],
            end_turn: Some(true),
            phase: None,
        }));
    };

    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    visible_text.push_str(text);
                    pending_text.push_str(text);
                }
            }
            Some("tool_use") => {
                flush_pending_text(
                    &mut events,
                    &mut pending_text,
                    &mut emitted_text_segments,
                    message_id,
                );

                emitted_tool_calls += 1;
                let tool_use_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("tool_use block is missing a non-empty `id`"))?
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("tool_use block is missing a non-empty `name`"))?
                    .to_string();
                let item_id = Some(format!("{message_id}-tool-item-{emitted_tool_calls}"));
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));

                let item = if let Some(input) = input.as_str() {
                    ResponseItem::CustomToolCall {
                        id: item_id,
                        status: None,
                        call_id: tool_use_id,
                        name,
                        input: input.to_string(),
                    }
                } else {
                    ResponseItem::FunctionCall {
                        id: item_id,
                        name,
                        namespace: None,
                        arguments: serde_json::to_string(&input)
                            .context("serialize Claude tool_use input")?,
                        call_id: tool_use_id,
                    }
                };
                events.push(ResponseEvent::OutputItemDone(item));
            }
            _ => {}
        }
    }

    flush_pending_text(
        &mut events,
        &mut pending_text,
        &mut emitted_text_segments,
        message_id,
    );

    Ok(ParsedAssistantContent {
        visible_text,
        events,
    })
}

pub(crate) fn parse_token_usage(usage: Option<&serde_json::Value>) -> Option<TokenUsage> {
    let usage = usage?.as_object()?;
    let input_tokens = usage
        .get("input_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let output_tokens = usage
        .get("output_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let cached_input_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default()
        + usage
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default();
    Some(TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens: 0,
        total_tokens: input_tokens + output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::ClaudeCodeStreamAccumulator;
    use codex_api::common::ResponseEvent;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    fn expect_single_event(events: Vec<ResponseEvent>) -> ResponseEvent {
        assert_eq!(events.len(), 1);
        events.into_iter().next().expect("single event")
    }

    #[test]
    fn thinking_blocks_start_the_assistant_item_before_reasoning_deltas() {
        let mut accumulator = ClaudeCodeStreamAccumulator::default();

        let created = accumulator
            .push_line(
                r#"{"type":"system","subtype":"init","session_id":"mock-session","uuid":"init-1"}"#,
            )
            .expect("system init should parse");
        assert!(matches!(
            expect_single_event(created),
            ResponseEvent::Created
        ));

        let message_start = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","content":[]}},"session_id":"mock-session","uuid":"event-1"}"#,
            )
            .expect("message_start should parse");
        assert!(message_start.is_empty());

        let thinking_start = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}},"session_id":"mock-session","uuid":"event-2"}"#,
            )
            .expect("thinking block start should parse");
        assert!(matches!(
            expect_single_event(thinking_start),
            ResponseEvent::OutputItemAdded(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: None,
                phase: None,
            }) if id == "msg-1" && role == "assistant" && content.is_empty()
        ));

        let thinking_delta = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"first thought"}},"session_id":"mock-session","uuid":"event-3"}"#,
            )
            .expect("thinking delta should parse");
        assert!(matches!(
            expect_single_event(thinking_delta),
            ResponseEvent::ReasoningContentDelta { delta, content_index }
                if delta == "first thought" && content_index == 0
        ));

        let text_delta = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello"}},"session_id":"mock-session","uuid":"event-4"}"#,
            )
            .expect("text delta should parse");
        assert!(matches!(
            expect_single_event(text_delta),
            ResponseEvent::OutputTextDelta(delta) if delta == "hello"
        ));

        let assistant = accumulator
            .push_line(
                r#"{"type":"assistant","message":{"id":"msg-1","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
            )
            .expect("assistant payload should parse");
        assert!(matches!(
            expect_single_event(assistant),
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: Some(true),
                phase: None,
            }) if id == "msg-1"
                && role == "assistant"
                && content == vec![ContentItem::OutputText {
                    text: "hello".to_string(),
                }]
        ));

        let summary = accumulator.finish();
        assert_eq!(summary.response_id, "msg-1");
        assert_eq!(summary.assistant_text, "hello");
    }

    #[test]
    fn thinking_deltas_without_a_prior_block_start_still_seed_the_assistant_item() {
        let mut accumulator = ClaudeCodeStreamAccumulator::default();

        let message_start = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg-2","type":"message","role":"assistant","content":[]}},"session_id":"mock-session","uuid":"event-1"}"#,
            )
            .expect("message_start should parse");
        assert!(matches!(
            expect_single_event(message_start),
            ResponseEvent::Created
        ));

        let events = accumulator
            .push_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"orphan thought"}},"session_id":"mock-session","uuid":"event-2"}"#,
            )
            .expect("thinking delta should parse");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            ResponseEvent::OutputItemAdded(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: None,
                phase: None,
            }) if id == "msg-2" && role == "assistant" && content.is_empty()
        ));
        assert!(matches!(
            &events[1],
            ResponseEvent::ReasoningContentDelta { delta, content_index }
                if delta == "orphan thought" && *content_index == 0
        ));
    }

    #[test]
    fn assistant_payload_with_structured_tool_use_emits_tool_items() {
        let mut accumulator = ClaudeCodeStreamAccumulator::default();

        let events = accumulator
            .push_line(
                r#"{"type":"assistant","message":{"id":"msg-structured","type":"message","role":"assistant","content":[{"type":"text","text":"Running shell."},{"type":"tool_use","id":"toolu-1","name":"mcp__codex__codex-shell","input":{"command":"printf hi"}},{"type":"text","text":"Done."}]}}"#,
            )
            .expect("assistant payload should parse");

        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(
            &events[1],
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: Some(true),
                phase: None,
            }) if id == "msg-structured"
                && role == "assistant"
                && content == &vec![ContentItem::OutputText {
                    text: "Running shell.".to_string(),
                }]
        ));
        assert!(matches!(
            &events[2],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                id: Some(id),
                name,
                namespace: None,
                arguments,
                call_id,
            }) if id == "msg-structured-tool-item-1"
                && name == "mcp__codex__codex-shell"
                && call_id == "toolu-1"
                && serde_json::from_str::<serde_json::Value>(arguments)
                    .expect("tool arguments should be valid json")
                    == serde_json::json!({ "command": "printf hi" })
        ));
        assert!(matches!(
            &events[3],
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: Some(true),
                phase: None,
            }) if id == "msg-structured-text-2"
                && role == "assistant"
                && content == &vec![ContentItem::OutputText {
                    text: "Done.".to_string(),
                }]
        ));

        let summary = accumulator.finish();
        assert_eq!(summary.response_id, "msg-structured");
        assert_eq!(summary.assistant_text, "Running shell.Done.");
    }
}
