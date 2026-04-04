use crate::agent::external::ClaudeCliRequest;
use crate::agent::external::ClaudeCliSession;
use crate::agent::external::run_claude_cli_stream_json_controlled;
use crate::claude_code_control::ClaudeControlRequest;
use crate::claude_code_control::ControlRequestParseOutcome;
use crate::claude_code_control::parse_control_request_line;
use crate::claude_code_stream::ClaudeCodeStreamAccumulator;
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
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
    let controlled = run_claude_cli_stream_json_controlled(
        claude_cli,
        ClaudeCliRequest {
            cwd: cwd.to_path_buf(),
            model: model_info.slug.clone(),
            system_prompt: build_system_prompt(&prompt.base_instructions.text),
            user_prompt,
            session: ClaudeCliSession::Ephemeral,
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
    let mut raw_lines = controlled.lines;
    let control_responder = controlled.control_responder;

    let (tx_event, rx_event) = mpsc::channel(1600);
    tokio::spawn(async move {
        let mut accumulator = ClaudeCodeStreamAccumulator::default();
        while let Some(line) = raw_lines.recv().await {
            match line {
                Ok(line) => {
                    match parse_control_request_line(&line, &control_responder) {
                        Ok(ControlRequestParseOutcome::ControlRequest(
                            ClaudeControlRequest::CanUseTool(permission_request),
                        )) => {
                            if tx_event
                                .send(Ok(ResponseEvent::ClaudeCodePermissionRequest(
                                    permission_request,
                                )))
                                .await
                                .is_err()
                            {
                                return;
                            }
                            continue;
                        }
                        Ok(ControlRequestParseOutcome::ControlRequest(
                            ClaudeControlRequest::UnsupportedSubtype { subtype },
                        )) => {
                            let message = format!(
                                "Claude Code carrier emitted an unsupported control_request subtype `{subtype}`"
                            );
                            let _ = tx_event.send(Err(CodexErr::Stream(message, None))).await;
                            return;
                        }
                        Ok(ControlRequestParseOutcome::NotControlRequest) => {}
                        Err(message) => {
                            let _ = tx_event.send(Err(CodexErr::Stream(message, None))).await;
                            return;
                        }
                    }
                    let events = match accumulator.push_line(&line) {
                        Ok(events) => events,
                        Err(err) => {
                            let _ = tx_event
                                .send(Err(CodexErr::Stream(err.to_string(), None)))
                                .await;
                            return;
                        }
                    };
                    for event in events {
                        if tx_event.send(Ok(event)).await.is_err() {
                            return;
                        }
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

        let summary = accumulator.finish();
        let _ = tx_event
            .send(Ok(ResponseEvent::Completed {
                response_id: summary.response_id,
                token_usage: summary.token_usage,
            }))
            .await;
    });

    Ok(ResponseStream { rx_event })
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
