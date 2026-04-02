use crate::auth::AuthCredentialsStoreMode;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::error::CodexErr;
use crate::error::Result;
use codex_api::AnthropicImageSource;
use codex_api::AnthropicMessage;
use codex_api::AnthropicMessageContent;
use codex_api::AnthropicMessagesClient;
use codex_api::AnthropicMessagesRequest;
use codex_api::AnthropicOutputConfig;
use codex_api::AnthropicTool;
use codex_api::AnthropicToolChoice;
use codex_api::AnthropicToolKind;
use codex_api::AnthropicToolResultContent;
use codex_api::ReqwestTransport;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_tools::ToolSpec;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;
use url::Url;

use crate::default_client::build_reqwest_client;
use crate::model_provider_info::ModelProviderInfo;

const ANTHROPIC_BRIDGE_PROMPT: &str = concat!(
    "You are the main assistant running inside Codex through the Anthropic Messages API.\n",
    "When tools are available, use the Codex-provided tools rather than relying on hidden external tooling.\n",
    "Return the assistant response that should appear in the Codex conversation."
);

pub(super) async fn stream_anthropic_turn(
    provider: &ModelProviderInfo,
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    prompt: &Prompt,
    model_info: &ModelInfo,
    effort: Option<ReasoningEffortConfig>,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> Result<ResponseStream> {
    let transport = ReqwestTransport::new(build_reqwest_client());
    let auth = resolve_auth(codex_home, auth_credentials_store_mode).await?;
    let api_provider = provider.to_api_provider(/*auth_mode*/ None)?;
    let registry = AnthropicToolRegistry::from_prompt_tools(&prompt.tools)?;
    let request = AnthropicMessagesRequest {
        model: model_info.slug.clone(),
        messages: render_messages(&prompt.input, &registry)?,
        system: Some(build_system_prompt(&prompt.base_instructions.text)),
        tools: registry.definitions,
        tool_choice: if registry.tool_kinds.is_empty() {
            None
        } else {
            Some(AnthropicToolChoice::Auto)
        },
        max_tokens: max_output_tokens_for_model(&model_info.slug),
        output_config: build_output_config(model_info, effort),
        stream: true,
    };
    let client = AnthropicMessagesClient::new(transport, api_provider, auth);
    let mut stream = client
        .stream_request(&request, registry.tool_kinds, cancellation_token)
        .await
        .map_err(map_anthropic_error)?;
    let (tx_event, rx_event) = mpsc::channel(1600);
    tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            let mapped = event.map_err(map_anthropic_error);
            if tx_event.send(mapped).await.is_err() {
                return;
            }
        }
    });
    Ok(ResponseStream { rx_event })
}

fn map_anthropic_error(err: codex_api::ApiError) -> CodexErr {
    crate::api_bridge::map_api_error(err)
}

async fn resolve_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> Result<AnthropicApiAuthProvider> {
    let auth = crate::auth::resolve_anthropic_runtime_auth(codex_home, auth_credentials_store_mode)
        .await
        .map_err(|err| CodexErr::Stream(format!("resolve Anthropic auth: {err}"), None))?;
    Ok(match auth {
        Some(crate::auth::AnthropicRuntimeAuth::ApiKey(api_key)) => {
            AnthropicApiAuthProvider::ApiKey(api_key)
        }
        Some(crate::auth::AnthropicRuntimeAuth::OauthAccessToken(access_token)) => {
            AnthropicApiAuthProvider::Oauth(access_token)
        }
        None => AnthropicApiAuthProvider::None,
    })
}

#[derive(Clone)]
enum AnthropicApiAuthProvider {
    None,
    ApiKey(String),
    Oauth(String),
}

impl codex_api::AuthProvider for AnthropicApiAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        match self {
            Self::Oauth(token) => Some(token.clone()),
            Self::None | Self::ApiKey(_) => None,
        }
    }

    fn apply_headers(&self, headers: &mut HeaderMap) {
        match self {
            Self::None => {}
            Self::ApiKey(api_key) => {
                if let Ok(header) = HeaderValue::from_str(api_key) {
                    let _ = headers.insert("x-api-key", header);
                }
            }
            Self::Oauth(token) => {
                if let Ok(header) = HeaderValue::from_str(&format!("Bearer {token}")) {
                    let _ = headers.insert(http::header::AUTHORIZATION, header);
                }
            }
        }
    }
}

struct AnthropicToolRegistry {
    definitions: Vec<AnthropicTool>,
    tool_kinds: HashMap<String, AnthropicToolKind>,
}

impl AnthropicToolRegistry {
    fn from_prompt_tools(tools: &[ToolSpec]) -> Result<Self> {
        let mut definitions = Vec::new();
        let mut tool_kinds = HashMap::new();

        for tool in tools {
            match tool {
                ToolSpec::Function(tool) => {
                    definitions.push(AnthropicTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: serde_json::to_value(&tool.parameters).map_err(|err| {
                            CodexErr::Stream(
                                format!("serialize Anthropic tool schema: {err}"),
                                None,
                            )
                        })?,
                    });
                    tool_kinds.insert(tool.name.clone(), AnthropicToolKind::Function);
                }
                ToolSpec::Freeform(tool) => {
                    definitions.push(AnthropicTool {
                        name: tool.name.clone(),
                        description: format!(
                            "{}\n\nPut the complete tool payload into the `input` string field.",
                            tool.description
                        ),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string",
                                    "description": "Complete freeform payload for the tool."
                                }
                            },
                            "required": ["input"],
                            "additionalProperties": false
                        }),
                    });
                    tool_kinds.insert(tool.name.clone(), AnthropicToolKind::Custom);
                }
                ToolSpec::ToolSearch {
                    execution,
                    description,
                    parameters,
                } if execution == "client" => {
                    definitions.push(AnthropicTool {
                        name: "tool_search".to_string(),
                        description: description.clone(),
                        input_schema: serde_json::to_value(parameters).map_err(|err| {
                            CodexErr::Stream(
                                format!("serialize Anthropic tool_search schema: {err}"),
                                None,
                            )
                        })?,
                    });
                    tool_kinds.insert("tool_search".to_string(), AnthropicToolKind::ToolSearch);
                }
                ToolSpec::ToolSearch { .. } => {}
                ToolSpec::LocalShell {} => {
                    definitions.push(AnthropicTool {
                        name: "local_shell".to_string(),
                        description: "Runs a shell command and returns its output. Always pass the command as an argv array and set workdir when useful.".to_string(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "command": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Command argv. Prefer [\"bash\", \"-lc\", \"...\"] for shell commands."
                                },
                                "workdir": {
                                    "type": "string",
                                    "description": "Working directory for the command."
                                },
                                "timeout_ms": {
                                    "type": "number",
                                    "description": "Optional timeout in milliseconds."
                                }
                            },
                            "required": ["command"],
                            "additionalProperties": false
                        }),
                    });
                    tool_kinds.insert("local_shell".to_string(), AnthropicToolKind::Function);
                }
                ToolSpec::WebSearch { .. } | ToolSpec::ImageGeneration { .. } => {
                    // These special Responses-built-ins do not have a Codex function-tool handler yet.
                }
            }
        }

        Ok(Self {
            definitions,
            tool_kinds,
        })
    }
}

fn render_messages(
    items: &[ResponseItem],
    _registry: &AnthropicToolRegistry,
) -> Result<Vec<AnthropicMessage>> {
    let mut messages = Vec::<AnthropicMessage>::new();

    for item in items {
        if let Some((role, content)) = render_item(item)? {
            if content.is_empty() {
                continue;
            }
            if let Some(last) = messages.last_mut()
                && last.role == role
            {
                last.content.extend(content);
            } else {
                messages.push(AnthropicMessage { role, content });
            }
        }
    }

    Ok(messages)
}

fn render_item(item: &ResponseItem) -> Result<Option<(String, Vec<AnthropicMessageContent>)>> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let content = render_message_content(content)?;
            if content.is_empty() {
                return Ok(None);
            }
            let role = if role == "assistant" {
                "assistant".to_string()
            } else {
                "user".to_string()
            };
            Ok(Some((role, content)))
        }
        ResponseItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => {
            let input = serde_json::from_str::<Value>(arguments)
                .unwrap_or_else(|_| Value::String(arguments.clone()));
            Ok(Some((
                "assistant".to_string(),
                vec![AnthropicMessageContent::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                }],
            )))
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => Ok(Some((
            "assistant".to_string(),
            vec![AnthropicMessageContent::ToolUse {
                id: call_id.clone(),
                name: name.clone(),
                input: json!({ "input": input }),
            }],
        ))),
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            arguments,
            ..
        } => Ok(Some((
            "assistant".to_string(),
            vec![AnthropicMessageContent::ToolUse {
                id: call_id.clone(),
                name: "tool_search".to_string(),
                input: arguments.clone(),
            }],
        ))),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            action,
            ..
        } => Ok(Some((
            "assistant".to_string(),
            vec![AnthropicMessageContent::ToolUse {
                id: call_id.clone(),
                name: "local_shell".to_string(),
                input: serde_json::to_value(action).unwrap_or(Value::Null),
            }],
        ))),
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            let content = render_tool_result_content(&output.body)?;
            Ok(Some((
                "user".to_string(),
                vec![AnthropicMessageContent::ToolResult {
                    tool_use_id: call_id.clone(),
                    content,
                    is_error: output.success.map(|success| !success),
                }],
            )))
        }
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            tools,
            ..
        } => Ok(Some((
            "user".to_string(),
            vec![AnthropicMessageContent::ToolResult {
                tool_use_id: call_id.clone(),
                content: vec![AnthropicToolResultContent::Text {
                    text: serde_json::to_string(tools).unwrap_or_default(),
                }],
                is_error: None,
            }],
        ))),
        ResponseItem::Reasoning { .. } => Ok(None),
        ResponseItem::WebSearchCall { action, .. } => Ok(action.as_ref().map(|action| {
            (
                "assistant".to_string(),
                vec![AnthropicMessageContent::Text {
                    text: format!(
                        "[previous web_search_call]\n{}",
                        serde_json::to_string(action).unwrap_or_default()
                    ),
                }],
            )
        })),
        ResponseItem::ImageGenerationCall { .. } => Ok(None),
        _ => Ok(None),
    }
}

fn render_message_content(content: &[ContentItem]) -> Result<Vec<AnthropicMessageContent>> {
    let mut rendered = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.trim().is_empty() {
                    rendered.push(AnthropicMessageContent::Text { text: text.clone() });
                }
            }
            ContentItem::InputImage { image_url } => {
                rendered.push(AnthropicMessageContent::Image {
                    source: anthropic_image_source(image_url)?,
                })
            }
        }
    }
    Ok(rendered)
}

fn render_tool_result_content(
    body: &FunctionCallOutputBody,
) -> Result<Vec<AnthropicToolResultContent>> {
    let content = match body {
        FunctionCallOutputBody::Text(text) => {
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![AnthropicToolResultContent::Text { text: text.clone() }]
            }
        }
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .filter_map(render_tool_result_content_item)
            .collect::<Result<Vec<_>>>()?,
    };

    if content.is_empty() {
        return Ok(vec![AnthropicToolResultContent::Text {
            text: body
                .to_text()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| "[tool returned no textual output]".to_string()),
        }]);
    }

    Ok(content)
}

fn render_tool_result_content_item(
    item: &FunctionCallOutputContentItem,
) -> Option<Result<AnthropicToolResultContent>> {
    match item {
        FunctionCallOutputContentItem::InputText { text } => (!text.trim().is_empty())
            .then(|| Ok(AnthropicToolResultContent::Text { text: text.clone() })),
        FunctionCallOutputContentItem::InputImage { image_url, .. } => Some(
            anthropic_image_source(image_url)
                .map(|source| AnthropicToolResultContent::Image { source }),
        ),
    }
}

fn anthropic_image_source(image_url: &str) -> Result<AnthropicImageSource> {
    if let Some((media_type, data)) = parse_base64_data_url(image_url) {
        return Ok(AnthropicImageSource::Base64 { media_type, data });
    }

    let parsed = Url::parse(image_url).map_err(|err| {
        CodexErr::UnsupportedOperation(format!(
            "Native Anthropic runtime only supports data:image/... URLs or http(s) image URLs, got `{image_url}`: {err}"
        ))
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(AnthropicImageSource::Url {
            url: image_url.to_string(),
        }),
        scheme => Err(CodexErr::UnsupportedOperation(format!(
            "Native Anthropic runtime only supports data:image/... URLs or http(s) image URLs, got unsupported scheme `{scheme}`"
        ))),
    }
}

fn parse_base64_data_url(image_url: &str) -> Option<(String, String)> {
    let (prefix, payload) = image_url.split_once(',')?;
    let normalized_prefix = prefix.to_ascii_lowercase();
    if !normalized_prefix.starts_with("data:image/") || !normalized_prefix.ends_with(";base64") {
        return None;
    }
    let media_type = prefix[5..prefix.len() - ";base64".len()].to_string();
    Some((media_type, payload.to_string()))
}

fn build_system_prompt(base_instructions: &str) -> String {
    let trimmed = base_instructions.trim();
    if trimmed.is_empty() {
        return ANTHROPIC_BRIDGE_PROMPT.to_string();
    }
    format!("{trimmed}\n\n{ANTHROPIC_BRIDGE_PROMPT}")
}

fn build_output_config(
    model_info: &ModelInfo,
    effort: Option<ReasoningEffortConfig>,
) -> Option<AnthropicOutputConfig> {
    let effort = effort.or(model_info.default_reasoning_level)?;
    let effort = match effort {
        ReasoningEffortConfig::None | ReasoningEffortConfig::Minimal => return None,
        ReasoningEffortConfig::Low => "low",
        ReasoningEffortConfig::Medium => "medium",
        ReasoningEffortConfig::High => "high",
        ReasoningEffortConfig::XHigh => "max",
    };
    let supports_effort =
        model_info.slug.contains("opus-4-6") || model_info.slug.contains("sonnet-4-6");
    supports_effort.then(|| AnthropicOutputConfig {
        effort: Some(effort.to_string()),
    })
}

fn max_output_tokens_for_model(model: &str) -> i64 {
    let slug = model.to_ascii_lowercase();
    if slug.contains("opus-4-6") || slug.contains("sonnet-4") || slug.contains("haiku-4") {
        32_000
    } else if slug.contains("claude-3-sonnet") || slug.contains("3-5-sonnet") {
        8_192
    } else if slug.contains("claude-3-opus") || slug.contains("claude-3-haiku") {
        4_096
    } else {
        8_192
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn render_message_content_preserves_text_and_input_images() {
        let content = render_message_content(&[
            ContentItem::InputText {
                text: "describe this".to_string(),
            },
            ContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
            },
        ])
        .expect("content should render");

        assert_eq!(
            content,
            vec![
                AnthropicMessageContent::Text {
                    text: "describe this".to_string(),
                },
                AnthropicMessageContent::Image {
                    source: AnthropicImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "AAA".to_string(),
                    },
                },
            ]
        );
    }

    #[test]
    fn render_tool_result_content_preserves_images() {
        let body = FunctionCallOutputBody::ContentItems(vec![
            FunctionCallOutputContentItem::InputText {
                text: "saved preview".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "https://example.com/preview.png".to_string(),
                detail: None,
            },
        ]);

        assert_eq!(
            render_tool_result_content(&body).expect("tool result should render"),
            vec![
                AnthropicToolResultContent::Text {
                    text: "saved preview".to_string(),
                },
                AnthropicToolResultContent::Image {
                    source: AnthropicImageSource::Url {
                        url: "https://example.com/preview.png".to_string(),
                    },
                },
            ]
        );
    }
}
