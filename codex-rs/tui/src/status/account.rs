#[derive(Debug, Clone)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    AnthropicOauth {
        email: Option<String>,
        subscription: Option<String>,
    },
    AnthropicApiKey,
    ApiKey,
}
