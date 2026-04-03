use crate::agent::external::ClaudeCliRequest;
use crate::agent::external::run_claude_cli_stream_json;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::compact::content_items_to_text;
use crate::config::ClaudeCliConfig;
use crate::config::ClaudeCliEffort;
use crate::error::CodexErr;
use crate::error::Result;
use crate::event_mapping::is_contextual_user_message_content;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::TokenUsage;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CLAUDE_BRIDGE_PROMPT: &str = concat!(
    "You are the main assistant running inside Codex through Claude Code.\n",
    "Return the assistant response that should appear in the Codex conversation."
);

pub(super) async fn stream_claude_cli_turn(
    claude_cli: &ClaudeCliConfig,
    prompt: &Prompt,
    model_info: &ModelInfo,
    cwd: &std::path::Path,
    effort: Option<ReasoningEffortConfig>,
    cancellation_token: CancellationToken,
) -> Result<ResponseStream> {
    let user_prompt = render_claude_turn_prompt(&prompt.input)?;
    let mut raw_lines = run_claude_cli_stream_json(
        claude_cli,
        ClaudeCliRequest {
            cwd: cwd.to_path_buf(),
            model: model_info.slug.clone(),
            system_prompt: build_system_prompt(&prompt.base_instructions.text),
            user_prompt,
            json_schema: prompt.output_schema.clone(),
            tools: claude_cli.tools.clone(),
            force_toolless: false,
            effort: effort
                .or(model_info.default_reasoning_level)
                .map(ClaudeCliEffort::from),
        },
        cancellation_token,
    )
    .await
    .map_err(|err| CodexErr::Stream(err.to_string(), None))?;

    let (tx_event, rx_event) = mpsc::channel(1600);
    tokio::spawn(async move {
        let mut state = ClaudeCodeStreamState::default();
        while let Some(line) = raw_lines.recv().await {
            match line {
                Ok(line) => {
                    if let Err(err) = handle_stream_line(&line, &mut state, &tx_event).await {
                        let _ = tx_event.send(Err(err)).await;
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx_event
                        .send(Err(CodexErr::Stream(err.to_string(), None)))
                        .await;
                    return;
                }
            }
        }

        let response_id = state
            .response_id
            .clone()
            .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
        let _ = tx_event
            .send(Ok(ResponseEvent::Completed {
                response_id,
                token_usage: state.token_usage,
            }))
            .await;
    });

    Ok(ResponseStream { rx_event })
}

#[derive(Default)]
struct ClaudeCodeStreamState {
    created_sent: bool,
    response_id: Option<String>,
    token_usage: Option<TokenUsage>,
    active_message_id: Option<String>,
}

async fn handle_stream_line(
    line: &str,
    state: &mut ClaudeCodeStreamState,
    tx_event: &mpsc::Sender<Result<ResponseEvent>>,
) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|err| CodexErr::Stream(err.to_string(), None))?;
    let Some(message_type) = value.get("type").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };

    match message_type {
        "system" => {
            if value.get("subtype").and_then(serde_json::Value::as_str) == Some("init")
                && !state.created_sent
            {
                state.created_sent = true;
                tx_event.send(Ok(ResponseEvent::Created)).await.ok();
            }
        }
        "stream_event" => {
            if !state.created_sent {
                state.created_sent = true;
                tx_event.send(Ok(ResponseEvent::Created)).await.ok();
            }
            let Some(event) = value.get("event").and_then(serde_json::Value::as_object) else {
                return Ok(());
            };
            match event.get("type").and_then(serde_json::Value::as_str) {
                Some("message_start") => {
                    state.response_id = event
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
                    {
                        let item_id = state
                            .response_id
                            .clone()
                            .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
                        state.active_message_id = Some(item_id.clone());
                        tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                                id: Some(item_id),
                                role: "assistant".to_string(),
                                content: Vec::new(),
                                end_turn: None,
                                phase: None,
                            })))
                            .await
                            .ok();
                    }
                }
                Some("content_block_delta") => {
                    let Some(delta) = event.get("delta").and_then(serde_json::Value::as_object)
                    else {
                        return Ok(());
                    };
                    match delta.get("type").and_then(serde_json::Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) =
                                delta.get("text").and_then(serde_json::Value::as_str)
                            {
                                tx_event
                                    .send(Ok(ResponseEvent::OutputTextDelta(text.to_string())))
                                    .await
                                    .ok();
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(text) =
                                delta.get("thinking").and_then(serde_json::Value::as_str)
                            {
                                tx_event
                                    .send(Ok(ResponseEvent::ReasoningContentDelta {
                                        delta: text.to_string(),
                                        content_index: 0,
                                    }))
                                    .await
                                    .ok();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        "assistant" => {
            if !state.created_sent {
                state.created_sent = true;
                tx_event.send(Ok(ResponseEvent::Created)).await.ok();
            }
            let Some(message) = value.get("message").and_then(serde_json::Value::as_object) else {
                return Ok(());
            };
            let message_id = message
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| state.active_message_id.clone())
                .or_else(|| state.response_id.clone())
                .unwrap_or_else(|| format!("claude-code-{}", Uuid::new_v4()));
            state.response_id = Some(message_id.clone());
            let text = message
                .get("content")
                .and_then(serde_json::Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter(|&block| {
                            block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                        })
                        .map(|block| {
                            block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            tx_event
                .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                    id: Some(message_id),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text }],
                    end_turn: Some(true),
                    phase: None,
                })))
                .await
                .ok();
            state.active_message_id = None;
        }
        "result" => {
            if let Some(session_id) = value.get("session_id").and_then(serde_json::Value::as_str) {
                state
                    .response_id
                    .get_or_insert_with(|| session_id.to_string());
            }
            state.token_usage = parse_token_usage(value.get("usage"));
        }
        _ => {}
    }

    Ok(())
}

fn parse_token_usage(usage: Option<&serde_json::Value>) -> Option<TokenUsage> {
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

fn build_system_prompt(base_instructions: &str) -> String {
    let trimmed = base_instructions.trim();
    if trimmed.is_empty() {
        return CLAUDE_BRIDGE_PROMPT.to_string();
    }
    format!("{trimmed}\n\n{CLAUDE_BRIDGE_PROMPT}")
}

fn render_claude_turn_prompt(items: &[ResponseItem]) -> Result<String> {
    let mut sections = Vec::new();
    for item in items {
        if let Some(section) = render_item(item)? {
            sections.push(section);
        }
    }
    if sections.is_empty() {
        Ok(
            "<conversation_context>\nNo prior turn items were provided.\n</conversation_context>"
                .to_string(),
        )
    } else {
        Ok(format!(
            "<conversation_context>\n{}\n</conversation_context>",
            sections.join("\n\n")
        ))
    }
}

fn render_item(item: &ResponseItem) -> Result<Option<String>> {
    match item {
        ResponseItem::Message { role, content, .. } => render_message(role, content),
        ResponseItem::FunctionCall {
            name, arguments, ..
        } => Ok(render_text_block(
            &format!("tool_call name=\"{name}\""),
            arguments,
        )),
        ResponseItem::CustomToolCall { name, input, .. } => Ok(render_text_block(
            &format!("tool_call name=\"{name}\""),
            input,
        )),
        ResponseItem::LocalShellCall { action, .. } => Ok(render_text_block(
            "tool_call name=\"shell\"",
            &serde_json::to_string(action)?,
        )),
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => Ok(output.body.to_text().and_then(|text| {
            render_text_block(&format!("tool_result call_id=\"{call_id}\""), &text)
        })),
        ResponseItem::WebSearchCall { action, .. } => Ok(action.as_ref().and_then(|action| {
            render_text_block(
                "tool_call name=\"web_search\"",
                &serde_json::to_string(action).ok()?,
            )
        })),
        ResponseItem::Reasoning {
            summary, content, ..
        } => {
            let mut pieces = summary
                .iter()
                .map(|entry| match entry {
                    codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        text.as_str()
                    }
                })
                .collect::<Vec<_>>();
            if let Some(content) = content.as_ref() {
                pieces.extend(content.iter().map(|entry| match entry {
                    codex_protocol::models::ReasoningItemContent::ReasoningText { text }
                    | codex_protocol::models::ReasoningItemContent::Text { text } => text.as_str(),
                }));
            }
            Ok(render_text_block("assistant_reasoning", &pieces.join("\n")))
        }
        _ => Ok(None),
    }
}

fn render_message(role: &str, content: &[ContentItem]) -> Result<Option<String>> {
    ensure_no_image_inputs(content)?;
    let Some(text) = content_items_to_text(content) else {
        return Ok(None);
    };
    let tag = if role == "user" && is_contextual_user_message_content(content) {
        "contextual_user"
    } else {
        role
    };
    Ok(render_text_block(tag, &text))
}

fn ensure_no_image_inputs(content: &[ContentItem]) -> Result<()> {
    if content
        .iter()
        .any(|item| matches!(item, ContentItem::InputImage { .. }))
    {
        return Err(CodexErr::UnsupportedOperation(
            "Claude Code carrier does not yet support image inputs on the structured stream path"
                .to_string(),
        ));
    }
    Ok(())
}

fn render_text_block(tag: &str, text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| format!("<{tag}>\n{trimmed}\n</{tag}>"))
}
