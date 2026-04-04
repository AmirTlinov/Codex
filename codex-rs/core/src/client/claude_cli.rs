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
use codex_tools::ToolSpec;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CLAUDE_BRIDGE_PROMPT: &str = concat!(
    "You are the main assistant running inside Codex through Claude Code.\n",
    "Return the assistant response that should appear in the Codex conversation."
);
const CODEX_MCP_BRIDGE_PROMPT: &str = concat!(
    "An internal Codex MCP bridge is available in this session.\n",
    "If you need Codex-owned tools or a Codex-run worker, use `mcp__codex__codex` to start that task, ",
    "`mcp__codex__codex-reply` to continue it, and `mcp__codex__codex-shell` for exact shell commands.\n",
    "Prefer this bridge when you need Codex MCP servers, Codex-native tool behavior, or capabilities ",
    "that are not directly available through Claude Code built-ins."
);
const CLAUDE_RUNTIME_TRUTH_PROMPT: &str = concat!(
    "The user prompt may include a `<codex_runtime_truth>` block with current Codex runtime context.\n",
    "Treat that block as authoritative for this turn.\n",
    "When it includes collaboration-mode, permissions, environment, subagent, or tool-inventory updates, ",
    "prefer the latest such update over older conversation text or guesses."
);

pub(super) async fn stream_claude_cli_turn(
    claude_cli: &ClaudeCliConfig,
    prompt: &Prompt,
    model_info: &ModelInfo,
    cwd: &std::path::Path,
    effort: Option<ReasoningEffortConfig>,
    cancellation_token: CancellationToken,
) -> Result<ResponseStream> {
    let rendered_prompt = render_claude_prompt(prompt)?;
    let controlled = run_claude_cli_stream_json_controlled(
        claude_cli,
        ClaudeCliRequest {
            cwd: cwd.to_path_buf(),
            model: model_info.slug.clone(),
            system_prompt: build_system_prompt(
                &prompt.base_instructions.text,
                claude_cli.codex_self_exe.is_some(),
                /*include_runtime_truth_guidance*/
                !rendered_prompt.runtime_sections.is_empty(),
            ),
            user_prompt: rendered_prompt.user_prompt,
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

struct RenderedClaudePrompt {
    runtime_sections: Vec<String>,
    user_prompt: String,
}

fn build_system_prompt(
    base_instructions: &str,
    codex_mcp_bridge_available: bool,
    include_runtime_truth_guidance: bool,
) -> String {
    let trimmed = base_instructions.trim();
    let mut sections = Vec::new();
    if !trimmed.is_empty() {
        sections.push(trimmed.to_string());
    }
    sections.push(CLAUDE_BRIDGE_PROMPT.to_string());
    if codex_mcp_bridge_available {
        sections.push(CODEX_MCP_BRIDGE_PROMPT.to_string());
    }
    if include_runtime_truth_guidance {
        sections.push(CLAUDE_RUNTIME_TRUTH_PROMPT.to_string());
    }
    sections.join("\n\n")
}

fn render_claude_prompt(prompt: &Prompt) -> Result<RenderedClaudePrompt> {
    let mut runtime_sections = Vec::new();
    let mut conversation_sections = Vec::new();

    for item in &prompt.input {
        match item {
            ResponseItem::Message { role, content, .. } if role == "developer" => {
                if let Some(text) = content_items_to_text(content) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        runtime_sections.push(format!(
                            "<codex_runtime_update role=\"developer\">\n{trimmed}\n</codex_runtime_update>"
                        ));
                    }
                }
            }
            ResponseItem::Message { role, content, .. }
                if role == "user" && is_contextual_user_message_content(content) =>
            {
                if let Some(text) = content_items_to_text(content) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        runtime_sections.push(format!(
                            "<codex_runtime_update role=\"contextual_user\">\n{trimmed}\n</codex_runtime_update>"
                        ));
                    }
                }
            }
            _ => {
                if let Some(section) = render_item(item)? {
                    conversation_sections.push(section);
                }
            }
        }
    }

    if let Some(tool_summary) = render_tool_summary(&prompt.tools) {
        runtime_sections.push(tool_summary);
    }

    let user_prompt = render_claude_user_prompt(&runtime_sections, &conversation_sections);
    Ok(RenderedClaudePrompt {
        runtime_sections,
        user_prompt,
    })
}

fn render_claude_user_prompt(
    runtime_sections: &[String],
    conversation_sections: &[String],
) -> String {
    let mut sections = Vec::new();
    if !runtime_sections.is_empty() {
        sections.push(format!(
            "<codex_runtime_truth>\n{}\n</codex_runtime_truth>",
            runtime_sections.join("\n\n")
        ));
    }
    sections.push(render_conversation_context(conversation_sections));
    sections.join("\n\n")
}

fn render_conversation_context(sections: &[String]) -> String {
    if sections.is_empty() {
        return
            "<conversation_context>\nNo prior turn items were provided.\n</conversation_context>"
                .to_string();
    }
    format!(
        "<conversation_context>\n{}\n</conversation_context>",
        sections.join("\n\n")
    )
}

fn render_tool_summary(tools: &[ToolSpec]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    let tool_names = tools
        .iter()
        .map(ToolSpec::name)
        .collect::<Vec<_>>()
        .join(", ");
    let mut sections = vec![
        "The following tool names come from Codex's current turn-level tool inventory.".to_string(),
        "They describe what Codex itself can do in this turn, not necessarily direct Claude Code built-ins. When you need one of these capabilities from the Claude carrier, prefer the Codex bridge or a Codex-run worker instead of claiming direct built-in access.".to_string(),
        format!("Current Codex tool inventory: {tool_names}"),
    ];
    let detailed_tools = tools
        .iter()
        .filter_map(render_tool_detail)
        .collect::<Vec<_>>();
    if !detailed_tools.is_empty() {
        sections.push(detailed_tools.join("\n\n"));
    }
    Some(format!(
        "<codex_available_tools>\n{}\n</codex_available_tools>",
        sections.join("\n\n")
    ))
}

fn render_tool_detail(tool: &ToolSpec) -> Option<String> {
    let (name, description) = match tool {
        ToolSpec::Function(tool) => (tool.name.as_str(), tool.description.as_str()),
        ToolSpec::Freeform(tool) => (tool.name.as_str(), tool.description.as_str()),
        ToolSpec::ToolSearch { description, .. } => ("tool_search", description.as_str()),
        ToolSpec::LocalShell {} => {
            return Some(
                "<tool name=\"local_shell\">\nRuns a local shell command and returns its output.\n</tool>"
                    .to_string(),
            );
        }
        ToolSpec::WebSearch { .. } => {
            return Some(
                "<tool name=\"web_search\">\nPerforms web search when the current model/runtime supports it.\n</tool>"
                    .to_string(),
            );
        }
        ToolSpec::ImageGeneration { .. } => {
            return Some(
                "<tool name=\"image_generation\">\nGenerates images when the current model/runtime supports it.\n</tool>"
                    .to_string(),
            );
        }
    };

    if !matches!(
        name,
        "spawn_agent"
            | "send_input"
            | "wait_agent"
            | "close_agent"
            | "request_user_input"
            | "update_plan"
            | "exec_command"
            | "write_stdin"
            | "js_repl"
            | "apply_patch"
    ) {
        return None;
    }

    let description = compact_tool_description(name, description);
    (!description.is_empty()).then(|| format!("<tool name=\"{name}\">\n{description}\n</tool>"))
}

fn compact_tool_description(name: &str, description: &str) -> String {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if name == "spawn_agent" {
        return trimmed
            .split("\n### ")
            .next()
            .unwrap_or(trimmed)
            .trim()
            .to_string();
    }
    trimmed
        .split("\n\n")
        .next()
        .unwrap_or(trimmed)
        .trim()
        .to_string()
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
