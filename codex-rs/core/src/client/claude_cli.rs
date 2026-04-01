use crate::agent::external::ClaudeCliRequest;
use crate::agent::external::run_claude_cli;
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
use uuid::Uuid;

const CLAUDE_BRIDGE_PROMPT: &str = concat!(
    "You are the main assistant running inside Codex through Claude Code CLI.\n",
    "Use Claude Code's built-in tools when inspection, edits, or shell work are required.\n",
    "Return only the assistant response that should appear in the Codex conversation."
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
    let output = run_claude_cli(
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

    let response_id = format!("claude-cli-{}", Uuid::new_v4());
    let item = ResponseItem::Message {
        id: Some(response_id.clone()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText { text: output }],
        end_turn: Some(true),
        phase: None,
    };
    let (tx_event, rx_event) = mpsc::channel(3);
    tx_event.send(Ok(ResponseEvent::Created)).await.ok();
    tx_event
        .send(Ok(ResponseEvent::OutputItemDone(item)))
        .await
        .ok();
    tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id,
            token_usage: None,
        }))
        .await
        .ok();
    drop(tx_event);
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
            "Claude CLI main sessions do not yet support image inputs".to_string(),
        ));
    }
    Ok(())
}

fn render_text_block(tag: &str, text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| format!("<{tag}>\n{trimmed}\n</{tag}>"))
}
