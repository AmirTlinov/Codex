use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::models::ShellToolCallParams;
use codex_protocol::user_input::UserInput;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::error;
use uuid::Uuid;

use crate::codex::TurnContext;
use crate::protocol::EventMsg;
use crate::protocol::TaskStartedEvent;
use crate::state::TaskKind;
use crate::tools::context::ToolPayload;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolRouter;
use crate::turn_diff_tracker::TurnDiffTracker;

use super::SessionTask;
use super::SessionTaskContext;

const USER_SHELL_TOOL_NAME: &str = "local_shell";

#[derive(Clone)]
pub(crate) struct UserShellCommandTask {
    command: String,
}

impl UserShellCommandTask {
    pub(crate) fn new(command: String) -> Self {
        Self { command }
    }
}

#[async_trait]
impl SessionTask for UserShellCommandTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        turn_context: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let event = EventMsg::TaskStarted(TaskStartedEvent {
            model_context_window: turn_context.client.get_model_context_window(),
        });
        let session = session.clone_session();
        session.send_event(turn_context.as_ref(), event).await;

        let (command_body, directive) = extract_run_in_background_directive(&self.command);

        // Execute the user's script under their default shell when known; this
        // allows commands that use shell features (pipes, &&, redirects, etc.).
        // We do not source rc files or otherwise reformat the script.
        let shell_invocation = match session.user_shell() {
            crate::shell::Shell::Zsh(zsh) => vec![
                zsh.shell_path.clone(),
                "-lc".to_string(),
                command_body.clone(),
            ],
            crate::shell::Shell::Bash(bash) => vec![
                bash.shell_path.clone(),
                "-lc".to_string(),
                command_body.clone(),
            ],
            crate::shell::Shell::PowerShell(ps) => vec![
                ps.exe.clone(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                command_body.clone(),
            ],
            crate::shell::Shell::Unknown => {
                shlex::split(&command_body).unwrap_or_else(|| vec![command_body.clone()])
            }
        };

        let run_in_background = directive.run.or_else(|| {
            if directive.bookmark.is_some() || directive.description.is_some() {
                Some(true)
            } else {
                None
            }
        });

        let params = ShellToolCallParams {
            command: shell_invocation,
            workdir: None,
            timeout_ms: None,
            with_escalated_permissions: None,
            justification: None,
            run_in_background,
            description: directive.description,
            manage_process: Some(true),
            tail_lines: None,
            bookmark: directive.bookmark,
        };

        let tool_call = ToolCall {
            tool_name: USER_SHELL_TOOL_NAME.to_string(),
            call_id: Uuid::new_v4().to_string(),
            payload: ToolPayload::LocalShell { params },
        };

        let router = Arc::new(ToolRouter::from_config(&turn_context.tools_config, None));
        let tracker = Arc::new(Mutex::new(TurnDiffTracker::new()));
        let runtime = ToolCallRuntime::new(
            Arc::clone(&router),
            Arc::clone(&session),
            Arc::clone(&turn_context),
            Arc::clone(&tracker),
        );

        if let Err(err) = runtime
            .handle_tool_call(tool_call, cancellation_token)
            .await
        {
            error!("user shell command failed: {err:?}");
        }
        None
    }
}

#[derive(Default)]
struct RunInBackgroundDirective {
    run: Option<bool>,
    bookmark: Option<String>,
    description: Option<String>,
}

fn extract_run_in_background_directive(command: &str) -> (String, RunInBackgroundDirective) {
    let mut body = command.trim_end().to_string();
    let mut directive = RunInBackgroundDirective::default();
    const NEEDLE: &str = "run_in_background:";

    let lower = body.to_ascii_lowercase();
    if let Some(pos) = lower.rfind(NEEDLE) {
        let preceding = &body[..pos];
        if preceding
            .as_bytes()
            .last()
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            return (body, directive);
        }

        let value_str = body[pos + NEEDLE.len()..].trim();
        if !value_str.is_empty()
            && let Some(tokens) = shlex::split(value_str)
        {
            for token in tokens {
                let lower_token = token.to_ascii_lowercase();
                match lower_token.as_str() {
                    "true" => {
                        directive.run = Some(true);
                        continue;
                    }
                    "false" => {
                        directive.run = Some(false);
                        continue;
                    }
                    _ => {}
                }

                if let Some((key, value)) = token.split_once('=') {
                    match key.to_ascii_lowercase().as_str() {
                        "bookmark" => {
                            if !value.is_empty() {
                                directive.bookmark = Some(value.to_string());
                            }
                        }
                        "description" => {
                            if !value.is_empty() {
                                directive.description = Some(value.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if directive.run.is_some()
            || directive.bookmark.is_some()
            || directive.description.is_some()
        {
            body = preceding.trim_end().to_string();
        }
    }

    let body = normalize_background_markers(&body, &mut directive);
    (body, directive)
}

fn normalize_background_markers(command: &str, directive: &mut RunInBackgroundDirective) -> String {
    let mut text = command.trim().to_string();
    let mut implied_background = false;

    for keyword in ["nohup", "setsid"] {
        if let Some(rest) = strip_prefix_keyword(&text, keyword) {
            text = rest.to_string();
            implied_background = true;
        }
    }

    let (stripped, removed) = strip_trailing_ampersand(&text);
    if removed {
        text = stripped;
        implied_background = true;
    }

    if implied_background && directive.run.is_none() {
        directive.run = Some(true);
    }

    text.trim().to_string()
}

fn strip_prefix_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let trimmed = input.trim_start();
    let keyword_len = keyword.len();
    if trimmed.len() < keyword_len {
        return None;
    }
    let prefix = &trimmed[..keyword_len];
    if !prefix.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let remainder = &trimmed[keyword_len..];
    if let Some(ch) = remainder.chars().next()
        && !ch.is_whitespace()
    {
        return None;
    }
    Some(remainder.trim_start())
}

fn strip_trailing_ampersand(input: &str) -> (String, bool) {
    let trimmed = input.trim_end();
    if trimmed.is_empty() {
        return (trimmed.to_string(), false);
    }
    let mut chars = trimmed.chars().rev();
    if let Some(last) = chars.next()
        && last == '&'
    {
        let remaining: String = chars.rev().collect();
        let remaining = remaining.trim_end();
        if remaining.ends_with('&') {
            // Treat trailing "&&" as logical operator, not background marker.
            return (input.trim().to_string(), false);
        }
        return (remaining.to_string(), true);
    }
    (trimmed.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::extract_run_in_background_directive;

    #[test]
    fn strips_run_in_background_suffix() {
        let (body, opts) = extract_run_in_background_directive("npm start run_in_background: true");
        assert_eq!(body, "npm start");
        assert_eq!(opts.run, Some(true));

        let (body, opts) =
            extract_run_in_background_directive("npm start run_in_background: false");
        assert_eq!(body, "npm start");
        assert_eq!(opts.run, Some(false));
    }

    #[test]
    fn leaves_command_untouched_when_flag_missing() {
        let (body, opts) = extract_run_in_background_directive("npm run test");
        assert_eq!(body, "npm run test");
        assert!(opts.run.is_none());
    }

    #[test]
    fn parses_bookmark_and_description() {
        let (body, opts) = extract_run_in_background_directive(
            "npm start run_in_background: bookmark=build description=\"watch server\"",
        );
        assert_eq!(body, "npm start");
        assert_eq!(opts.run, None);
        assert_eq!(opts.bookmark.as_deref(), Some("build"));
        assert_eq!(opts.description.as_deref(), Some("watch server"));
    }

    #[test]
    fn infers_background_from_trailing_ampersand() {
        let (body, opts) = extract_run_in_background_directive("sleep 30 &");
        assert_eq!(body, "sleep 30");
        assert_eq!(opts.run, Some(true));
    }

    #[test]
    fn infers_background_from_nohup_prefix() {
        let (body, opts) = extract_run_in_background_directive("nohup npm run dev &");
        assert_eq!(body, "npm run dev");
        assert_eq!(opts.run, Some(true));
    }
}
