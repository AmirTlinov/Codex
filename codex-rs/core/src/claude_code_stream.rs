use anyhow::Context;
use codex_api::common::ResponseEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
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
                if event
                    .get("content_block")
                    .and_then(|block| block.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("text")
                    && self.active_message_id.is_none()
                {
                    self.partial_text.clear();
                    let item_id = self
                        .response_id
                        .clone()
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
            Some("content_block_delta") => {
                let Some(delta) = event.get("delta").and_then(serde_json::Value::as_object) else {
                    return Ok(events);
                };
                match delta.get("type").and_then(serde_json::Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(serde_json::Value::as_str) {
                            self.partial_text.push_str(text);
                            events.push(ResponseEvent::OutputTextDelta(text.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) =
                            delta.get("thinking").and_then(serde_json::Value::as_str)
                        {
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
        let text = extract_text_content(message.get("content")).unwrap_or_else(|| {
            if self.partial_text.is_empty() {
                String::new()
            } else {
                self.partial_text.clone()
            }
        });
        self.assistant_text = Some(text.clone());
        events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: Some(message_id),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText { text }],
            end_turn: Some(true),
            phase: None,
        }));
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
}

fn extract_text_content(content: Option<&serde_json::Value>) -> Option<String> {
    let content = content?.as_array()?;
    Some(
        content
            .iter()
            .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join(""),
    )
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
