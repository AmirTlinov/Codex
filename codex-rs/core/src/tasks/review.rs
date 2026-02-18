use std::sync::Arc;

use async_trait::async_trait;
use codex_protocol::config_types::ReviewMode;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentMessageDeltaEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ReviewOutputEvent;
use tokio_util::sync::CancellationToken;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex_delegate::run_codex_thread_one_shot;
use crate::config::Constrained;
use crate::orchestration::lifecycle::RunbookMemoryRecord;
use crate::orchestration::pr_package::ReviewChannels;
use crate::orchestration::pr_package::enforce_review_mode_gate;
use crate::review_format::format_review_findings_block;
use crate::review_format::render_review_output_text;
use crate::state::TaskKind;
use codex_protocol::user_input::UserInput;
use std::time::Duration;
use std::time::Instant;
use tracing::warn;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Clone, Copy)]
pub(crate) struct ReviewTask;

impl ReviewTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionTask for ReviewTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let _ = session
            .session
            .services
            .otel_manager
            .counter("codex.task.review", 1, &[]);

        let review_mode = ctx.config.review_mode;
        let remote_trigger = ctx
            .config
            .review_remote
            .trigger
            .as_deref()
            .unwrap_or("@codex review");
        let remote_review_requested = remote_review_trigger_requested(&input, remote_trigger);
        {
            let now = Instant::now();
            let record_id = format!("review-{}", ctx.sub_id);
            let session_handle = session.clone_session();
            let mut lifecycle = session_handle.team_lifecycle_store.lock().await;
            let record = RunbookMemoryRecord {
                record_id,
                owner: "review-task".to_string(),
                payload: format!("mode={review_mode:?};turn={}", ctx.sub_id),
                expires_at: now + Duration::from_secs(1),
                archived_at: None,
            };
            if let Err(err) = lifecycle.put_runbook_memory(record) {
                warn!("failed to persist review lifecycle record: {err}");
            }
            let archived_ids = lifecycle.sweep_expired_runbook_memory(now + Duration::from_secs(1));
            if let Some(first_archived_id) = archived_ids.first() {
                let _ = lifecycle.archived_record(first_archived_id);
            }
        }

        let mut local_review_completed = false;
        let mut output = match review_mode {
            ReviewMode::Remote => Some(ReviewOutputEvent {
                overall_explanation: remote_review_instructions(ctx.config.as_ref()),
                ..Default::default()
            }),
            ReviewMode::Local | ReviewMode::Hybrid => {
                // Start sub-codex conversation and get the receiver for events.
                let (mut out, completed) = match start_review_conversation(
                    session.clone(),
                    ctx.clone(),
                    input,
                    cancellation_token.clone(),
                )
                .await
                {
                    Some(receiver) => {
                        process_review_events(session.clone(), ctx.clone(), receiver).await
                    }
                    None => (None, false),
                };
                local_review_completed = completed;

                if matches!(review_mode, ReviewMode::Hybrid) {
                    let remote_note = remote_review_instructions(ctx.config.as_ref());
                    out = match out {
                        Some(mut ev) => {
                            if ev.overall_explanation.trim().is_empty() {
                                ev.overall_explanation = remote_note;
                            } else {
                                ev.overall_explanation = format!(
                                    "{}\n\n{}",
                                    ev.overall_explanation.trim_end(),
                                    remote_note
                                );
                            }
                            Some(ev)
                        }
                        None => Some(ReviewOutputEvent {
                            overall_explanation: format!(
                                "Local review did not complete.\n\n{remote_note}"
                            ),
                            ..Default::default()
                        }),
                    };
                }

                out
            }
        };
        let channels = ReviewChannels {
            local_pass: matches!(review_mode, ReviewMode::Local | ReviewMode::Hybrid)
                && local_review_completed,
            remote_pass: remote_review_requested,
        };
        if let Err(err) =
            enforce_review_mode_gate(review_mode, ctx.config.review_hybrid_policy, channels)
        {
            warn!("review mode gate check failed: {err}");
            output = Some(attach_review_gate_failure(
                output.take(),
                &err,
                channels,
                remote_trigger,
            ));
        }
        if !cancellation_token.is_cancelled() {
            exit_review_mode(session.clone_session(), output.clone(), ctx.clone()).await;
        }
        None
    }

    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        exit_review_mode(session.clone_session(), None, ctx).await;
    }
}

async fn start_review_conversation(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    input: Vec<UserInput>,
    cancellation_token: CancellationToken,
) -> Option<async_channel::Receiver<Event>> {
    let config = ctx.config.clone();
    let mut sub_agent_config = config.as_ref().clone();
    // Carry over review-only feature restrictions so the delegate cannot
    // re-enable blocked tools (web search, view image).
    if let Err(err) = sub_agent_config
        .web_search_mode
        .set(WebSearchMode::Disabled)
    {
        panic!("by construction Constrained<WebSearchMode> must always support Disabled: {err}");
    }

    // Set explicit review rubric for the sub-agent
    sub_agent_config.base_instructions = Some(crate::REVIEW_PROMPT.to_string());
    sub_agent_config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);

    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| ctx.model_info.slug.clone());
    sub_agent_config.model = Some(model);
    (run_codex_thread_one_shot(
        sub_agent_config,
        session.auth_manager(),
        session.models_manager(),
        input,
        session.clone_session(),
        ctx.clone(),
        cancellation_token,
        None,
    )
    .await)
        .ok()
        .map(|io| io.rx_event)
}

async fn process_review_events(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    receiver: async_channel::Receiver<Event>,
) -> (Option<ReviewOutputEvent>, bool) {
    let mut prev_agent_message: Option<Event> = None;
    while let Ok(event) = receiver.recv().await {
        match event.clone().msg {
            EventMsg::AgentMessage(_) => {
                if let Some(prev) = prev_agent_message.take() {
                    session
                        .clone_session()
                        .send_event(ctx.as_ref(), prev.msg)
                        .await;
                }
                prev_agent_message = Some(event);
            }
            // Suppress ItemCompleted only for assistant messages: forwarding it
            // would trigger legacy AgentMessage via as_legacy_events(), which this
            // review flow intentionally hides in favor of structured output.
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(_),
                ..
            })
            | EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { .. })
            | EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent { .. }) => {}
            EventMsg::TurnComplete(task_complete) => {
                // Parse review output from the last agent message (if present).
                let out = task_complete
                    .last_agent_message
                    .as_deref()
                    .map(parse_review_output_event);
                return (out, true);
            }
            EventMsg::TurnAborted(_) => {
                // Cancellation or abort: consumer will finalize with None.
                return (None, false);
            }
            other => {
                session
                    .clone_session()
                    .send_event(ctx.as_ref(), other)
                    .await;
            }
        }
    }
    // Channel closed without TurnComplete: treat as interrupted.
    (None, false)
}

fn remote_review_trigger_requested(input: &[UserInput], trigger: &str) -> bool {
    let trigger = trigger.trim();
    if trigger.is_empty() {
        return false;
    }

    input.iter().any(|item| match item {
        UserInput::Text { text, .. } => text.contains(trigger),
        _ => false,
    })
}

fn attach_review_gate_failure(
    output: Option<ReviewOutputEvent>,
    gate_error: &str,
    channels: ReviewChannels,
    remote_trigger: &str,
) -> ReviewOutputEvent {
    let local_state = if channels.local_pass {
        "pass"
    } else {
        "missing"
    };
    let remote_state = if channels.remote_pass {
        "requested"
    } else {
        "missing"
    };
    let failure_note = format!(
        "Review gate failed: {gate_error}\nlocal_pass={local_state}; remote_request={remote_state} (trigger `{remote_trigger}`)."
    );

    match output {
        Some(mut existing) => {
            if existing.overall_explanation.trim().is_empty() {
                existing.overall_explanation = failure_note;
            } else {
                existing.overall_explanation = format!(
                    "{}\n\n{}",
                    existing.overall_explanation.trim_end(),
                    failure_note
                );
            }
            existing.overall_correctness = "review_gate_failed".to_string();
            existing.overall_confidence_score = existing.overall_confidence_score.min(0.2);
            existing
        }
        None => ReviewOutputEvent {
            overall_correctness: "review_gate_failed".to_string(),
            overall_explanation: failure_note,
            overall_confidence_score: 0.0,
            ..Default::default()
        },
    }
}

/// Parse a ReviewOutputEvent from a text blob returned by the reviewer model.
/// If the text is valid JSON matching ReviewOutputEvent, deserialize it.
/// Otherwise, attempt to extract the first JSON object substring and parse it.
/// If parsing still fails, return a structured fallback carrying the plain text
/// in `overall_explanation`.
fn parse_review_output_event(text: &str) -> ReviewOutputEvent {
    if let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(text) {
        return ev;
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && start < end
        && let Some(slice) = text.get(start..=end)
        && let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(slice)
    {
        return ev;
    }
    ReviewOutputEvent {
        overall_explanation: text.to_string(),
        ..Default::default()
    }
}

fn remote_review_instructions(config: &crate::config::Config) -> String {
    let trigger = config
        .review_remote
        .trigger
        .as_deref()
        .unwrap_or("@codex review");
    // Provider is currently informational only; remote execution is handled outside of `/review`.
    // Keep this message deterministic and require explicit trigger markup so gate checks
    // can fail-closed when remote review was not actually requested.
    format!(
        "Remote review mode: request an external review (e.g. on a GitHub PR), then re-run `/review` with `{trigger}` in the request body to mark the remote review request as submitted."
    )
}

/// Emits an ExitedReviewMode Event with optional ReviewOutput,
/// and records a developer message with the review output.
pub(crate) async fn exit_review_mode(
    session: Arc<Session>,
    review_output: Option<ReviewOutputEvent>,
    ctx: Arc<TurnContext>,
) {
    const REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
    const REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";
    let (user_message, assistant_message) = if let Some(out) = review_output.clone() {
        let mut findings_str = String::new();
        let text = out.overall_explanation.trim();
        if !text.is_empty() {
            findings_str.push_str(text);
        }
        if !out.findings.is_empty() {
            let block = format_review_findings_block(&out.findings, None);
            findings_str.push_str(&format!("\n{block}"));
        }
        let rendered =
            crate::client_common::REVIEW_EXIT_SUCCESS_TMPL.replace("{results}", &findings_str);
        let assistant_message = render_review_output_text(&out);
        (rendered, assistant_message)
    } else {
        let rendered = crate::client_common::REVIEW_EXIT_INTERRUPTED_TMPL.to_string();
        let assistant_message =
            "Review was interrupted. Please re-run /review and wait for it to complete."
                .to_string();
        (rendered, assistant_message)
    };

    session
        .record_conversation_items(
            &ctx,
            &[ResponseItem::Message {
                id: Some(REVIEW_USER_MESSAGE_ID.to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_message }],
                end_turn: None,
                phase: None,
            }],
        )
        .await;

    session
        .send_event(
            ctx.as_ref(),
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output }),
        )
        .await;
    session
        .record_response_item_and_emit_turn_item(
            ctx.as_ref(),
            ResponseItem::Message {
                id: Some(REVIEW_ASSISTANT_MESSAGE_ID.to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: assistant_message,
                }],
                end_turn: None,
                phase: None,
            },
        )
        .await;

    // Review turns can run before any regular user turn, so explicitly
    // materialize rollout persistence. Do this after emitting review output so
    // file creation + git metadata collection cannot delay client-facing items.
    session.ensure_rollout_materialized().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn remote_review_trigger_requested_matches_text_items_only() {
        let input = vec![
            UserInput::Mention {
                name: "codex".to_string(),
                path: "app://github".to_string(),
            },
            UserInput::Text {
                text: "Please run @codex review on PR #1".to_string(),
                text_elements: Vec::new(),
            },
        ];

        assert!(remote_review_trigger_requested(&input, "@codex review"));
        assert!(!remote_review_trigger_requested(&input, "@codex approve"));
    }

    #[test]
    fn attach_review_gate_failure_marks_output_as_failed() {
        let output = ReviewOutputEvent {
            overall_explanation: "Local review completed.".to_string(),
            overall_correctness: "ok".to_string(),
            overall_confidence_score: 0.91,
            ..Default::default()
        };

        let failed = attach_review_gate_failure(
            Some(output),
            "review.mode=remote requires remote review PASS",
            ReviewChannels {
                local_pass: false,
                remote_pass: false,
            },
            "@codex review",
        );

        assert_eq!(failed.overall_correctness, "review_gate_failed");
        assert!(
            failed
                .overall_explanation
                .contains("review.mode=remote requires remote review PASS")
        );
        assert!(
            failed
                .overall_explanation
                .contains("remote_request=missing")
        );
        assert_eq!(failed.overall_confidence_score, 0.2);
    }
}
