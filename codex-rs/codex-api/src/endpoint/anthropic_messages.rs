use crate::auth::AuthProvider;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::sse::anthropic::spawn_anthropic_stream;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use http::HeaderValue;
use http::Method;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const EFFORT_BETA_HEADER: &str = "effort-2025-11-24";

pub struct AnthropicMessagesClient<T: HttpTransport, A: AuthProvider> {
    transport: T,
    provider: Provider,
    auth: A,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
}

impl<T: HttpTransport, A: AuthProvider> AnthropicMessagesClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            request_telemetry: request,
            ..self
        }
    }

    pub async fn stream_request(
        &self,
        request: &AnthropicMessagesRequest,
        tool_kinds: HashMap<String, AnthropicToolKind>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<ResponseStream, ApiError> {
        let mut headers = self.provider.headers.clone();
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        if request
            .output_config
            .as_ref()
            .and_then(|config| config.effort.as_ref())
            .is_some()
        {
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_static(EFFORT_BETA_HEADER),
            );
        }
        self.auth.apply_headers(&mut headers);

        let body = serde_json::to_value(request).map_err(|err| {
            ApiError::Stream(format!("failed to encode anthropic request: {err}"))
        })?;

        let mut req = self.provider.build_request(Method::POST, "messages");
        req.headers.extend(headers);
        req.body = Some(body);

        let _ = self.request_telemetry.as_ref();
        let stream_response = self
            .transport
            .stream(req)
            .await
            .map_err(ApiError::Transport)?;

        Ok(spawn_anthropic_stream(
            stream_response,
            self.provider.stream_idle_timeout,
            tool_kinds,
            cancellation_token,
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
    pub max_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<AnthropicOutputConfig>,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicMessageContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicMessageContent {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<AnthropicToolResultContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolResultContent {
    Text { text: String },
    Image { source: AnthropicImageSource },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AnthropicOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}
