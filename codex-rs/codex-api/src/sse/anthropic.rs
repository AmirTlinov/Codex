use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::endpoint::anthropic_messages::AnthropicToolKind;
use crate::error::ApiError;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::SearchToolCallParams;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::timeout;
use tracing::debug;

pub fn spawn_anthropic_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    tool_kinds: HashMap<String, AnthropicToolKind>,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_stream(
        stream_response.bytes,
        tx_event,
        idle_timeout,
        tool_kinds,
        cancellation_token,
    ));
    ResponseStream { rx_event }
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    message: Option<AnthropicMessageStart>,
    #[serde(default)]
    content_block: Option<AnthropicContentBlock>,
    #[serde(default)]
    delta: Option<Value>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    error: Option<AnthropicErrorBody>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageStart {
    id: String,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::enum_variant_names)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { _signature: String },
    CitationsDelta {},
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicErrorBody {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Clone)]
enum BlockState {
    Text {
        item_id: String,
        text: String,
        started: bool,
    },
    ToolUse {
        call_id: String,
        name: String,
        input_json: String,
        initial_input: Option<Value>,
    },
    Thinking {
        item_id: String,
        thinking: String,
    },
}

struct AnthropicParseState {
    response_id: Option<String>,
    usage: AnthropicUsage,
    blocks: HashMap<usize, BlockState>,
    created_sent: bool,
}

impl AnthropicParseState {
    fn new() -> Self {
        Self {
            response_id: None,
            usage: AnthropicUsage::default(),
            blocks: HashMap::new(),
            created_sent: false,
        }
    }

    fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.usage.input_tokens,
            cached_input_tokens: self.usage.cache_read_input_tokens
                + self.usage.cache_creation_input_tokens,
            output_tokens: self.usage.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens: self.usage.input_tokens + self.usage.output_tokens,
        }
    }
}

async fn process_stream(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    tool_kinds: HashMap<String, AnthropicToolKind>,
    cancellation_token: tokio_util::sync::CancellationToken,
) {
    let mut stream = stream
        .map(|res| res.map_err(|e| e.to_string()))
        .eventsource();

    let mut state = AnthropicParseState::new();

    loop {
        let next_event = tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                let _ = tx_event.send(Err(ApiError::Stream("Anthropic stream cancelled".to_string()))).await;
                return;
            }
            next = timeout(idle_timeout, stream.next()) => next,
        };
        match next_event {
            Ok(Some(Ok(event))) => {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    let response_id = state
                        .response_id
                        .clone()
                        .unwrap_or_else(|| "anthropic-message".to_string());
                    let _ = tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage: Some(state.token_usage()),
                        }))
                        .await;
                    return;
                }
                match serde_json::from_str::<AnthropicStreamEvent>(data) {
                    Ok(parsed) => {
                        if let Err(err) =
                            handle_event(parsed, &mut state, &tx_event, &tool_kinds).await
                        {
                            let _ = tx_event.send(Err(err)).await;
                            return;
                        }
                    }
                    Err(err) => {
                        debug!("failed to parse Anthropic stream event: {err}");
                        let _ = tx_event
                            .send(Err(ApiError::Stream(format!(
                                "failed to parse Anthropic stream event: {err}"
                            ))))
                            .await;
                        return;
                    }
                }
            }
            Ok(Some(Err(err))) => {
                let _ = tx_event.send(Err(ApiError::Stream(err.to_string()))).await;
                return;
            }
            Ok(None) => {
                let response_id = state
                    .response_id
                    .clone()
                    .unwrap_or_else(|| "anthropic-message".to_string());
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id,
                        token_usage: Some(state.token_usage()),
                    }))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "Anthropic stream timed out".to_string(),
                    )))
                    .await;
                return;
            }
        }
    }
}

async fn handle_event(
    event: AnthropicStreamEvent,
    state: &mut AnthropicParseState,
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    tool_kinds: &HashMap<String, AnthropicToolKind>,
) -> Result<(), ApiError> {
    match event.kind.as_str() {
        "message_start" => {
            if !state.created_sent {
                state.created_sent = true;
                tx_event.send(Ok(ResponseEvent::Created)).await.ok();
            }
            if let Some(message) = event.message {
                state.response_id = Some(message.id);
                if let Some(usage) = message.usage {
                    merge_usage(&mut state.usage, usage);
                }
            }
        }
        "content_block_start" => {
            let index = event.index.unwrap_or(0);
            let Some(content_block) = event.content_block else {
                return Ok(());
            };
            let block_state = match content_block {
                AnthropicContentBlock::Text { text } => BlockState::Text {
                    item_id: format!(
                        "{}-text-{index}",
                        state
                            .response_id
                            .clone()
                            .unwrap_or_else(|| "anthropic".to_string())
                    ),
                    text,
                    started: false,
                },
                AnthropicContentBlock::ToolUse { id, name, input } => BlockState::ToolUse {
                    call_id: id,
                    name,
                    input_json: String::new(),
                    initial_input: Some(input),
                },
                AnthropicContentBlock::Thinking { thinking } => BlockState::Thinking {
                    item_id: format!(
                        "{}-thinking-{index}",
                        state
                            .response_id
                            .clone()
                            .unwrap_or_else(|| "anthropic".to_string())
                    ),
                    thinking,
                },
            };
            state.blocks.insert(index, block_state);
        }
        "content_block_delta" => {
            let index = event.index.unwrap_or(0);
            let Some(delta) = event.delta else {
                return Ok(());
            };
            let Some(block) = state.blocks.get_mut(&index) else {
                return Ok(());
            };
            let delta: AnthropicDelta = serde_json::from_value(delta).map_err(|err| {
                ApiError::Stream(format!("invalid Anthropic content delta: {err}"))
            })?;
            match (block, delta) {
                (
                    BlockState::Text {
                        item_id,
                        text,
                        started,
                    },
                    AnthropicDelta::TextDelta { text: delta },
                ) => {
                    if !*started {
                        *started = true;
                        tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                                id: Some(item_id.clone()),
                                role: "assistant".to_string(),
                                content: Vec::new(),
                                end_turn: None,
                                phase: None,
                            })))
                            .await
                            .ok();
                    }
                    text.push_str(&delta);
                    tx_event
                        .send(Ok(ResponseEvent::OutputTextDelta(delta)))
                        .await
                        .ok();
                }
                (
                    BlockState::ToolUse { input_json, .. },
                    AnthropicDelta::InputJsonDelta { partial_json },
                ) => {
                    input_json.push_str(&partial_json);
                }
                (
                    BlockState::Thinking { thinking, .. },
                    AnthropicDelta::ThinkingDelta { thinking: delta },
                ) => {
                    thinking.push_str(&delta);
                }
                (BlockState::Thinking { .. }, AnthropicDelta::SignatureDelta { .. })
                | (_, AnthropicDelta::CitationsDelta {}) => {}
                _ => {}
            }
        }
        "content_block_stop" => {
            let index = event.index.unwrap_or(0);
            let Some(block) = state.blocks.remove(&index) else {
                return Ok(());
            };
            match block {
                BlockState::Text { item_id, text, .. } => {
                    tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                            id: Some(item_id),
                            role: "assistant".to_string(),
                            content: vec![ContentItem::OutputText { text }],
                            end_turn: None,
                            phase: None,
                        })))
                        .await
                        .ok();
                }
                BlockState::ToolUse {
                    call_id,
                    name,
                    input_json,
                    initial_input,
                } => {
                    let parsed_value = if !input_json.trim().is_empty() {
                        serde_json::from_str::<Value>(&input_json)
                            .unwrap_or(Value::String(input_json.clone()))
                    } else {
                        initial_input.unwrap_or(Value::Null)
                    };
                    let tool_kind = tool_kinds
                        .get(&name)
                        .copied()
                        .unwrap_or(AnthropicToolKind::Function);
                    let response_item = match tool_kind {
                        AnthropicToolKind::Function => {
                            let arguments = match &parsed_value {
                                Value::Object(_) | Value::Array(_) => {
                                    serde_json::to_string(&parsed_value)
                                        .unwrap_or_else(|_| "{}".to_string())
                                }
                                Value::String(value) => value.clone(),
                                other => serde_json::to_string(other)
                                    .unwrap_or_else(|_| "null".to_string()),
                            };
                            ResponseItem::FunctionCall {
                                id: Some(call_id.clone()),
                                name,
                                namespace: None,
                                arguments,
                                call_id,
                            }
                        }
                        AnthropicToolKind::Custom => ResponseItem::CustomToolCall {
                            id: Some(call_id.clone()),
                            status: None,
                            call_id,
                            name,
                            input: anthropic_custom_input_text(&parsed_value),
                        },
                        AnthropicToolKind::ToolSearch => ResponseItem::ToolSearchCall {
                            id: Some(call_id.clone()),
                            call_id: Some(call_id),
                            status: None,
                            execution: "client".to_string(),
                            arguments: anthropic_tool_search_arguments(parsed_value),
                        },
                    };
                    tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(response_item)))
                        .await
                        .ok();
                }
                BlockState::Thinking { item_id, thinking } => {
                    tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                            id: item_id,
                            summary: Vec::<ReasoningItemReasoningSummary>::new(),
                            content: Some(vec![ReasoningItemContent::ReasoningText {
                                text: thinking,
                            }]),
                            encrypted_content: None,
                        })))
                        .await
                        .ok();
                }
            }
        }
        "message_delta" => {
            let stop_reason = event
                .delta
                .as_ref()
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str);
            if let Some(usage) = event.usage {
                merge_usage(&mut state.usage, usage);
            }
            if matches!(stop_reason, Some("end_turn" | "tool_use" | "max_tokens")) {
                // stop_reason is carried on the Completed event via response_id/token usage only.
            }
        }
        "message_stop" => {}
        "error" => {
            let message = event
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "Anthropic stream returned an error".to_string());
            return Err(ApiError::Stream(message));
        }
        _ => {}
    }
    Ok(())
}

fn anthropic_custom_input_text(value: &Value) -> String {
    match value {
        Value::Object(map) => map
            .get("input")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn anthropic_tool_search_arguments(value: Value) -> Value {
    let value_clone = value.clone();
    serde_json::from_value::<SearchToolCallParams>(value_clone)
        .map(|_| value.clone())
        .unwrap_or_else(|_| serde_json::json!({ "query": value.to_string() }))
}

fn merge_usage(total: &mut AnthropicUsage, next: AnthropicUsage) {
    if next.input_tokens > 0 {
        total.input_tokens = next.input_tokens;
    }
    if next.output_tokens > 0 {
        total.output_tokens = next.output_tokens;
    }
    if next.cache_creation_input_tokens > 0 {
        total.cache_creation_input_tokens = next.cache_creation_input_tokens;
    }
    if next.cache_read_input_tokens > 0 {
        total.cache_read_input_tokens = next.cache_read_input_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream;
    use http::HeaderMap;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn sse_bytes(payload: &str) -> ByteStream {
        Box::pin(stream::iter(vec![
            Ok::<Bytes, codex_client::TransportError>(Bytes::from(payload.to_string())),
        ]))
    }

    #[tokio::test]
    async fn anthropic_stream_emits_text_and_tool_use_items() {
        let payload = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"usage\":{\"input_tokens\":12}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"apply_patch\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"*** Begin Patch\\\\n*** End Patch\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let stream_response = StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: sse_bytes(payload),
        };
        let mut stream = spawn_anthropic_stream(
            stream_response,
            Duration::from_secs(5),
            HashMap::from([("apply_patch".to_string(), AnthropicToolKind::Custom)]),
            tokio_util::sync::CancellationToken::new(),
        );
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event should parse"));
        }

        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(
            events[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
        ));
        assert!(matches!(events[2], ResponseEvent::OutputTextDelta(ref delta) if delta == "Hello"));
        assert!(matches!(
            events[3],
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        ));
        assert!(matches!(
            events[4],
            ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall { ref name, ref input, .. })
                if name == "apply_patch" && input.contains("*** Begin Patch")
        ));
        assert!(matches!(
            events.last(),
            Some(ResponseEvent::Completed { response_id, token_usage: Some(token_usage) })
                if response_id == "msg-1" && token_usage.input_tokens == 12 && token_usage.output_tokens == 7
        ));
    }

    #[test]
    fn anthropic_tool_search_arguments_falls_back_to_query_text() {
        let value = json!({"unexpected": true});
        assert_eq!(
            anthropic_tool_search_arguments(value.clone()),
            json!({"query": value.to_string()})
        );
    }

    #[tokio::test]
    async fn anthropic_stream_reports_cancellation() {
        let stream_response = StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: sse_bytes(""),
        };
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        cancellation_token.cancel();
        let mut stream = spawn_anthropic_stream(
            stream_response,
            Duration::from_secs(5),
            HashMap::new(),
            cancellation_token,
        );
        let event = stream.next().await.expect("expected cancellation event");
        assert!(
            matches!(event, Err(ApiError::Stream(message)) if message == "Anthropic stream cancelled")
        );
    }
}
