use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use std::borrow::Cow;

use rand::Rng;
use tracing::debug;
use tracing::error;

const INITIAL_DELAY_MS: u64 = 200;
const BACKOFF_FACTOR: f64 = 2.0;

pub(crate) fn backoff(attempt: u64) -> Duration {
    let exp = BACKOFF_FACTOR.powi(attempt.saturating_sub(1) as i32);
    let base = (INITIAL_DELAY_MS as f64 * exp) as u64;
    let jitter = rand::rng().random_range(0.9..1.1);
    Duration::from_millis((base as f64 * jitter) as u64)
}

pub(crate) fn error_or_panic(message: impl std::string::ToString) {
    if cfg!(debug_assertions) {
        panic!("{}", message.to_string());
    } else {
        error!("{}", message.to_string());
    }
}

pub(crate) fn try_parse_error_message(text: &str) -> String {
    debug!("Parsing server error response: {}", text);
    let json = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
    if let Some(error) = json.get("error")
        && let Some(message) = error.get("message")
        && let Some(message_str) = message.as_str()
    {
        return message_str.to_string();
    }
    if text.is_empty() {
        return "Unknown error".to_string();
    }
    text.to_string()
}

pub(crate) fn escape_xml_text(text: &str) -> Cow<'_, str> {
    escape_xml(text, false)
}

pub(crate) fn escape_xml_attr(text: &str) -> Cow<'_, str> {
    escape_xml(text, true)
}

fn escape_xml(text: &str, is_attr: bool) -> Cow<'_, str> {
    let needs_escape = text.chars().any(|ch| match ch {
        '&' | '<' | '>' => true,
        '"' | '\'' if is_attr => true,
        _ => false,
    });
    if !needs_escape {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if is_attr => out.push_str("&quot;"),
            '\'' if is_attr => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    Cow::Owned(out)
}

pub fn resolve_path(base: &Path, path: &PathBuf) -> PathBuf {
    if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_parse_error_message() {
        let text = r#"{
  "error": {
    "message": "Your refresh token has already been used to generate a new access token. Please try signing in again.",
    "type": "invalid_request_error",
    "param": null,
    "code": "refresh_token_reused"
  }
}"#;
        let message = try_parse_error_message(text);
        assert_eq!(
            message,
            "Your refresh token has already been used to generate a new access token. Please try signing in again."
        );
    }

    #[test]
    fn test_try_parse_error_message_no_error() {
        let text = r#"{"message": "test"}"#;
        let message = try_parse_error_message(text);
        assert_eq!(message, r#"{"message": "test"}"#);
    }

    #[test]
    fn test_escape_xml_text_escapes_special_chars() {
        assert_eq!(escape_xml_text("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn test_escape_xml_attr_escapes_quotes() {
        assert_eq!(escape_xml_attr("\"'&<>"), "&quot;&apos;&amp;&lt;&gt;");
    }
}
