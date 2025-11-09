use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::truncate::truncate_middle;
use codex_utils_tokenizer::Tokenizer;
use tracing::warn;

mod errors;
mod session;
mod session_manager;

pub use errors::UnifiedExecError;
pub use session::UnifiedExecOutputWindow;
pub(crate) use session::UnifiedExecSession;
pub use session::UnifiedExecSessionOutput;
pub use session::UnifiedExecSessionSnapshot;
pub(crate) use session_manager::TerminateDisposition;
pub use session_manager::UnifiedExecSessionManager;

pub const MIN_YIELD_TIME_MS: u64 = 100;
pub const MAX_YIELD_TIME_MS: u64 = 60_000;
pub const MAX_OUTPUT_TOKENS: usize = 32_768;
pub const DEFAULT_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone)]
pub struct UnifiedExecContext {
    pub session: Arc<Session>,
    pub turn: Arc<TurnContext>,
    pub call_id: String,
}

impl UnifiedExecContext {
    pub fn new(session: Arc<Session>, turn: Arc<TurnContext>, call_id: String) -> Self {
        Self {
            session,
            turn,
            call_id,
        }
    }
}

impl fmt::Debug for UnifiedExecContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnifiedExecContext")
            .field("call_id", &self.call_id)
            .finish()
    }
}

pub struct WriteStdinRequest<'a> {
    pub session_id: i32,
    pub input: &'a str,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<usize>,
}

pub struct UnifiedExecRequest<'a> {
    pub session_id: Option<i32>,
    pub input_chunks: &'a [String],
    pub timeout_ms: Option<u64>,
}

pub struct UnifiedExecResult {
    pub session_id: Option<i32>,
    pub output: String,
}

pub struct UnifiedExecResponse {
    pub event_call_id: String,
    pub wall_time: Duration,
    pub output: String,
    pub session_id: Option<i32>,
    pub exit_code: Option<i32>,
}

pub struct UnifiedExecKillResult {
    pub exit_code: i32,
    pub aggregated_output: String,
    pub call_id: String,
    pub timed_out: bool,
}

pub(crate) struct SessionEntry {
    pub session: Arc<UnifiedExecSession>,
    pub session_ref: Option<Arc<Session>>,
    pub turn_ref: Option<Arc<TurnContext>>,
    pub call_id: Option<String>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub started_at: Instant,
}

impl fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEntry")
            .field("call_id", &self.call_id)
            .field("command", &self.command)
            .field("cwd", &self.cwd)
            .field("started_at", &self.started_at)
            .finish()
    }
}

pub(crate) fn clamp_yield_time(requested: Option<u64>) -> u64 {
    match requested {
        Some(ms) => ms.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS),
        None => MIN_YIELD_TIME_MS,
    }
}

pub(crate) fn resolve_max_tokens(requested: Option<usize>) -> Option<usize> {
    match requested {
        Some(0) => None,
        Some(value) => Some(value.min(MAX_OUTPUT_TOKENS)),
        None => None,
    }
}

pub(crate) fn truncate_output_to_tokens(
    text: &str,
    max_tokens: Option<usize>,
) -> (String, Option<usize>) {
    let Some(limit) = max_tokens else {
        return (text.to_string(), None);
    };

    if limit == 0 {
        let approx_tokens = text.len().div_ceil(4);
        let message = format!("…{approx_tokens} tokens truncated…");
        return (message, Some(approx_tokens));
    }

    match Tokenizer::try_default() {
        Ok(tokenizer) => {
            let tokens = tokenizer.encode(text, false);
            let original_len = tokens.len();
            if original_len <= limit {
                return (text.to_string(), None);
            }

            let removed = original_len - limit;
            let prefix_len = limit / 2;
            let suffix_len = limit.saturating_sub(prefix_len);

            let prefix = if prefix_len > 0 {
                tokenizer
                    .decode(&tokens[..prefix_len])
                    .unwrap_or_else(|_| text.chars().take(prefix_len * 4).collect())
            } else {
                String::new()
            };
            let suffix = if suffix_len > 0 {
                tokenizer
                    .decode(&tokens[original_len - suffix_len..])
                    .unwrap_or_else(|_| {
                        text.chars()
                            .rev()
                            .take(suffix_len * 4)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect()
                    })
            } else {
                String::new()
            };

            let notice = format!("…{removed} tokens truncated…");
            let mut output = String::with_capacity(prefix.len() + suffix.len() + notice.len() + 2);
            if !prefix.is_empty() {
                output.push_str(&prefix);
                ensure_trailing_newline(&mut output);
            }
            output.push_str(&notice);
            if suffix_len > 0 {
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&suffix);
            }

            (output, Some(original_len))
        }
        Err(err) => {
            warn!(
                error = ?err,
                "failed to initialize tokenizer; falling back to byte-based truncation"
            );
            let approx_bytes = limit.saturating_mul(4);
            let (output, estimate) = truncate_middle(text, approx_bytes);
            let approx_tokens = estimate
                .and_then(|value| usize::try_from(value).ok())
                .or_else(|| Some(text.len().div_ceil(4)));
            (output, approx_tokens)
        }
    }
}

fn ensure_trailing_newline(buffer: &mut String) {
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }
}
