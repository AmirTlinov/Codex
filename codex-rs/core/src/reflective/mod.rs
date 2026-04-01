mod model;
mod prompt;

use std::sync::Arc;

use anyhow::Context;
use codex_features::Feature;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

use crate::agent::external::ClaudeCliRequest;
use crate::agent::external::default_claude_model;
use crate::agent::external::run_claude_cli;
use crate::agent::role::apply_role_to_config;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex_delegate::run_codex_thread_one_shot;
use crate::rollout::recorder::RolloutRecorder;

pub(crate) use model::ReflectiveWindowState;
use prompt::ReflectiveReport;

const CLAUDE_REFLECTIVE_LAST_N_USER_TURNS: usize = 12;
const MIN_REFLECTIVE_TOTAL_TOKENS: i64 = 3_000;
const MIN_REFLECTIVE_TOOL_CALLS: u64 = 1;

pub(crate) fn should_schedule_after_regular_turn(
    feature_enabled: bool,
    session_source: &SessionSource,
    turn_tool_calls: u64,
    turn_total_tokens: i64,
    last_agent_message: Option<&str>,
) -> bool {
    if !feature_enabled || matches!(session_source, SessionSource::SubAgent(_)) {
        return false;
    }

    if last_agent_message.is_none() {
        return false;
    }

    turn_tool_calls >= MIN_REFLECTIVE_TOOL_CALLS || turn_total_tokens >= MIN_REFLECTIVE_TOTAL_TOKENS
}

pub(crate) fn spawn_after_regular_turn(
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_tool_calls: u64,
    turn_total_tokens: i64,
    last_agent_message: Option<String>,
) {
    if !should_schedule_after_regular_turn(
        session.enabled(Feature::ReflectiveWindow),
        &turn_context.session_source,
        turn_tool_calls,
        turn_total_tokens,
        last_agent_message.as_deref(),
    ) {
        return;
    }

    tokio::spawn(async move {
        if session.has_pending_input().await
            || session.has_queued_response_items_for_next_turn().await
            || session.has_active_turn().await
        {
            return;
        }

        let history_epoch = session.history_epoch().await;
        let source_turn_id = turn_context.sub_id.clone();
        if let Err(err) =
            run_reflective_sidecar(session, turn_context, source_turn_id, history_epoch).await
        {
            debug!("reflective sidecar skipped or failed: {err:#}");
        }
    });
}

pub(crate) async fn current_prompt_item(session: &Session) -> Option<ResponseItem> {
    session
        .reflective_window()
        .await
        .map(model::ReflectiveWindowState::into_prompt_item)
}

#[cfg(test)]
pub(crate) fn test_window(source_turn_id: &str) -> ReflectiveWindowState {
    ReflectiveWindowState::from_report(
        source_turn_id.to_string(),
        prompt::ReflectiveReport {
            observations: vec![model::ReflectiveObservation {
                category: model::ReflectiveObservationCategory::Risk,
                note: "Check a subtle edge".to_string(),
                why_it_matters: "A stale reflective result could overwrite fresher truth"
                    .to_string(),
                evidence: "This window is applied asynchronously after the main turn".to_string(),
                confidence: model::ReflectiveConfidence::High,
                disposition: model::ReflectiveDisposition::Verify,
            }],
        },
    )
    .expect("test reflective window")
}

async fn run_reflective_sidecar(
    session: Arc<Session>,
    parent_turn: Arc<TurnContext>,
    source_turn_id: String,
    history_epoch: u64,
) -> anyhow::Result<()> {
    let spawn_config = build_reflective_spawn_config(parent_turn.as_ref()).await?;
    let report = if spawn_config.agent_backend.is_claude_cli() {
        run_reflective_sidecar_claude(session.as_ref(), &spawn_config).await?
    } else {
        let Some(initial_history) = load_forked_initial_history(session.as_ref()).await? else {
            return Ok(());
        };
        run_reflective_sidecar_codex(
            Arc::clone(&session),
            Arc::clone(&parent_turn),
            spawn_config,
            initial_history,
        )
        .await?
    };

    let Some(report) = report else {
        return Ok(());
    };
    let Some(window) = ReflectiveWindowState::from_report(source_turn_id.clone(), report) else {
        return Ok(());
    };

    if !session
        .can_apply_reflective_window(history_epoch, source_turn_id.as_str())
        .await
    {
        return Ok(());
    }

    session.set_reflective_window(Some(window)).await;
    Ok(())
}

async fn run_reflective_sidecar_codex(
    session: Arc<Session>,
    parent_turn: Arc<TurnContext>,
    spawn_config: crate::config::Config,
    initial_history: InitialHistory,
) -> anyhow::Result<Option<ReflectiveReport>> {
    let codex = run_codex_thread_one_shot(
        spawn_config,
        Arc::clone(&session.services.auth_manager),
        Arc::clone(&session.services.models_manager),
        vec![UserInput::Text {
            text: prompt::reflective_user_prompt(),
            text_elements: Vec::new(),
        }],
        session,
        parent_turn,
        CancellationToken::new(),
        SubAgentSource::Other("reflective_sidecar".to_string()),
        Some(prompt::reflective_output_schema()),
        Some(initial_history),
    )
    .await
    .context("spawn reflective sidecar")?;

    let final_message = loop {
        let event = codex.next_event().await.context("read reflective event")?;
        match event.msg {
            EventMsg::TurnComplete(payload) => break payload.last_agent_message,
            EventMsg::TurnAborted(_) => return Ok(None),
            EventMsg::Error(error) => {
                warn!("reflective sidecar error: {}", error.message);
            }
            _ => {}
        }
    };
    let Some(final_message) = final_message else {
        return Ok(None);
    };
    let report: ReflectiveReport =
        serde_json::from_str(&final_message).context("parse reflective sidecar JSON output")?;
    Ok(Some(report))
}

async fn run_reflective_sidecar_claude(
    session: &Session,
    spawn_config: &crate::config::Config,
) -> anyhow::Result<Option<ReflectiveReport>> {
    let history = session.clone_history().await;
    let transcript = crate::compact_transcript::render_compact_transcript(
        history.raw_items(),
        Some(CLAUDE_REFLECTIVE_LAST_N_USER_TURNS),
    );
    if transcript.trim().is_empty() {
        return Ok(None);
    }

    let mut user_sections = Vec::new();
    if let Some(developer_instructions) = spawn_config.developer_instructions.as_deref()
        && !developer_instructions.trim().is_empty()
    {
        user_sections.push(format!(
            "<codex_developer_instructions>\n{}\n</codex_developer_instructions>",
            developer_instructions.trim()
        ));
    }
    user_sections.push(prompt::reflective_user_prompt());
    user_sections.push(format!(
        "<thread_transcript>\n{transcript}\n</thread_transcript>"
    ));
    let user_prompt = user_sections.join("\n\n");
    let output = run_claude_cli(
        &spawn_config.claude_cli,
        ClaudeCliRequest {
            cwd: spawn_config.cwd.to_path_buf(),
            model: default_claude_model(spawn_config.model.as_deref()),
            system_prompt:
                "You are a reflective Claude sidecar running under Codex. Output strict JSON only."
                    .to_string(),
            user_prompt,
            json_schema: Some(prompt::reflective_output_schema()),
            tools: None,
            force_toolless: true,
            effort: spawn_config.claude_cli.effort.or(spawn_config
                .model_reasoning_effort
                .map(crate::config::ClaudeCliEffort::from)),
        },
        CancellationToken::new(),
    )
    .await
    .context("run reflective Claude CLI")?;
    let report: ReflectiveReport =
        serde_json::from_str(&output).context("parse reflective Claude JSON output")?;
    Ok(Some(report))
}

async fn build_reflective_spawn_config(
    parent_turn: &TurnContext,
) -> anyhow::Result<crate::config::Config> {
    let mut config = parent_turn.config.as_ref().clone();
    config.ephemeral = true;
    config.model = Some(parent_turn.model_info.slug.clone());
    config.model_reasoning_effort = parent_turn.reasoning_effort;
    config.model_reasoning_summary = Some(codex_protocol::config_types::ReasoningSummary::Concise);
    if let Some(reflective_agent_type) = parent_turn.config.reflective_window_agent_type.as_deref()
    {
        apply_role_to_config(&mut config, Some(reflective_agent_type))
            .await
            .map_err(anyhow::Error::msg)?;
    }
    config.developer_instructions = Some(
        config
            .developer_instructions
            .as_deref()
            .filter(|instructions| !instructions.trim().is_empty())
            .map(|instructions| format!("{instructions}\n\n{}", prompt::reflective_policy_prompt()))
            .unwrap_or_else(|| prompt::reflective_policy_prompt().to_string()),
    );
    config.permissions.approval_policy =
        crate::config::Constrained::allow_only(codex_protocol::protocol::AskForApproval::Never);
    config.permissions.sandbox_policy = crate::config::Constrained::allow_only(
        codex_protocol::protocol::SandboxPolicy::new_read_only_policy(),
    );
    config
        .features
        .disable(Feature::ShellTool)
        .context("disable shell tool")?;
    config.features.disable(Feature::ApplyPatchFreeform).ok();
    config
        .features
        .disable(Feature::ExecPermissionApprovals)
        .ok();
    config
        .features
        .disable(Feature::RequestPermissionsTool)
        .ok();
    config.features.disable(Feature::WebSearchRequest).ok();
    config.features.disable(Feature::WebSearchCached).ok();
    config.features.disable(Feature::SearchTool).ok();
    config.features.disable(Feature::ImageGeneration).ok();
    config.features.disable(Feature::JsRepl).ok();
    config.features.disable(Feature::CodeMode).ok();
    config.features.disable(Feature::CodeModeOnly).ok();
    config.features.disable(Feature::Artifact).ok();
    config.features.disable(Feature::Apps).ok();
    config.features.disable(Feature::ToolSearch).ok();
    config.features.disable(Feature::ToolSuggest).ok();
    config.features.disable(Feature::Collab).ok();
    config.features.disable(Feature::MultiAgentV2).ok();
    config.features.disable(Feature::SpawnCsv).ok();
    Ok(config)
}

async fn load_forked_initial_history(session: &Session) -> anyhow::Result<Option<InitialHistory>> {
    session.flush_rollout().await;
    let Some(rollout_path) = session.current_rollout_path().await else {
        return Ok(None);
    };
    let history = RolloutRecorder::get_rollout_history(rollout_path.as_path()).await?;
    Ok(Some(match history {
        InitialHistory::New => InitialHistory::New,
        InitialHistory::Forked(items) => InitialHistory::Forked(items),
        InitialHistory::Resumed(resumed) => InitialHistory::Forked(resumed.history),
    }))
}

#[cfg(test)]
mod tests;
