use crate::agent::AgentStatus;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::Constrained;
use crate::config_loader::ConfigLayerEntry;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::error::CodexErr;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_app_server_protocol::ConfigLayerSource;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CollabAgentIdentity;
use codex_protocol::protocol::CollabAgentInteractionBeginEvent;
use codex_protocol::protocol::CollabAgentInteractionEndEvent;
use codex_protocol::protocol::CollabAgentSpawnBeginEvent;
use codex_protocol::protocol::CollabAgentSpawnEndEvent;
use codex_protocol::protocol::CollabCloseBeginEvent;
use codex_protocol::protocol::CollabCloseEndEvent;
use codex_protocol::protocol::CollabMessageMetadata;
use codex_protocol::protocol::CollabResumeBeginEvent;
use codex_protocol::protocol::CollabResumeEndEvent;
use codex_protocol::protocol::CollabWaitingBeginEvent;
use codex_protocol::protocol::CollabWaitingEndEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs as async_fs;
use toml::Value as TomlValue;

pub struct CollabHandler;

/// Minimum wait timeout to prevent tight polling loops from burning CPU.
pub(crate) const MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
pub(crate) const MAX_WAIT_TIMEOUT_MS: i64 = 300_000;
const TEAM_CONFIG_ALLOWED_TOP_LEVEL_KEYS: &[&str] = &[
    "agents",
    "apps",
    "compact_prompt",
    "developer_instructions",
    "disable_paste_burst",
    "experimental_use_freeform_apply_patch",
    "experimental_use_unified_exec_tool",
    "features",
    "file_opener",
    "ghost_snapshot",
    "hide_agent_reasoning",
    "instructions",
    "mcp_servers",
    "memories",
    "model",
    "model_auto_compact_token_limit",
    "model_context_window",
    "model_provider",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_supports_reasoning_summaries",
    "model_verbosity",
    "notice",
    "oss_provider",
    "personality",
    "project_doc_fallback_filenames",
    "project_doc_max_bytes",
    "project_root_markers",
    "review",
    "review_model",
    "show_raw_agent_reasoning",
    "skills",
    "suppress_unstable_features_warning",
    "tool_output_token_limit",
    "tools",
    "tui",
    "web_search",
];

fn trim_token_punctuation(token: &str) -> &str {
    token.trim_matches(|ch: char| ",:;.!?".contains(ch))
}

fn directive_value(prompt: &str, keys: &[&str]) -> Option<String> {
    prompt.split_whitespace().find_map(|raw| {
        let token = trim_token_punctuation(raw);
        let inner = token.strip_prefix('[')?.strip_suffix(']')?;
        let (key, value) = inner.split_once(':')?;
        let key = key.trim().to_ascii_lowercase();
        let matched = keys.iter().any(|candidate| key == *candidate);
        if matched {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn extract_mentions(prompt: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();

    for raw in prompt.split_whitespace() {
        let token = trim_token_punctuation(raw);
        let Some(mention) = token.strip_prefix('@') else {
            continue;
        };
        if mention.is_empty() {
            continue;
        }
        let normalized = format!("@{}", mention.to_ascii_lowercase());
        if seen.insert(normalized.clone()) {
            mentions.push(normalized);
        }
    }

    mentions
}

fn extract_refs(prompt: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for raw in prompt.split_whitespace() {
        let token = trim_token_punctuation(raw);
        if !token.starts_with("CODE_REF::") {
            continue;
        }
        if seen.insert(token.to_string()) {
            refs.push(token.to_string());
        }
    }
    refs
}

fn extract_intent(prompt: &str) -> String {
    prompt
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        })
        .unwrap_or_default()
}

pub(crate) fn collab_status_label(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::PendingInit => "pending_init",
        AgentStatus::Running => "running",
        AgentStatus::Completed(_) => "completed",
        AgentStatus::Errored(_) => "errored",
        AgentStatus::Shutdown => "shutdown",
        AgentStatus::NotFound => "not_found",
    }
}

pub(crate) fn build_collab_message_metadata(
    author: &str,
    role: &str,
    status: &str,
    prompt: &str,
) -> CollabMessageMetadata {
    let priority = directive_value(prompt, &["priority"]).unwrap_or_else(|| "normal".to_string());
    CollabMessageMetadata {
        author: author.to_string(),
        role: role.to_string(),
        status: status.to_string(),
        mentions: extract_mentions(prompt),
        intent: extract_intent(prompt),
        refs: extract_refs(prompt),
        priority,
        sla: directive_value(prompt, &["sla"]),
        task_ref: directive_value(prompt, &["task", "task_ref"]),
        slice_ref: directive_value(prompt, &["slice", "slice_ref"]),
    }
}

fn validate_team_segment(value: &str, field_name: &str) -> Result<String, FunctionCallError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field_name} must contain at least one alphanumeric character."
        )));
    }

    if trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field_name} may contain only A-Za-z0-9._- and must not contain path separators."
        )));
    }

    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamAgentRef {
    team: Option<String>,
    agent: String,
}

impl TeamAgentRef {
    fn display(&self) -> String {
        if let Some(team) = self.team.as_deref() {
            format!("{team}/{}", self.agent)
        } else {
            self.agent.clone()
        }
    }
}

fn parse_team_agent_ref(value: &str, field_name: &str) -> Result<TeamAgentRef, FunctionCallError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field_name} must contain at least one alphanumeric character."
        )));
    }

    let segments = trimmed.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        [agent] => Ok(TeamAgentRef {
            team: None,
            agent: validate_team_segment(agent, field_name)?,
        }),
        [team, agent] => Ok(TeamAgentRef {
            team: Some(validate_team_segment(team, field_name)?),
            agent: validate_team_segment(agent, field_name)?,
        }),
        _ => Err(FunctionCallError::RespondToModel(format!(
            "{field_name} must be `<agent>` or `<team>/<agent>`."
        ))),
    }
}

fn legacy_team_agent_dir(cwd: &Path, team_agent: &str) -> PathBuf {
    cwd.join(".codex").join("team").join(team_agent)
}

fn legacy_team_prompt_path(cwd: &Path, team_agent: &str) -> PathBuf {
    legacy_team_agent_dir(cwd, team_agent).join("prompt")
}

fn legacy_team_config_path(cwd: &Path, team_agent: &str) -> PathBuf {
    legacy_team_agent_dir(cwd, team_agent).join("config.toml")
}

fn team_workspace_dir(cwd: &Path) -> PathBuf {
    cwd.join(".codex").join("agents")
}

fn global_team_workspace_dir(codex_home: &Path) -> PathBuf {
    codex_home.join("agents")
}

fn team_workspace_dirs_for_read(cwd: &Path, codex_home: &Path) -> Vec<PathBuf> {
    let local = team_workspace_dir(cwd);
    let global = global_team_workspace_dir(codex_home);
    if local == global {
        vec![local]
    } else {
        vec![local, global]
    }
}

fn namespaced_team_dir_in_workspace(workspace_dir: &Path, team: &str) -> PathBuf {
    workspace_dir.join(team)
}

fn namespaced_team_manifest_path_in_workspace(workspace_dir: &Path, team: &str) -> PathBuf {
    namespaced_team_dir_in_workspace(workspace_dir, team).join("team.toml")
}

fn namespaced_team_agent_dir_in_workspace(
    workspace_dir: &Path,
    team: &str,
    agent: &str,
) -> PathBuf {
    namespaced_team_dir_in_workspace(workspace_dir, team).join(agent)
}

fn namespaced_team_prompt_path_in_workspace(
    workspace_dir: &Path,
    team: &str,
    agent: &str,
) -> PathBuf {
    namespaced_team_agent_dir_in_workspace(workspace_dir, team, agent).join("system_prompt.md")
}

fn namespaced_team_prompt_fallback_path_in_workspace(
    workspace_dir: &Path,
    team: &str,
    agent: &str,
) -> PathBuf {
    namespaced_team_agent_dir_in_workspace(workspace_dir, team, agent).join("prompt")
}

fn namespaced_team_config_path_in_workspace(
    workspace_dir: &Path,
    team: &str,
    agent: &str,
) -> PathBuf {
    namespaced_team_agent_dir_in_workspace(workspace_dir, team, agent).join("config.toml")
}

fn parse_team_agent_config_toml(
    config_toml: &str,
    context: &str,
) -> Result<TomlValue, FunctionCallError> {
    let parsed = toml::from_str::<TomlValue>(config_toml).map_err(|err| {
        FunctionCallError::RespondToModel(format!("{context} is invalid TOML: {err}"))
    })?;
    validate_team_agent_config_overlay(&parsed)?;
    Ok(parsed)
}

fn validate_team_agent_config_overlay(config_overlay: &TomlValue) -> Result<(), FunctionCallError> {
    let Some(table) = config_overlay.as_table() else {
        return Err(FunctionCallError::RespondToModel(
            "team_agent config must be a TOML table at the top level.".to_string(),
        ));
    };

    let mut unsupported_keys = table
        .keys()
        .filter(|key| !TEAM_CONFIG_ALLOWED_TOP_LEVEL_KEYS.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unsupported_keys.sort();

    if !unsupported_keys.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "team_agent config contains unsupported top-level keys: {}. Allowed keys: {}.",
            unsupported_keys.join(", "),
            TEAM_CONFIG_ALLOWED_TOP_LEVEL_KEYS.join(", ")
        )));
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    id: String,
}

#[async_trait]
impl ToolHandler for CollabHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            call_id,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "collab handler received unsupported payload".to_string(),
                ));
            }
        };

        match tool_name.as_str() {
            "spawn_agent" => spawn::handle(session, turn, call_id, arguments).await,
            "send_input" => send_input::handle(session, turn, call_id, arguments).await,
            "resume_agent" => resume_agent::handle(session, turn, call_id, arguments).await,
            "wait" => wait::handle(session, turn, call_id, arguments).await,
            "close_agent" => close_agent::handle(session, turn, call_id, arguments).await,
            "team_agent_list" => team_agent_profile::list(session, turn, arguments).await,
            "team_agent_get" => team_agent_profile::get(session, turn, arguments).await,
            "team_agent_upsert" => team_agent_profile::upsert(session, turn, arguments).await,
            "team_agent_delete" => team_agent_profile::delete(session, turn, arguments).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported collab tool {other}"
            ))),
        }
    }
}

mod spawn {
    use super::*;
    use crate::agent::AgentRole;
    use crate::agent::exceeds_thread_spawn_depth_limit;
    use crate::agent::next_thread_spawn_depth;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    use tracing::warn;

    fn agent_role_label(agent_role: AgentRole) -> &'static str {
        match agent_role {
            AgentRole::Default => "default",
            AgentRole::Scout => "scout",
            AgentRole::Validator => "validator",
            AgentRole::Plan => "plan",
        }
    }

    fn parse_requested_role(value: &str) -> Option<AgentRole> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        let serialized = serde_json::to_string(trimmed).ok()?;
        serde_json::from_str::<AgentRole>(&serialized).ok()
    }

    fn normalize_handle(value: &str) -> Option<String> {
        let mut handle = String::new();
        let mut last_was_dash = false;
        let mut last_was_slash = false;
        for ch in value.trim().trim_start_matches('@').chars() {
            let normalized = if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else if ch == '/' {
                '/'
            } else if matches!(ch, '_' | '-') || ch.is_whitespace() {
                '-'
            } else {
                continue;
            };

            if normalized == '-' {
                if last_was_dash || last_was_slash || handle.is_empty() {
                    continue;
                }
                last_was_dash = true;
                last_was_slash = false;
                handle.push(normalized);
                continue;
            }

            if normalized == '/' {
                if last_was_slash || handle.is_empty() {
                    continue;
                }
                if last_was_dash {
                    let _ = handle.pop();
                }
                last_was_dash = false;
                last_was_slash = true;
                handle.push(normalized);
            } else {
                last_was_dash = false;
                last_was_slash = false;
                handle.push(normalized);
            }
        }

        while matches!(handle.chars().last(), Some('-' | '/')) {
            let _ = handle.pop();
        }
        let handle = handle.trim_start_matches('/').to_string();
        (!handle.is_empty()).then_some(handle)
    }

    fn normalize_color_token(color: Option<&str>) -> Result<Option<String>, FunctionCallError> {
        let Some(color) = color else {
            return Ok(None);
        };
        let token = color.trim().to_ascii_lowercase();
        if token.is_empty() {
            return Ok(None);
        }
        if matches!(
            token.as_str(),
            "red" | "green" | "yellow" | "blue" | "magenta" | "cyan"
        ) {
            return Ok(Some(token));
        }

        Err(FunctionCallError::RespondToModel(
            "spawn_agent color must be one of: red, green, yellow, blue, magenta, cyan."
                .to_string(),
        ))
    }

    async fn read_optional_trimmed(
        path: &Path,
        context: &str,
    ) -> Result<Option<String>, FunctionCallError> {
        match async_fs::read_to_string(path).await {
            Ok(contents) => {
                let trimmed = contents.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(FunctionCallError::RespondToModel(format!(
                "{context} failed to read from {}: {err}",
                path.display()
            ))),
        }
    }

    async fn discover_namespaced_agent_teams(
        cwd: &Path,
        codex_home: &Path,
        agent: &str,
    ) -> Result<Vec<String>, FunctionCallError> {
        let mut teams = Vec::new();
        let mut seen = HashSet::new();
        for root in team_workspace_dirs_for_read(cwd, codex_home) {
            let mut dir = match async_fs::read_dir(&root).await {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "spawn_agent failed to inspect team workspace {}: {err}",
                        root.display()
                    )));
                }
            };

            while let Some(entry) = dir.next_entry().await.map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "spawn_agent failed to read team workspace {}: {err}",
                    root.display()
                ))
            })? {
                let metadata = entry.metadata().await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "spawn_agent failed to inspect {}: {err}",
                        entry.path().display()
                    ))
                })?;
                if !metadata.is_dir() {
                    continue;
                }
                let Some(raw_team) = entry.file_name().to_str().map(ToString::to_string) else {
                    continue;
                };
                let Ok(team) = validate_team_segment(&raw_team, "spawn_agent team name") else {
                    continue;
                };

                let profile_dir = namespaced_team_agent_dir_in_workspace(&root, &team, agent);
                if !profile_dir.is_dir() {
                    continue;
                }
                let has_prompt = namespaced_team_prompt_path_in_workspace(&root, &team, agent)
                    .is_file()
                    || namespaced_team_prompt_fallback_path_in_workspace(&root, &team, agent)
                        .is_file();
                let has_config =
                    namespaced_team_config_path_in_workspace(&root, &team, agent).is_file();
                if (has_prompt || has_config) && seen.insert(team.clone()) {
                    teams.push(team);
                }
            }
        }

        teams.sort();
        Ok(teams)
    }

    async fn load_team_profile(
        cwd: &Path,
        codex_home: &Path,
        team_agent: &TeamAgentRef,
    ) -> Result<(Option<String>, Option<TomlValue>), FunctionCallError> {
        async fn load_named_profile(
            workspace_dir: &Path,
            team: &str,
            agent: &str,
        ) -> Result<Option<(Option<String>, Option<TomlValue>)>, FunctionCallError> {
            let prompt_path = namespaced_team_prompt_path_in_workspace(workspace_dir, team, agent);
            let prompt_fallback_path =
                namespaced_team_prompt_fallback_path_in_workspace(workspace_dir, team, agent);
            let config_path = namespaced_team_config_path_in_workspace(workspace_dir, team, agent);
            let team_manifest_path =
                namespaced_team_manifest_path_in_workspace(workspace_dir, team);

            let prompt = read_optional_trimmed(
                &prompt_path,
                &format!("spawn_agent team prompt for `{team}/{agent}`"),
            )
            .await?
            .or(read_optional_trimmed(
                &prompt_fallback_path,
                &format!("spawn_agent fallback team prompt for `{team}/{agent}`"),
            )
            .await?);

            if let Some(manifest_toml) = read_optional_trimmed(
                &team_manifest_path,
                &format!("spawn_agent team manifest for `{team}`"),
            )
            .await?
            {
                let _manifest: TomlValue = toml::from_str(&manifest_toml).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "spawn_agent team manifest for `{team}` at {} is invalid TOML: {err}",
                        team_manifest_path.display()
                    ))
                })?;
            }

            let config = match read_optional_trimmed(
                &config_path,
                &format!("spawn_agent team config for `{team}/{agent}`"),
            )
            .await?
            {
                Some(config_toml) => Some(parse_team_agent_config_toml(
                    &config_toml,
                    &format!(
                        "spawn_agent team config for `{team}/{agent}` at {}",
                        config_path.display()
                    ),
                )?),
                None => None,
            };

            if prompt.is_none() && config.is_none() {
                return Ok(None);
            }

            Ok(Some((prompt, config)))
        }

        if let Some(team) = team_agent.team.as_deref() {
            for workspace_dir in team_workspace_dirs_for_read(cwd, codex_home) {
                if let Some(result) =
                    load_named_profile(&workspace_dir, team, &team_agent.agent).await?
                {
                    return Ok(result);
                }
            }
            return Err(FunctionCallError::RespondToModel(format!(
                "spawn_agent team profile `{team}/{}` was not found under {} or {}",
                team_agent.agent,
                namespaced_team_agent_dir_in_workspace(
                    &team_workspace_dir(cwd),
                    team,
                    &team_agent.agent,
                )
                .display(),
                namespaced_team_agent_dir_in_workspace(
                    &global_team_workspace_dir(codex_home),
                    team,
                    &team_agent.agent,
                )
                .display()
            )));
        }

        let legacy_prompt_path = legacy_team_prompt_path(cwd, &team_agent.agent);
        let legacy_config_path = legacy_team_config_path(cwd, &team_agent.agent);
        let prompt = read_optional_trimmed(
            &legacy_prompt_path,
            &format!("spawn_agent team prompt for `{}`", team_agent.agent),
        )
        .await?;
        let config = match read_optional_trimmed(
            &legacy_config_path,
            &format!("spawn_agent team config for `{}`", team_agent.agent),
        )
        .await?
        {
            Some(config_toml) => Some(parse_team_agent_config_toml(
                &config_toml,
                &format!(
                    "spawn_agent team config for `{}` at {}",
                    team_agent.agent,
                    legacy_config_path.display()
                ),
            )?),
            None => None,
        };
        if prompt.is_some() || config.is_some() {
            return Ok((prompt, config));
        }

        let matching_teams =
            discover_namespaced_agent_teams(cwd, codex_home, &team_agent.agent).await?;
        if let [team] = matching_teams.as_slice() {
            for workspace_dir in team_workspace_dirs_for_read(cwd, codex_home) {
                if let Some(result) =
                    load_named_profile(&workspace_dir, team, &team_agent.agent).await?
                {
                    return Ok(result);
                }
            }
        }
        if !matching_teams.is_empty() {
            return Err(FunctionCallError::RespondToModel(format!(
                "spawn_agent team profile `{}` is ambiguous across teams: {}. Use `<team>/<agent>`.",
                team_agent.agent,
                matching_teams.join(", ")
            )));
        }

        Err(FunctionCallError::RespondToModel(format!(
            "spawn_agent team profile `{}` was not found under {}, {} or {}",
            team_agent.agent,
            legacy_team_agent_dir(cwd, &team_agent.agent).display(),
            team_workspace_dir(cwd).display(),
            global_team_workspace_dir(codex_home).display()
        )))
    }

    fn merge_team_prompt(
        base_instructions: Option<String>,
        team_prompt: Option<&str>,
    ) -> Option<String> {
        let Some(team_prompt) = team_prompt
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        else {
            return base_instructions;
        };

        match base_instructions {
            None => Some(team_prompt.to_string()),
            Some(mut instructions) => {
                let base = instructions.trim();
                if base.is_empty() {
                    return Some(team_prompt.to_string());
                }
                instructions.push_str("\n\n");
                instructions.push_str(team_prompt);
                Some(instructions)
            }
        }
    }

    fn stable_prompt_version(prompt: &str) -> u32 {
        let mut hash = 2_166_136_261_u32;
        for byte in prompt.bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(16_777_619);
        }
        hash.max(1)
    }

    fn resolve_prompt_profile(config: &Config, role: AgentRole) -> (String, u32) {
        let role_label = agent_role_label(role);
        let prompt_source = config.base_instructions.as_deref().unwrap_or(role_label);
        let prompt_version = stable_prompt_version(prompt_source);
        (
            format!("{role_label}-v{prompt_version:08x}"),
            prompt_version,
        )
    }

    fn compose_agent_label(
        handle: &str,
        display_name: Option<&str>,
        color: Option<&str>,
        role: AgentRole,
        prompt_profile: Option<&str>,
    ) -> String {
        CollabAgentIdentity {
            handle: handle.to_string(),
            display_name: display_name.map(ToString::to_string),
            color_token: color.map(ToString::to_string),
            role_label: Some(agent_role_label(role).to_string()),
            prompt_profile: prompt_profile.map(ToString::to_string),
        }
        .to_agent_type_label()
    }

    fn resolve_spawn_role_and_handle(
        args: &SpawnAgentArgs,
        team_profile: Option<&TeamAgentRef>,
    ) -> Result<(AgentRole, String), FunctionCallError> {
        let role_from_agent_type = match args.agent_type.as_deref() {
            Some(agent_type) => {
                let parsed = parse_requested_role(agent_type).ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "spawn_agent agent_type must be one of: default, scout, validator, plan. Use `handle` for custom labels."
                            .to_string(),
                    )
                })?;
                Some(parsed)
            }
            None => None,
        };

        let agent_role = args.role.or(role_from_agent_type).unwrap_or_else(|| {
            if args.handle.is_some() {
                AgentRole::Default
            } else {
                AgentRole::Scout
            }
        });

        let team_handle = team_profile
            .filter(|profile| profile.team.is_some())
            .map(TeamAgentRef::display);
        let raw_handle = args
            .handle
            .as_deref()
            .map(ToString::to_string)
            .or(team_handle)
            .unwrap_or_else(|| agent_role_label(agent_role).to_string());

        let handle = normalize_handle(raw_handle.as_str()).ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "spawn_agent handle must contain at least one alphanumeric character.".to_string(),
            )
        })?;

        Ok((agent_role, handle))
    }

    fn resolve_team_profile_selection(
        args: &SpawnAgentArgs,
    ) -> Result<Option<TeamAgentRef>, FunctionCallError> {
        args.team_agent
            .as_deref()
            .map(|team_agent| parse_team_agent_ref(team_agent, "spawn_agent team_agent"))
            .transpose()
    }

    #[derive(Debug, Deserialize)]
    struct SpawnAgentArgs {
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        /// Optional role shorthand label.
        agent_type: Option<String>,
        /// Explicit runtime role override.
        role: Option<AgentRole>,
        /// Team Mesh handle shown in transcript (without the leading @).
        handle: Option<String>,
        /// Optional display name shown in transcript beside handle.
        display_name: Option<String>,
        /// Optional Team Mesh color token.
        color: Option<String>,
        /// Optional team agent profile name under `.codex/team/<team_agent>/` or `.codex/agents/<team>/<agent>/`.
        team_agent: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct SpawnAgentResult {
        agent_id: String,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SpawnAgentArgs = parse_arguments(&arguments)?;
        let team_profile = resolve_team_profile_selection(&args)?;
        let (agent_role, handle) = resolve_spawn_role_and_handle(&args, team_profile.as_ref())?;
        let display_name = args
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let color_token = normalize_color_token(args.color.as_deref())?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let (team_prompt, team_config) = match team_profile.as_ref() {
            Some(team_agent) => {
                load_team_profile(&turn.cwd, turn.config.codex_home.as_path(), team_agent).await?
            }
            None => (None, None),
        };

        if turn.tools_config.agent_role == AgentRole::Plan && agent_role != AgentRole::Scout {
            return Err(FunctionCallError::RespondToModel(
                "Plan agents can only spawn scout agents. Set `role` (or `agent_type`) to `scout`."
                    .to_string(),
            ));
        }

        let is_subagent = matches!(turn.session_source, SessionSource::SubAgent(_));
        if is_subagent && agent_role != AgentRole::Scout {
            return Err(FunctionCallError::RespondToModel(
                "Subagents can only spawn scout agents. Set `role` (or `agent_type`) to `scout`."
                    .to_string(),
            ));
        }

        if turn.tools_config.agent_role == AgentRole::Default
            && !matches!(
                agent_role,
                AgentRole::Default | AgentRole::Scout | AgentRole::Validator | AgentRole::Plan
            )
        {
            return Err(FunctionCallError::RespondToModel(
                "Main role can only spawn default, scout, validator, or plan agents.".to_string(),
            ));
        }

        let prompt = input_preview(&input_items);
        let session_source = turn.session_source.clone();
        let child_depth = next_thread_spawn_depth(&session_source);
        if exceeds_thread_spawn_depth_limit(child_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }

        let mut config = build_agent_spawn_config(
            &session.get_base_instructions().await,
            turn.as_ref(),
            child_depth,
            team_config.as_ref(),
        )?;
        if let Some(team_prompt) = team_prompt.as_deref() {
            config.developer_instructions =
                merge_team_prompt(config.developer_instructions, Some(team_prompt));
        }
        agent_role
            .apply_to_config(&mut config)
            .map_err(FunctionCallError::RespondToModel)?;
        let (prompt_profile, prompt_version) = resolve_prompt_profile(&config, agent_role);
        let agent_type = Some(compose_agent_label(
            &handle,
            display_name,
            color_token.as_deref(),
            agent_role,
            Some(prompt_profile.as_str()),
        ));

        session
            .send_event(
                &turn,
                CollabAgentSpawnBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    agent_type: agent_type.clone(),
                    prompt: prompt.clone(),
                }
                .into(),
            )
            .await;

        let result = session
            .services
            .agent_control
            .spawn_agent(
                config,
                input_items,
                Some(thread_spawn_source(session.conversation_id, child_depth)),
            )
            .await
            .map_err(collab_spawn_error);

        let (new_thread_id, status) = match &result {
            Ok(thread_id) => (
                Some(*thread_id),
                session.services.agent_control.get_status(*thread_id).await,
            ),
            Err(_) => (None, AgentStatus::NotFound),
        };

        session
            .send_event(
                &turn,
                CollabAgentSpawnEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    new_thread_id,
                    agent_type,
                    prompt,
                    status,
                }
                .into(),
            )
            .await;

        let new_thread_id = result?;
        {
            let mut lifecycle = session.team_lifecycle_store.lock().await;
            let role_label = agent_role_label(agent_role).to_string();
            if let Err(err) = lifecycle.upsert_role_prompt(
                role_label.clone(),
                prompt_version,
                prompt_profile.clone(),
            ) {
                warn!("failed to register role prompt version for {role_label}: {err}");
            }
            let mut profile_roles = lifecycle
                .profile("active-session-team")
                .map(|profile| profile.role_versions.clone())
                .unwrap_or_else(HashMap::new);
            profile_roles.insert(role_label, prompt_version);
            if let Err(err) = lifecycle.create_or_update_profile(
                "active-session-team".to_string(),
                profile_roles,
                Instant::now(),
            ) {
                warn!("failed to update active team profile: {err}");
            }
        }
        if agent_role == AgentRole::Scout {
            turn.context_validated.store(false, Ordering::Release);
            turn.scout_context_ready.store(true, Ordering::Release);
        } else if turn.tools_config.agent_role != AgentRole::Scout
            && turn.scout_context_ready.load(Ordering::Acquire)
        {
            turn.context_validated.store(true, Ordering::Release);
        }

        let content = serde_json::to_string(&SpawnAgentResult {
            agent_id: new_thread_id.to_string(),
        })
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize spawn_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

mod team_agent_profile {
    use super::*;
    use std::io::ErrorKind;
    use std::sync::Arc;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;
    use tokio::io::AsyncWriteExt;

    #[derive(Debug, Deserialize)]
    struct TeamAgentListArgs {}

    #[derive(Debug, Deserialize)]
    struct TeamAgentGetArgs {
        team_agent: String,
    }

    #[derive(Debug, Deserialize)]
    struct TeamAgentUpsertArgs {
        team_agent: String,
        prompt: Option<String>,
        config_toml: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct TeamAgentDeleteArgs {
        team_agent: String,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TeamAgentEntry {
        name: String,
        has_prompt: bool,
        has_config: bool,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TeamAgentListResult {
        agents: Vec<TeamAgentEntry>,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TeamAgentGetResult {
        team_agent: String,
        prompt: Option<String>,
        config_toml: Option<String>,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TeamAgentUpsertResult {
        team_agent: String,
        prompt_written: bool,
        prompt_deleted: bool,
        config_written: bool,
        config_deleted: bool,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    struct TeamAgentDeleteResult {
        team_agent: String,
        deleted: bool,
    }

    fn to_tool_output<T: Serialize>(
        result: &T,
        operation: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let content = serde_json::to_string(result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize {operation} result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }

    async fn read_optional_trimmed_file(path: &Path) -> Result<Option<String>, FunctionCallError> {
        match async_fs::read_to_string(path).await {
            Ok(contents) => {
                let trimmed = contents.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(FunctionCallError::RespondToModel(format!(
                "failed to read {}: {err}",
                path.display()
            ))),
        }
    }

    async fn remove_file_if_exists(path: &Path) -> Result<bool, FunctionCallError> {
        match async_fs::remove_file(path).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => Err(FunctionCallError::RespondToModel(format!(
                "failed to delete {}: {err}",
                path.display()
            ))),
        }
    }

    async fn atomic_write_file(path: &Path, contents: &str) -> Result<(), FunctionCallError> {
        let parent = path.parent().ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "failed to determine parent directory for {}",
                path.display()
            ))
        })?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("team-profile");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let pid = std::process::id();

        for attempt in 0..16 {
            let temp_path = parent.join(format!(".{file_name}.tmp.{pid}.{timestamp}.{attempt}"));
            let mut temp_file = match async_fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await
            {
                Ok(file) => file,
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "failed to create temporary profile file {}: {err}",
                        temp_path.display()
                    )));
                }
            };

            if let Err(err) = temp_file.write_all(contents.as_bytes()).await {
                let _ = async_fs::remove_file(&temp_path).await;
                return Err(FunctionCallError::RespondToModel(format!(
                    "failed to write temporary profile file {}: {err}",
                    temp_path.display()
                )));
            }
            if let Err(err) = temp_file.sync_all().await {
                let _ = async_fs::remove_file(&temp_path).await;
                return Err(FunctionCallError::RespondToModel(format!(
                    "failed to sync temporary profile file {}: {err}",
                    temp_path.display()
                )));
            }
            drop(temp_file);

            match async_fs::rename(&temp_path, path).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    #[cfg(windows)]
                    if matches!(
                        err.kind(),
                        ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
                    ) {
                        match async_fs::remove_file(path).await {
                            Ok(()) => {}
                            Err(remove_err) if remove_err.kind() == ErrorKind::NotFound => {}
                            Err(remove_err) => {
                                let _ = async_fs::remove_file(&temp_path).await;
                                return Err(FunctionCallError::RespondToModel(format!(
                                    "failed to remove existing profile file {} before replace: {remove_err}",
                                    path.display()
                                )));
                            }
                        }

                        match async_fs::rename(&temp_path, path).await {
                            Ok(()) => return Ok(()),
                            Err(rename_err) => {
                                let _ = async_fs::remove_file(&temp_path).await;
                                return Err(FunctionCallError::RespondToModel(format!(
                                    "failed to replace {} with {} after removing existing target: {rename_err}",
                                    path.display(),
                                    temp_path.display()
                                )));
                            }
                        }
                    }

                    let _ = async_fs::remove_file(&temp_path).await;
                    return Err(FunctionCallError::RespondToModel(format!(
                        "failed to atomically replace {} with {}: {err}",
                        path.display(),
                        temp_path.display()
                    )));
                }
            }
        }

        Err(FunctionCallError::RespondToModel(format!(
            "failed to allocate a unique temporary profile path for {}",
            path.display()
        )))
    }

    struct TeamAgentPaths {
        dir: PathBuf,
        prompt_path: PathBuf,
        prompt_fallback_path: Option<PathBuf>,
        config_path: PathBuf,
    }

    fn namespaced_team_agent_paths_for_workspace(
        workspace_dir: &Path,
        team: &str,
        agent: &str,
    ) -> TeamAgentPaths {
        TeamAgentPaths {
            dir: namespaced_team_agent_dir_in_workspace(workspace_dir, team, agent),
            prompt_path: namespaced_team_prompt_path_in_workspace(workspace_dir, team, agent),
            prompt_fallback_path: Some(namespaced_team_prompt_fallback_path_in_workspace(
                workspace_dir,
                team,
                agent,
            )),
            config_path: namespaced_team_config_path_in_workspace(workspace_dir, team, agent),
        }
    }

    fn team_agent_paths(cwd: &Path, team_agent: &TeamAgentRef) -> TeamAgentPaths {
        if let Some(team) = team_agent.team.as_deref() {
            namespaced_team_agent_paths_for_workspace(
                &team_workspace_dir(cwd),
                team,
                &team_agent.agent,
            )
        } else {
            TeamAgentPaths {
                dir: legacy_team_agent_dir(cwd, &team_agent.agent),
                prompt_path: legacy_team_prompt_path(cwd, &team_agent.agent),
                prompt_fallback_path: None,
                config_path: legacy_team_config_path(cwd, &team_agent.agent),
            }
        }
    }

    async fn read_team_agent_prompt(
        paths: &TeamAgentPaths,
    ) -> Result<Option<String>, FunctionCallError> {
        let prompt = read_optional_trimmed_file(&paths.prompt_path).await?;
        if prompt.is_some() {
            return Ok(prompt);
        }
        let Some(fallback_path) = paths.prompt_fallback_path.as_ref() else {
            return Ok(None);
        };
        read_optional_trimmed_file(fallback_path).await
    }

    async fn list_legacy_profiles(cwd: &Path) -> Result<Vec<TeamAgentEntry>, FunctionCallError> {
        let team_dir = cwd.join(".codex").join("team");
        let mut agents = Vec::new();
        match async_fs::read_dir(&team_dir).await {
            Ok(mut dir) => {
                while let Some(entry) = dir.next_entry().await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to read team directory {}: {err}",
                        team_dir.display()
                    ))
                })? {
                    let metadata = entry.metadata().await.map_err(|err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to inspect {}: {err}",
                            entry.path().display()
                        ))
                    })?;
                    if !metadata.is_dir() {
                        continue;
                    }
                    let Some(name) = entry.file_name().to_str().map(ToString::to_string) else {
                        continue;
                    };
                    if validate_team_segment(&name, "team_agent directory name").is_err() {
                        continue;
                    }
                    agents.push(TeamAgentEntry {
                        name,
                        has_prompt: entry.path().join("prompt").is_file(),
                        has_config: entry.path().join("config.toml").is_file(),
                    });
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "failed to list team profiles in {}: {err}",
                    team_dir.display()
                )));
            }
        }
        Ok(agents)
    }

    async fn list_namespaced_profiles(
        cwd: &Path,
        codex_home: &Path,
    ) -> Result<Vec<TeamAgentEntry>, FunctionCallError> {
        let mut agents = Vec::new();
        let mut seen = HashSet::new();

        for workspace_dir in team_workspace_dirs_for_read(cwd, codex_home) {
            let mut teams = match async_fs::read_dir(&workspace_dir).await {
                Ok(entries) => entries,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "failed to read team workspace {}: {err}",
                        workspace_dir.display()
                    )));
                }
            };

            while let Some(team_entry) = teams.next_entry().await.map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to inspect team workspace {}: {err}",
                    workspace_dir.display()
                ))
            })? {
                let metadata = team_entry.metadata().await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to inspect {}: {err}",
                        team_entry.path().display()
                    ))
                })?;
                if !metadata.is_dir() {
                    continue;
                }
                let Some(raw_team) = team_entry.file_name().to_str().map(ToString::to_string)
                else {
                    continue;
                };
                let Ok(team) = validate_team_segment(&raw_team, "team directory name") else {
                    continue;
                };

                let mut team_profiles =
                    async_fs::read_dir(team_entry.path()).await.map_err(|err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to read team directory {}: {err}",
                            team_entry.path().display()
                        ))
                    })?;
                while let Some(agent_entry) = team_profiles.next_entry().await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to read team profile entries in {}: {err}",
                        team_entry.path().display()
                    ))
                })? {
                    let agent_meta = agent_entry.metadata().await.map_err(|err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to inspect {}: {err}",
                            agent_entry.path().display()
                        ))
                    })?;
                    if !agent_meta.is_dir() {
                        continue;
                    }
                    let Some(raw_agent) = agent_entry.file_name().to_str().map(ToString::to_string)
                    else {
                        continue;
                    };
                    let Ok(agent) = validate_team_segment(&raw_agent, "team agent directory name")
                    else {
                        continue;
                    };
                    let name = format!("{team}/{agent}");
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let has_prompt = agent_entry.path().join("system_prompt.md").is_file()
                        || agent_entry.path().join("prompt").is_file();
                    let has_config = agent_entry.path().join("config.toml").is_file();
                    agents.push(TeamAgentEntry {
                        name,
                        has_prompt,
                        has_config,
                    });
                }
            }
        }
        Ok(agents)
    }

    pub async fn list(
        _session: Arc<Session>,
        turn: Arc<TurnContext>,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let _: TeamAgentListArgs = parse_arguments(&arguments)?;
        let mut agents = list_legacy_profiles(&turn.cwd).await?;
        agents.extend(list_namespaced_profiles(&turn.cwd, turn.config.codex_home.as_path()).await?);

        agents.sort_by(|left, right| left.name.cmp(&right.name));
        to_tool_output(&TeamAgentListResult { agents }, "team_agent_list")
    }

    pub async fn get(
        _session: Arc<Session>,
        turn: Arc<TurnContext>,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: TeamAgentGetArgs = parse_arguments(&arguments)?;
        let team_agent_ref = parse_team_agent_ref(&args.team_agent, "team_agent_get team_agent")?;
        let team_agent = team_agent_ref.display();
        let mut paths_candidates = vec![team_agent_paths(&turn.cwd, &team_agent_ref)];
        if let Some(team) = team_agent_ref.team.as_deref() {
            let global_paths = namespaced_team_agent_paths_for_workspace(
                &global_team_workspace_dir(turn.config.codex_home.as_path()),
                team,
                &team_agent_ref.agent,
            );
            if global_paths.dir != paths_candidates[0].dir {
                paths_candidates.push(global_paths);
            }
        } else {
            let mut namespaced_matches =
                list_namespaced_profiles(&turn.cwd, turn.config.codex_home.as_path())
                    .await?
                    .into_iter()
                    .filter_map(|entry| {
                        entry.name.split_once('/').and_then(|(team, agent)| {
                            (agent == team_agent_ref.agent).then_some(team.to_string())
                        })
                    })
                    .collect::<Vec<_>>();
            namespaced_matches.sort();
            namespaced_matches.dedup();

            if namespaced_matches.len() > 1 {
                return Err(FunctionCallError::RespondToModel(format!(
                    "team_agent profile `{}` is ambiguous across teams: {}. Use `<team>/<agent>`.",
                    team_agent_ref.agent,
                    namespaced_matches.join(", ")
                )));
            }

            if let [team] = namespaced_matches.as_slice() {
                let local_paths = namespaced_team_agent_paths_for_workspace(
                    &team_workspace_dir(&turn.cwd),
                    team,
                    &team_agent_ref.agent,
                );
                if local_paths.dir != paths_candidates[0].dir {
                    paths_candidates.push(local_paths);
                }

                let global_paths = namespaced_team_agent_paths_for_workspace(
                    &global_team_workspace_dir(turn.config.codex_home.as_path()),
                    team,
                    &team_agent_ref.agent,
                );
                if !paths_candidates
                    .iter()
                    .any(|paths| paths.dir == global_paths.dir)
                {
                    paths_candidates.push(global_paths);
                }
            }
        }

        let mut checked_dirs = Vec::new();
        for paths in paths_candidates {
            checked_dirs.push(paths.dir.display().to_string());
            let prompt = read_team_agent_prompt(&paths).await?;
            let config_toml = read_optional_trimmed_file(&paths.config_path).await?;
            if prompt.is_none() && config_toml.is_none() {
                continue;
            }

            return to_tool_output(
                &TeamAgentGetResult {
                    team_agent,
                    prompt,
                    config_toml,
                },
                "team_agent_get",
            );
        }

        Err(FunctionCallError::RespondToModel(format!(
            "team_agent profile `{team_agent}` was not found under {}",
            checked_dirs.join(" or ")
        )))
    }

    pub async fn upsert(
        _session: Arc<Session>,
        turn: Arc<TurnContext>,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: TeamAgentUpsertArgs = parse_arguments(&arguments)?;
        let team_agent_ref =
            parse_team_agent_ref(&args.team_agent, "team_agent_upsert team_agent")?;
        let team_agent = team_agent_ref.display();

        if args.prompt.is_none() && args.config_toml.is_none() {
            return Err(FunctionCallError::RespondToModel(
                "team_agent_upsert requires at least one field: `prompt` or `config_toml`."
                    .to_string(),
            ));
        }

        let paths = team_agent_paths(&turn.cwd, &team_agent_ref);
        let target_dir = paths.dir.clone();
        async_fs::create_dir_all(&target_dir).await.map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to create team profile directory {}: {err}",
                target_dir.display()
            ))
        })?;

        let mut prompt_written = false;
        let mut prompt_deleted = false;
        let mut config_written = false;
        let mut config_deleted = false;

        if let Some(prompt) = args.prompt.as_deref() {
            if prompt.trim().is_empty() {
                let deleted_primary = remove_file_if_exists(&paths.prompt_path).await?;
                let deleted_fallback =
                    if let Some(fallback_path) = paths.prompt_fallback_path.as_ref() {
                        remove_file_if_exists(fallback_path).await?
                    } else {
                        false
                    };
                prompt_deleted = deleted_primary || deleted_fallback;
            } else {
                atomic_write_file(&paths.prompt_path, prompt).await?;
                if let Some(fallback_path) = paths.prompt_fallback_path.as_ref() {
                    let _ = remove_file_if_exists(fallback_path).await?;
                }
                prompt_written = true;
            }
        }

        if let Some(config_toml) = args.config_toml.as_deref() {
            if config_toml.trim().is_empty() {
                config_deleted = remove_file_if_exists(&paths.config_path).await?;
            } else {
                let parsed = parse_team_agent_config_toml(
                    config_toml,
                    &format!("team_agent_upsert config for `{team_agent}`"),
                )?;
                let _ = apply_team_config(turn.config.as_ref(), &parsed)?;
                atomic_write_file(&paths.config_path, config_toml).await?;
                config_written = true;
            }
        }

        let has_prompt = paths.prompt_path.is_file()
            || paths
                .prompt_fallback_path
                .as_ref()
                .is_some_and(|fallback| fallback.is_file());
        if !has_prompt && !paths.config_path.is_file() {
            let _ = async_fs::remove_dir(&target_dir).await;
        }

        to_tool_output(
            &TeamAgentUpsertResult {
                team_agent,
                prompt_written,
                prompt_deleted,
                config_written,
                config_deleted,
            },
            "team_agent_upsert",
        )
    }

    pub async fn delete(
        _session: Arc<Session>,
        turn: Arc<TurnContext>,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: TeamAgentDeleteArgs = parse_arguments(&arguments)?;
        let team_agent_ref =
            parse_team_agent_ref(&args.team_agent, "team_agent_delete team_agent")?;
        let team_agent = team_agent_ref.display();
        let target_dir = team_agent_paths(&turn.cwd, &team_agent_ref).dir;

        let deleted = match async_fs::remove_dir_all(&target_dir).await {
            Ok(()) => true,
            Err(err) if err.kind() == ErrorKind::NotFound => false,
            Err(err) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "failed to delete team profile {}: {err}",
                    target_dir.display()
                )));
            }
        };

        to_tool_output(
            &TeamAgentDeleteResult {
                team_agent,
                deleted,
            },
            "team_agent_delete",
        )
    }
}

mod send_input {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct SendInputArgs {
        id: String,
        message: Option<String>,
        items: Option<Vec<UserInput>>,
        #[serde(default)]
        interrupt: bool,
    }

    #[derive(Debug, Serialize)]
    struct SendInputResult {
        submission_id: String,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SendInputArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let input_items = parse_collab_input(args.message, args.items)?;
        let prompt = input_preview(&input_items);
        let sender_role = match turn.tools_config.agent_role {
            crate::agent::AgentRole::Default => "default",
            crate::agent::AgentRole::Scout => "scout",
            crate::agent::AgentRole::Validator => "validator",
            crate::agent::AgentRole::Plan => "plan",
        };
        let sender_handle = session.conversation_id.to_string();
        if args.interrupt {
            session
                .services
                .agent_control
                .interrupt_agent(receiver_thread_id)
                .await
                .map_err(|err| collab_agent_error(receiver_thread_id, err))?;
        }
        session
            .send_event(
                &turn,
                CollabAgentInteractionBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    prompt: prompt.clone(),
                    message: build_collab_message_metadata(
                        &sender_handle,
                        sender_role,
                        "running",
                        &prompt,
                    ),
                }
                .into(),
            )
            .await;
        let result = session
            .services
            .agent_control
            .send_input(receiver_thread_id, input_items)
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err));
        let status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        let message_metadata = build_collab_message_metadata(
            &sender_handle,
            sender_role,
            collab_status_label(&status),
            &prompt,
        );
        session
            .send_event(
                &turn,
                CollabAgentInteractionEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    prompt,
                    message: message_metadata,
                    status,
                }
                .into(),
            )
            .await;
        let submission_id = result?;

        let content = serde_json::to_string(&SendInputResult { submission_id }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize send_input result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

mod resume_agent {
    use super::*;
    use crate::agent::next_thread_spawn_depth;
    use crate::rollout::find_thread_path_by_id_str;
    use std::sync::Arc;

    #[derive(Debug, Deserialize)]
    struct ResumeAgentArgs {
        id: String,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub(super) struct ResumeAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: ResumeAgentArgs = parse_arguments(&arguments)?;
        let receiver_thread_id = agent_id(&args.id)?;
        let child_depth = next_thread_spawn_depth(&turn.session_source);
        if exceeds_thread_spawn_depth_limit(child_depth) {
            return Err(FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string(),
            ));
        }

        session
            .send_event(
                &turn,
                CollabResumeBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                }
                .into(),
            )
            .await;

        let mut status = session
            .services
            .agent_control
            .get_status(receiver_thread_id)
            .await;
        let error = if matches!(status, AgentStatus::NotFound) {
            // If the thread is no longer active, attempt to restore it from rollout.
            match try_resume_closed_agent(
                &session,
                &turn,
                receiver_thread_id,
                &args.id,
                child_depth,
            )
            .await
            {
                Ok(resumed_status) => {
                    status = resumed_status;
                    None
                }
                Err(err) => {
                    status = session
                        .services
                        .agent_control
                        .get_status(receiver_thread_id)
                        .await;
                    Some(err)
                }
            }
        } else {
            None
        };

        session
            .send_event(
                &turn,
                CollabResumeEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id,
                    status: status.clone(),
                }
                .into(),
            )
            .await;

        if let Some(err) = error {
            return Err(err);
        }

        let content = serde_json::to_string(&ResumeAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize resume_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }

    async fn try_resume_closed_agent(
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        receiver_thread_id: ThreadId,
        receiver_id: &str,
        child_depth: i32,
    ) -> Result<AgentStatus, FunctionCallError> {
        let rollout_path = find_thread_path_by_id_str(
            turn.config.codex_home.as_path(),
            receiver_id,
        )
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "tool failed: failed to locate rollout for agent {receiver_thread_id}: {err}"
            ))
        })?
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "agent with id {receiver_thread_id} not found"
            ))
        })?;

        let config = build_agent_resume_config(turn.as_ref(), child_depth)?;
        let resumed_thread_id = session
            .services
            .agent_control
            .resume_agent_from_rollout(
                config,
                rollout_path,
                thread_spawn_source(session.conversation_id, child_depth),
            )
            .await
            .map_err(|err| collab_agent_error(receiver_thread_id, err))?;

        Ok(session
            .services
            .agent_control
            .get_status(resumed_thread_id)
            .await)
    }
}

mod wait {
    use super::*;
    use crate::agent::status::is_final;
    use futures::FutureExt;
    use futures::StreamExt;
    use futures::stream::FuturesUnordered;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch::Receiver;
    use tokio::time::Instant;

    use tokio::time::timeout_at;

    #[derive(Debug, Deserialize)]
    struct WaitArgs {
        ids: Vec<String>,
        timeout_ms: Option<i64>,
    }

    #[derive(Debug, Serialize)]
    struct WaitResult {
        status: HashMap<ThreadId, AgentStatus>,
        timed_out: bool,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: WaitArgs = parse_arguments(&arguments)?;
        if args.ids.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "ids must be non-empty".to_owned(),
            ));
        }
        let receiver_thread_ids = args
            .ids
            .iter()
            .map(|id| agent_id(id))
            .collect::<Result<Vec<_>, _>>()?;

        // Validate timeout.
        // Very short timeouts encourage busy-polling loops in the orchestrator prompt and can
        // cause high CPU usage even with a single active worker, so clamp to a minimum.
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let timeout_ms = match timeout_ms {
            ms if ms <= 0 => {
                return Err(FunctionCallError::RespondToModel(
                    "timeout_ms must be greater than zero".to_owned(),
                ));
            }
            ms => ms.clamp(MIN_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS),
        };

        session
            .send_event(
                &turn,
                CollabWaitingBeginEvent {
                    sender_thread_id: session.conversation_id,
                    receiver_thread_ids: receiver_thread_ids.clone(),
                    call_id: call_id.clone(),
                }
                .into(),
            )
            .await;

        let mut status_rxs = Vec::with_capacity(receiver_thread_ids.len());
        let mut initial_final_statuses = Vec::new();
        for id in &receiver_thread_ids {
            match session.services.agent_control.subscribe_status(*id).await {
                Ok(rx) => {
                    let status = rx.borrow().clone();
                    if is_final(&status) {
                        initial_final_statuses.push((*id, status));
                    }
                    status_rxs.push((*id, rx));
                }
                Err(CodexErr::ThreadNotFound(_)) => {
                    initial_final_statuses.push((*id, AgentStatus::NotFound));
                }
                Err(err) => {
                    let mut statuses = HashMap::with_capacity(1);
                    statuses.insert(*id, session.services.agent_control.get_status(*id).await);
                    session
                        .send_event(
                            &turn,
                            CollabWaitingEndEvent {
                                sender_thread_id: session.conversation_id,
                                call_id: call_id.clone(),
                                statuses,
                            }
                            .into(),
                        )
                        .await;
                    return Err(collab_agent_error(*id, err));
                }
            }
        }

        let statuses = if !initial_final_statuses.is_empty() {
            initial_final_statuses
        } else {
            // Wait for the first agent to reach a final status.
            let mut futures = FuturesUnordered::new();
            for (id, rx) in status_rxs.into_iter() {
                let session = session.clone();
                futures.push(wait_for_final_status(session, id, rx));
            }
            let mut results = Vec::new();
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            loop {
                match timeout_at(deadline, futures.next()).await {
                    Ok(Some(Some(result))) => {
                        results.push(result);
                        break;
                    }
                    Ok(Some(None)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
            if !results.is_empty() {
                // Drain the unlikely last elements to prevent race.
                loop {
                    match futures.next().now_or_never() {
                        Some(Some(Some(result))) => results.push(result),
                        Some(Some(None)) => continue,
                        Some(None) | None => break,
                    }
                }
            }
            results
        };

        // Convert payload.
        let statuses_map = statuses.clone().into_iter().collect::<HashMap<_, _>>();
        let result = WaitResult {
            status: statuses_map.clone(),
            timed_out: statuses.is_empty(),
        };

        // Final event emission.
        session
            .send_event(
                &turn,
                CollabWaitingEndEvent {
                    sender_thread_id: session.conversation_id,
                    call_id,
                    statuses: statuses_map,
                }
                .into(),
            )
            .await;

        let content = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize wait result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: None,
        })
    }

    async fn wait_for_final_status(
        session: Arc<Session>,
        thread_id: ThreadId,
        mut status_rx: Receiver<AgentStatus>,
    ) -> Option<(ThreadId, AgentStatus)> {
        let mut status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some((thread_id, status));
        }

        loop {
            if status_rx.changed().await.is_err() {
                let latest = session.services.agent_control.get_status(thread_id).await;
                return is_final(&latest).then_some((thread_id, latest));
            }
            status = status_rx.borrow().clone();
            if is_final(&status) {
                return Some((thread_id, status));
            }
        }
    }
}

pub mod close_agent {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug, Deserialize, Serialize)]
    pub(super) struct CloseAgentResult {
        pub(super) status: AgentStatus,
    }

    pub async fn handle(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        call_id: String,
        arguments: String,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: CloseAgentArgs = parse_arguments(&arguments)?;
        let agent_id = agent_id(&args.id)?;
        session
            .send_event(
                &turn,
                CollabCloseBeginEvent {
                    call_id: call_id.clone(),
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                }
                .into(),
            )
            .await;
        let status = match session
            .services
            .agent_control
            .subscribe_status(agent_id)
            .await
        {
            Ok(mut status_rx) => status_rx.borrow_and_update().clone(),
            Err(err) => {
                let status = session.services.agent_control.get_status(agent_id).await;
                session
                    .send_event(
                        &turn,
                        CollabCloseEndEvent {
                            call_id: call_id.clone(),
                            sender_thread_id: session.conversation_id,
                            receiver_thread_id: agent_id,
                            status,
                        }
                        .into(),
                    )
                    .await;
                return Err(collab_agent_error(agent_id, err));
            }
        };
        let result = if !matches!(status, AgentStatus::Shutdown) {
            session
                .services
                .agent_control
                .shutdown_agent(agent_id)
                .await
                .map_err(|err| collab_agent_error(agent_id, err))
                .map(|_| ())
        } else {
            Ok(())
        };
        session
            .send_event(
                &turn,
                CollabCloseEndEvent {
                    call_id,
                    sender_thread_id: session.conversation_id,
                    receiver_thread_id: agent_id,
                    status: status.clone(),
                }
                .into(),
            )
            .await;
        result?;

        let content = serde_json::to_string(&CloseAgentResult { status }).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize close_agent result: {err}"))
        })?;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success: Some(true),
        })
    }
}

fn agent_id(id: &str) -> Result<ThreadId, FunctionCallError> {
    ThreadId::from_string(id)
        .map_err(|e| FunctionCallError::RespondToModel(format!("invalid agent id {id}: {e:?}")))
}

fn collab_spawn_error(err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab spawn failed: {err}")),
    }
}

fn collab_agent_error(agent_id: ThreadId, err: CodexErr) -> FunctionCallError {
    match err {
        CodexErr::ThreadNotFound(id) => {
            FunctionCallError::RespondToModel(format!("agent with id {id} not found"))
        }
        CodexErr::InternalAgentDied => {
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} is closed"))
        }
        CodexErr::UnsupportedOperation(_) => {
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        }
        err => FunctionCallError::RespondToModel(format!("collab tool failed: {err}")),
    }
}

fn thread_spawn_source(parent_thread_id: ThreadId, depth: i32) -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
    })
}

fn parse_collab_input(
    message: Option<String>,
    items: Option<Vec<UserInput>>,
) -> Result<Vec<UserInput>, FunctionCallError> {
    match (message, items) {
        (Some(_), Some(_)) => Err(FunctionCallError::RespondToModel(
            "Provide either message or items, but not both".to_string(),
        )),
        (None, None) => Err(FunctionCallError::RespondToModel(
            "Provide one of: message or items".to_string(),
        )),
        (Some(message), None) => {
            if message.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Empty message can't be sent to an agent".to_string(),
                ));
            }
            Ok(vec![UserInput::Text {
                text: message,
                text_elements: Vec::new(),
            }])
        }
        (None, Some(items)) => {
            if items.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "Items can't be empty".to_string(),
                ));
            }
            Ok(items)
        }
    }
}

fn input_preview(items: &[UserInput]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => text.clone(),
            UserInput::Image { .. } => "[image]".to_string(),
            UserInput::LocalImage { path } => format!("[local_image:{}]", path.display()),
            UserInput::Skill { name, path } => {
                format!("[skill:${name}]({})", path.display())
            }
            UserInput::Mention { name, path } => format!("[mention:${name}]({path})"),
            _ => "[input]".to_string(),
        })
        .collect();

    parts.join("\n")
}

fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    turn: &TurnContext,
    child_depth: i32,
    team_config: Option<&TomlValue>,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn, child_depth)?;
    if let Some(team_config) = team_config {
        config = apply_team_config(&config, team_config)?;
    }
    config.base_instructions = Some(base_instructions.text.clone());
    Ok(config)
}

fn apply_team_config(
    base_config: &Config,
    config_overlay: &TomlValue,
) -> Result<Config, FunctionCallError> {
    validate_team_agent_config_overlay(config_overlay)?;

    let mut team_layers = base_config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    team_layers.push(ConfigLayerEntry::new(
        ConfigLayerSource::SessionFlags,
        config_overlay.clone(),
    ));

    let team_layer_stack = ConfigLayerStack::new(
        team_layers,
        base_config.config_layer_stack.requirements().clone(),
        base_config.config_layer_stack.requirements_toml().clone(),
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("spawn_agent team config is invalid: {err}"))
    })?;

    let merged = team_layer_stack
        .effective_config()
        .try_into()
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "spawn_agent failed to deserialize merged config for team overlay: {err}"
            ))
        })?;

    Config::load_config_with_layer_stack(
        merged,
        ConfigOverrides::default(),
        base_config.codex_home.clone(),
        team_layer_stack,
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("spawn_agent team config failed to apply: {err}"))
    })
}

fn build_agent_resume_config(
    turn: &TurnContext,
    child_depth: i32,
) -> Result<Config, FunctionCallError> {
    let mut config = build_agent_shared_config(turn, child_depth)?;
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    Ok(config)
}

fn build_agent_shared_config(
    turn: &TurnContext,
    child_depth: i32,
) -> Result<Config, FunctionCallError> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    config.model = Some(turn.model_info.slug.clone());
    config.model_provider = turn.provider.clone();
    config.model_reasoning_effort = turn.reasoning_effort;
    config.model_reasoning_summary = turn.reasoning_summary;
    config.developer_instructions = turn.developer_instructions.clone();
    config.compact_prompt = turn.compact_prompt.clone();
    config.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
    config.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
    config.cwd = turn.cwd.clone();
    config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);
    config
        .permissions
        .sandbox_policy
        .set(turn.sandbox_policy.clone())
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("sandbox_policy is invalid: {err}"))
        })?;

    // If the new agent will be at max depth:
    if exceeds_thread_spawn_depth_limit(child_depth + 1) {
        config.features.disable(Feature::Collab);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthManager;
    use crate::CodexAuth;
    use crate::ThreadManager;
    use crate::agent::AgentRole;
    use crate::agent::MAX_THREAD_SPAWN_DEPTH;
    use crate::built_in_model_providers;
    use crate::codex::make_session_and_context;
    use crate::config::AgentsToml;
    use crate::config::types::ShellEnvironmentPolicy;
    use crate::function_tool::FunctionCallError;
    use crate::protocol::AskForApproval;
    use crate::protocol::Op;
    use crate::protocol::SandboxPolicy;
    use crate::protocol::SessionSource;
    use crate::protocol::SubAgentSource;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::ThreadId;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::protocol::InitialHistory;
    use codex_protocol::protocol::RolloutItem;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use tokio::time::timeout;

    fn invocation(
        session: Arc<crate::codex::Session>,
        turn: Arc<TurnContext>,
        tool_name: &str,
        payload: ToolPayload,
    ) -> ToolInvocation {
        ToolInvocation {
            session,
            turn,
            tracker: Arc::new(Mutex::new(TurnDiffTracker::default())),
            call_id: "call-1".to_string(),
            tool_name: tool_name.to_string(),
            payload,
        }
    }

    fn function_payload(args: serde_json::Value) -> ToolPayload {
        ToolPayload::Function {
            arguments: args.to_string(),
        }
    }

    fn thread_manager() -> ThreadManager {
        ThreadManager::with_models_provider_for_tests(
            CodexAuth::from_api_key("dummy"),
            built_in_model_providers()["openai"].clone(),
        )
    }

    #[tokio::test]
    async fn handler_rejects_non_function_payloads() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            ToolPayload::Custom {
                input: "hello".to_string(),
            },
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("payload should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "collab handler received unsupported payload".to_string()
            )
        );
    }

    #[tokio::test]
    async fn handler_rejects_unknown_tool() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "unknown_tool",
            function_payload(json!({})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("tool should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("unsupported collab tool unknown_tool".to_string())
        );
    }

    #[tokio::test]
    async fn plan_role_rejects_spawning_non_scout_agents() {
        let (session, mut turn) = make_session_and_context().await;
        turn.tools_config.agent_role = AgentRole::Plan;
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let invalid_agent_types = ["validator", "plan", "default"];

        for invalid_agent_type in invalid_agent_types {
            let invocation = invocation(
                session.clone(),
                turn.clone(),
                "spawn_agent",
                function_payload(json!({
                    "message": "context please",
                    "agent_type": invalid_agent_type,
                })),
            );
            let Err(err) = CollabHandler.handle(invocation).await else {
                panic!("spawn_agent should be rejected for non-scout agent_type");
            };
            assert_eq!(
                err,
                FunctionCallError::RespondToModel(
                    "Plan agents can only spawn scout agents. Set `role` (or `agent_type`) to `scout`."
                        .to_string()
                )
            );
        }
    }

    #[tokio::test]
    async fn spawn_agent_rejects_unknown_agent_type_labels() {
        let (session, turn) = make_session_and_context().await;
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        for unknown_agent_type in ["devops", "researcher-team"] {
            let invocation = invocation(
                session.clone(),
                turn.clone(),
                "spawn_agent",
                function_payload(json!({
                    "message": "context please",
                    "agent_type": unknown_agent_type,
                })),
            );
            let Err(err) = CollabHandler.handle(invocation).await else {
                panic!("unknown role should be rejected: {unknown_agent_type}");
            };
            assert_eq!(
                err,
                FunctionCallError::RespondToModel(
                    "spawn_agent agent_type must be one of: default, scout, validator, plan. Use `handle` for custom labels."
                        .to_string()
                )
            );
        }
    }

    #[tokio::test]
    async fn subagent_role_rejects_spawning_non_scout_agents() {
        let (session, mut turn) = make_session_and_context().await;
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::new(),
            depth: 0,
        });
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let invalid_agent_types = ["default", "validator", "plan"];

        for invalid_agent_type in invalid_agent_types {
            let invocation = invocation(
                session.clone(),
                turn.clone(),
                "spawn_agent",
                function_payload(json!({
                    "message": "context please",
                    "agent_type": invalid_agent_type,
                })),
            );
            let Err(err) = CollabHandler.handle(invocation).await else {
                panic!("spawn_agent should be rejected for non-scout agent_type");
            };
            assert_eq!(
                err,
                FunctionCallError::RespondToModel(
                    "Subagents can only spawn scout agents. Set `role` (or `agent_type`) to `scout`."
                        .to_string()
                )
            );
        }
    }

    #[tokio::test]
    async fn specialist_can_spawn_scouts() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::new(),
            depth: 0,
        });
        turn.tools_config.agent_role = AgentRole::Default;
        turn.scout_context_ready
            .store(false, std::sync::atomic::Ordering::Release);
        turn.context_validated
            .store(true, std::sync::atomic::Ordering::Release);

        let turn = Arc::new(turn);
        let invocation = invocation(
            Arc::new(session),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "collect context for patch",
                "role": "scout",
            })),
        );

        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("subagent should be able to spawn scout");
        match output {
            ToolOutput::Function { success, .. } => {
                assert_eq!(success, Some(true));
            }
            _ => panic!("expected function output"),
        }

        assert!(
            turn.scout_context_ready
                .load(std::sync::atomic::Ordering::Acquire),
            "scout spawn should set scout context readiness"
        );
        assert!(
            !turn
                .context_validated
                .load(std::sync::atomic::Ordering::Acquire),
            "new scout should reset context approval"
        );
    }

    #[tokio::test]
    async fn spawn_agent_accepts_custom_handle_and_color() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let invocation = invocation(
            session.clone(),
            turn,
            "spawn_agent",
            function_payload(json!({
                "message": "handle this",
                "role": "default",
                "handle": "DevOps Engineer",
                "color": "red"
            })),
        );

        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed");
        match output {
            ToolOutput::Function { success, .. } => {
                assert_eq!(success, Some(true));
            }
            _ => panic!("expected function output"),
        }

        let lifecycle = session.team_lifecycle_store.lock().await;
        let active_profile = lifecycle
            .profile("active-session-team")
            .expect("active-session-team profile should be recorded after successful spawn");
        assert!(
            active_profile.role_versions.contains_key("default"),
            "default role version should be captured in lifecycle profile"
        );
    }

    #[tokio::test]
    async fn spawn_agent_does_not_mutate_team_profile_when_spawn_fails() {
        let (session, turn) = make_session_and_context().await;
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let invocation = invocation(
            session.clone(),
            turn,
            "spawn_agent",
            function_payload(json!({
                "message": "this should fail because manager is unavailable",
                "role": "scout",
            })),
        );

        let Err(_) = CollabHandler.handle(invocation).await else {
            panic!("spawn should fail when thread manager is unavailable");
        };

        let lifecycle = session.team_lifecycle_store.lock().await;
        assert!(
            lifecycle.profile("active-session-team").is_none(),
            "failed spawn should not mutate active team profile"
        );
    }

    #[tokio::test]
    async fn spawn_agent_sets_context_validated_after_scout_ready_for_requesting_role() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        turn.tools_config.agent_role = AgentRole::Validator;
        turn.scout_context_ready
            .store(true, std::sync::atomic::Ordering::Release);
        turn.context_validated
            .store(false, std::sync::atomic::Ordering::Release);

        let turn = Arc::new(turn);
        let invocation = invocation(
            Arc::new(session),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "continue implementation",
                "role": "validator",
            })),
        );

        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed");
        match output {
            ToolOutput::Function { success, .. } => {
                assert_eq!(success, Some(true));
            }
            _ => panic!("expected function output"),
        }

        assert!(
            turn.context_validated
                .load(std::sync::atomic::Ordering::Acquire),
            "requesting role should be able to acknowledge scout context readiness"
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_unknown_color_token() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "handle this",
                "role": "default",
                "handle": "devops-engineer",
                "color": "orange"
            })),
        );

        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid color should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "spawn_agent color must be one of: red, green, yellow, blue, magenta, cyan."
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_fails_when_team_profile_is_missing() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "handle this",
                "team_agent": "platform-scout",
            })),
        );

        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("spawn should reject missing team profile");
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected user-facing validation error");
        };
        assert!(
            message.contains("spawn_agent team profile `platform-scout` was not found under"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn spawn_agent_applies_team_profile_config_override() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        let profile_dir = temp_dir
            .path()
            .join(".codex")
            .join("team")
            .join("architect");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create profile directory");
        async_fs::write(profile_dir.join("prompt"), "Use architect profile.")
            .await
            .expect("write profile prompt");
        async_fs::write(
            profile_dir.join("config.toml"),
            "model = \"gpt-5.1-codex-max\"\n",
        )
        .await
        .expect("write profile config");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "run with team profile",
                "role": "default",
                "team_agent": "architect",
            })),
        );

        CollabHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed with valid team profile");

        let thread_ids = manager.list_thread_ids().await;
        assert_eq!(thread_ids.len(), 1);
        let thread = manager
            .get_thread(thread_ids[0])
            .await
            .expect("spawned thread should exist");
        let snapshot = thread.config_snapshot().await;
        assert_eq!(snapshot.model, "gpt-5.1-codex-max");
    }

    #[tokio::test]
    async fn spawn_agent_applies_namespaced_team_profile_config_override() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        let profile_dir = temp_dir
            .path()
            .join(".codex")
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create profile directory");
        async_fs::write(
            temp_dir
                .path()
                .join(".codex")
                .join("agents")
                .join("backend-review")
                .join("team.toml"),
            "[team]\nname = \"backend-review\"\norchestrator = \"validator\"\n",
        )
        .await
        .expect("write team manifest");
        async_fs::write(
            profile_dir.join("system_prompt.md"),
            "Use backend validator profile.",
        )
        .await
        .expect("write profile prompt");
        async_fs::write(
            profile_dir.join("config.toml"),
            "model = \"gpt-5.1-codex-max\"\n",
        )
        .await
        .expect("write profile config");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "run with namespaced team profile",
                "role": "default",
                "team_agent": "backend-review/validator",
            })),
        );

        CollabHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed with valid namespaced team profile");

        let thread_ids = manager.list_thread_ids().await;
        assert_eq!(thread_ids.len(), 1);
        let thread = manager
            .get_thread(thread_ids[0])
            .await
            .expect("spawned thread should exist");
        let snapshot = thread.config_snapshot().await;
        assert_eq!(snapshot.model, "gpt-5.1-codex-max");
    }

    #[tokio::test]
    async fn spawn_agent_applies_namespaced_team_profile_config_override_from_codex_home() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        let profile_dir = turn
            .config
            .codex_home
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create codex_home profile directory");
        async_fs::write(
            turn.config
                .codex_home
                .join("agents")
                .join("backend-review")
                .join("team.toml"),
            "[team]\nname = \"backend-review\"\norchestrator = \"validator\"\n",
        )
        .await
        .expect("write team manifest");
        async_fs::write(
            profile_dir.join("system_prompt.md"),
            "Use backend validator profile from codex_home.",
        )
        .await
        .expect("write profile prompt");
        async_fs::write(
            profile_dir.join("config.toml"),
            "model = \"gpt-5.1-codex-max\"\n",
        )
        .await
        .expect("write profile config");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "run with namespaced codex_home team profile",
                "role": "default",
                "team_agent": "backend-review/validator",
            })),
        );

        CollabHandler
            .handle(invocation)
            .await
            .expect("spawn should succeed with codex_home namespaced profile");

        let thread_ids = manager.list_thread_ids().await;
        assert_eq!(thread_ids.len(), 1);
        let thread = manager
            .get_thread(thread_ids[0])
            .await
            .expect("spawned thread should exist");
        let snapshot = thread.config_snapshot().await;
        assert_eq!(snapshot.model, "gpt-5.1-codex-max");
    }

    #[tokio::test]
    async fn team_agent_get_reads_namespaced_profile_from_codex_home() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        let profile_dir = turn
            .config
            .codex_home
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create codex_home profile directory");
        async_fs::write(
            profile_dir.join("system_prompt.md"),
            "Use validator profile from codex_home.",
        )
        .await
        .expect("write profile prompt");
        async_fs::write(
            profile_dir.join("config.toml"),
            "model = \"gpt-5.1-codex-max\"\n",
        )
        .await
        .expect("write profile config");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "team_agent_get",
            function_payload(json!({
                "team_agent": "backend-review/validator",
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("team_agent_get should read codex_home namespaced profile");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        assert_eq!(success, Some(true));
        let get_json: serde_json::Value = serde_json::from_str(&content).expect("get json");
        assert_eq!(
            get_json["team_agent"],
            serde_json::Value::String("backend-review/validator".to_string())
        );
        assert_eq!(
            get_json["prompt"],
            serde_json::Value::String("Use validator profile from codex_home.".to_string())
        );
        assert!(
            get_json["config_toml"]
                .as_str()
                .is_some_and(|config| config.contains("model = \"gpt-5.1-codex-max\""))
        );
    }

    #[tokio::test]
    async fn team_agent_list_includes_namespaced_profiles_from_codex_home() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        let profile_dir = turn
            .config
            .codex_home
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create codex_home profile directory");
        async_fs::write(
            profile_dir.join("system_prompt.md"),
            "Use validator profile from codex_home.",
        )
        .await
        .expect("write profile prompt");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "team_agent_list",
            function_payload(json!({})),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("team_agent_list should include codex_home namespaced profile");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        assert_eq!(success, Some(true));
        let list_json: serde_json::Value = serde_json::from_str(&content).expect("list json");
        let agents = list_json["agents"]
            .as_array()
            .expect("agents list should be array");
        let Some(entry) = agents.iter().find(|entry| {
            entry["name"] == serde_json::Value::String("backend-review/validator".to_string())
        }) else {
            panic!("expected backend-review/validator in team_agent_list result: {agents:?}");
        };
        assert_eq!(entry["has_prompt"], serde_json::Value::Bool(true));
    }

    #[tokio::test]
    async fn team_agent_get_resolves_unique_namespaced_profile_without_team_segment() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        let profile_dir = turn
            .config
            .codex_home
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&profile_dir)
            .await
            .expect("create codex_home profile directory");
        async_fs::write(
            profile_dir.join("system_prompt.md"),
            "Use validator profile from codex_home.",
        )
        .await
        .expect("write profile prompt");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "team_agent_get",
            function_payload(json!({
                "team_agent": "validator",
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("team_agent_get should resolve unique namespaced profile");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        assert_eq!(success, Some(true));
        let get_json: serde_json::Value = serde_json::from_str(&content).expect("get json");
        assert_eq!(
            get_json["team_agent"],
            serde_json::Value::String("validator".to_string())
        );
        assert_eq!(
            get_json["prompt"],
            serde_json::Value::String("Use validator profile from codex_home.".to_string())
        );
    }

    #[tokio::test]
    async fn team_agent_get_rejects_ambiguous_namespaced_profile_without_team_segment() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        for team in ["backend-review", "frontend-review"] {
            let profile_dir = turn
                .config
                .codex_home
                .join("agents")
                .join(team)
                .join("validator");
            async_fs::create_dir_all(&profile_dir)
                .await
                .expect("create codex_home profile directory");
            async_fs::write(
                profile_dir.join("system_prompt.md"),
                "Use validator profile.",
            )
            .await
            .expect("write profile prompt");
        }

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "team_agent_get",
            function_payload(json!({
                "team_agent": "validator",
            })),
        );
        let Err(FunctionCallError::RespondToModel(message)) =
            CollabHandler.handle(invocation).await
        else {
            panic!("team_agent_get should reject ambiguous namespaced profile");
        };
        assert!(
            message.contains("is ambiguous across teams"),
            "unexpected ambiguity error: {message}"
        );
    }

    #[tokio::test]
    async fn spawn_agent_does_not_fallback_to_global_on_invalid_local_profile() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();

        let local_profile_dir = temp_dir
            .path()
            .join(".codex")
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&local_profile_dir)
            .await
            .expect("create local profile directory");
        async_fs::write(
            local_profile_dir.join("system_prompt.md"),
            "Use local validator profile.",
        )
        .await
        .expect("write local profile prompt");
        async_fs::write(local_profile_dir.join("config.toml"), "model = [")
            .await
            .expect("write invalid local profile config");

        let global_profile_dir = turn
            .config
            .codex_home
            .join("agents")
            .join("backend-review")
            .join("validator");
        async_fs::create_dir_all(&global_profile_dir)
            .await
            .expect("create global profile directory");
        async_fs::write(
            global_profile_dir.join("system_prompt.md"),
            "Use global validator profile.",
        )
        .await
        .expect("write global profile prompt");
        async_fs::write(
            global_profile_dir.join("config.toml"),
            "model = \"gpt-5.1-codex-max\"\n",
        )
        .await
        .expect("write global profile config");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "run with namespaced team profile",
                "role": "default",
                "team_agent": "backend-review/validator",
            })),
        );

        let Err(FunctionCallError::RespondToModel(message)) =
            CollabHandler.handle(invocation).await
        else {
            panic!("spawn should fail when local profile config is invalid");
        };
        assert!(
            message.contains("is invalid TOML"),
            "unexpected error for invalid local profile: {message}"
        );
        assert!(
            message.contains(
                local_profile_dir
                    .join("config.toml")
                    .to_string_lossy()
                    .as_ref()
            ),
            "error should reference local invalid config path: {message}"
        );
    }

    #[tokio::test]
    async fn team_agent_tools_reject_segments_without_alphanumeric_chars() {
        let (session, turn) = make_session_and_context().await;
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        for invalid_team_agent in [".", "_", "---"] {
            let invocation = invocation(
                session.clone(),
                turn.clone(),
                "team_agent_get",
                function_payload(json!({
                    "team_agent": invalid_team_agent,
                })),
            );

            let Err(FunctionCallError::RespondToModel(message)) =
                CollabHandler.handle(invocation).await
            else {
                panic!("team_agent_get should reject invalid team_agent={invalid_team_agent}");
            };
            assert!(
                message.contains("must contain at least one alphanumeric character"),
                "unexpected error for team_agent={invalid_team_agent}: {message}"
            );
        }
    }

    #[tokio::test]
    async fn team_agent_profile_tools_roundtrip() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        let upsert_invocation = invocation(
            session.clone(),
            turn.clone(),
            "team_agent_upsert",
            function_payload(json!({
                "team_agent": "scout",
                "prompt": "Collect deterministic context.",
                "config_toml": "model = \"gpt-5-mini\"\n[tools]\nweb_search = false\n"
            })),
        );
        let upsert_output = CollabHandler
            .handle(upsert_invocation)
            .await
            .expect("team_agent_upsert should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(upsert_content),
            success: upsert_success,
            ..
        } = upsert_output
        else {
            panic!("expected function output");
        };
        assert_eq!(upsert_success, Some(true));
        let upsert_json: serde_json::Value =
            serde_json::from_str(&upsert_content).expect("upsert result json");
        assert_eq!(upsert_json["prompt_written"], true);
        assert_eq!(upsert_json["config_written"], true);

        let get_invocation = invocation(
            session.clone(),
            turn.clone(),
            "team_agent_get",
            function_payload(json!({
                "team_agent": "scout",
            })),
        );
        let get_output = CollabHandler
            .handle(get_invocation)
            .await
            .expect("team_agent_get should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(get_content),
            success: get_success,
            ..
        } = get_output
        else {
            panic!("expected function output");
        };
        assert_eq!(get_success, Some(true));
        let get_json: serde_json::Value = serde_json::from_str(&get_content).expect("get json");
        assert_eq!(get_json["team_agent"], "scout");
        assert_eq!(get_json["prompt"], "Collect deterministic context.");
        assert!(
            get_json["config_toml"]
                .as_str()
                .is_some_and(|config| config.contains("model = \"gpt-5-mini\""))
        );

        let list_invocation = invocation(
            session.clone(),
            turn.clone(),
            "team_agent_list",
            function_payload(json!({})),
        );
        let list_output = CollabHandler
            .handle(list_invocation)
            .await
            .expect("team_agent_list should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(list_content),
            success: list_success,
            ..
        } = list_output
        else {
            panic!("expected function output");
        };
        assert_eq!(list_success, Some(true));
        let list_json: serde_json::Value = serde_json::from_str(&list_content).expect("list json");
        let agents = list_json["agents"]
            .as_array()
            .expect("agents list should be array");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "scout");
        assert_eq!(agents[0]["has_prompt"], true);
        assert_eq!(agents[0]["has_config"], true);

        let delete_invocation = invocation(
            session,
            turn,
            "team_agent_delete",
            function_payload(json!({
                "team_agent": "scout",
            })),
        );
        let delete_output = CollabHandler
            .handle(delete_invocation)
            .await
            .expect("team_agent_delete should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(delete_content),
            success: delete_success,
            ..
        } = delete_output
        else {
            panic!("expected function output");
        };
        assert_eq!(delete_success, Some(true));
        let delete_json: serde_json::Value =
            serde_json::from_str(&delete_content).expect("delete json");
        assert_eq!(delete_json["deleted"], true);
    }

    #[tokio::test]
    async fn namespaced_team_agent_profile_roundtrip_uses_system_prompt_file() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        let upsert_invocation = invocation(
            session.clone(),
            turn.clone(),
            "team_agent_upsert",
            function_payload(json!({
                "team_agent": "backend-review/validator",
                "prompt": "Use strict backend validator rubric.",
                "config_toml": "model = \"gpt-5-mini\"\n"
            })),
        );
        CollabHandler
            .handle(upsert_invocation)
            .await
            .expect("team_agent_upsert should succeed for namespaced profile");

        let system_prompt_path = temp_dir
            .path()
            .join(".codex")
            .join("agents")
            .join("backend-review")
            .join("validator")
            .join("system_prompt.md");
        assert!(
            system_prompt_path.is_file(),
            "expected system prompt at {}",
            system_prompt_path.display()
        );

        let get_invocation = invocation(
            session.clone(),
            turn.clone(),
            "team_agent_get",
            function_payload(json!({
                "team_agent": "backend-review/validator",
            })),
        );
        let get_output = CollabHandler
            .handle(get_invocation)
            .await
            .expect("team_agent_get should succeed for namespaced profile");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(get_content),
            success: get_success,
            ..
        } = get_output
        else {
            panic!("expected function output");
        };
        assert_eq!(get_success, Some(true));
        let get_json: serde_json::Value = serde_json::from_str(&get_content).expect("get json");
        assert_eq!(
            get_json["team_agent"],
            serde_json::Value::String("backend-review/validator".to_string())
        );
        assert_eq!(
            get_json["prompt"],
            serde_json::Value::String("Use strict backend validator rubric.".to_string())
        );
    }

    #[tokio::test]
    async fn team_agent_upsert_rejects_disallowed_config_keys() {
        let (session, mut turn) = make_session_and_context().await;
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "team_agent_upsert",
            function_payload(json!({
                "team_agent": "validator",
                "config_toml": "approval_policy = \"never\"",
            })),
        );

        let Err(FunctionCallError::RespondToModel(message)) =
            CollabHandler.handle(invocation).await
        else {
            panic!("team_agent_upsert should reject disallowed keys");
        };
        assert!(
            message.contains("unsupported top-level keys: approval_policy"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_empty_message() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "   "})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("empty message should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Empty message can't be sent to an agent".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_message_and_items_are_both_set() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            })),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("message+items should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Provide either message or items, but not both".to_string()
            )
        );
    }

    #[tokio::test]
    async fn spawn_agent_errors_when_manager_dropped() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("spawn should fail without a manager");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("collab manager unavailable".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_agent_rejects_when_depth_limit_exceeded() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: MAX_THREAD_SPAWN_DEPTH,
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(json!({"message": "hello"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("spawn should fail when depth limit exceeded");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_empty_message() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": ThreadId::new().to_string(), "message": ""})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("empty message should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Empty message can't be sent to an agent".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_when_message_and_items_are_both_set() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": ThreadId::new().to_string(),
                "message": "hello",
                "items": [{"type": "mention", "name": "drive", "path": "app://drive"}]
            })),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("message+items should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Provide either message or items, but not both".to_string()
            )
        );
    }

    #[tokio::test]
    async fn send_input_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": "not-a-uuid", "message": "hi"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id not-a-uuid:"));
    }

    #[tokio::test]
    async fn send_input_reports_missing_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let agent_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({"id": agent_id.to_string(), "message": "hi"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("missing agent should be reported");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
        );
    }

    #[tokio::test]
    async fn send_input_interrupts_before_prompt() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "message": "hi",
                "interrupt": true
            })),
        );
        CollabHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");

        let ops = manager.captured_ops();
        let ops_for_agent: Vec<&Op> = ops
            .iter()
            .filter_map(|(id, op)| (*id == agent_id).then_some(op))
            .collect();
        assert_eq!(ops_for_agent.len(), 2);
        assert!(matches!(ops_for_agent[0], Op::Interrupt));
        assert!(matches!(ops_for_agent[1], Op::UserInput { .. }));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn send_input_accepts_structured_items() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "send_input",
            function_payload(json!({
                "id": agent_id.to_string(),
                "items": [
                    {"type": "mention", "name": "drive", "path": "app://google_drive"},
                    {"type": "text", "text": "read the folder"}
                ]
            })),
        );
        CollabHandler
            .handle(invocation)
            .await
            .expect("send_input should succeed");

        let expected = Op::UserInput {
            items: vec![
                UserInput::Mention {
                    name: "drive".to_string(),
                    path: "app://google_drive".to_string(),
                },
                UserInput::Text {
                    text: "read the folder".to_string(),
                    text_elements: Vec::new(),
                },
            ],
            final_output_json_schema: None,
        };
        let captured = manager
            .captured_ops()
            .into_iter()
            .find(|(id, op)| *id == agent_id && *op == expected);
        assert_eq!(captured, Some((agent_id, expected)));

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn resume_agent_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": "not-a-uuid"})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id not-a-uuid:"));
    }

    #[tokio::test]
    async fn resume_agent_reports_missing_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let agent_id = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("missing agent should be reported");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(format!("agent with id {agent_id} not found"))
        );
    }

    #[tokio::test]
    async fn resume_agent_noops_for_active_agent() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let status_before = manager.agent_control().get_status(agent_id).await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );

        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("resume_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: resume_agent::ResumeAgentResult =
            serde_json::from_str(&content).expect("resume_agent result should be json");
        assert_eq!(result.status, status_before);
        assert_eq!(success, Some(true));

        let thread_ids = manager.list_thread_ids().await;
        assert_eq!(thread_ids, vec![agent_id]);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn resume_agent_restores_closed_agent_and_accepts_send_input() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager
            .resume_thread_with_history(
                config,
                InitialHistory::Forked(vec![RolloutItem::ResponseItem(ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "materialized".to_string(),
                    }],
                    end_turn: None,
                    phase: None,
                })]),
                AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy")),
                false,
            )
            .await
            .expect("start thread");
        let agent_id = thread.thread_id;
        let _ = manager
            .agent_control()
            .shutdown_agent(agent_id)
            .await
            .expect("shutdown agent");
        assert_eq!(
            manager.agent_control().get_status(agent_id).await,
            AgentStatus::NotFound
        );
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        let resume_invocation = invocation(
            session.clone(),
            turn.clone(),
            "resume_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = CollabHandler
            .handle(resume_invocation)
            .await
            .expect("resume_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: resume_agent::ResumeAgentResult =
            serde_json::from_str(&content).expect("resume_agent result should be json");
        assert_ne!(result.status, AgentStatus::NotFound);
        assert_eq!(success, Some(true));

        let send_invocation = invocation(
            session,
            turn,
            "send_input",
            function_payload(json!({"id": agent_id.to_string(), "message": "hello"})),
        );
        let output = CollabHandler
            .handle(send_invocation)
            .await
            .expect("send_input should succeed after resume");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: serde_json::Value =
            serde_json::from_str(&content).expect("send_input result should be json");
        let submission_id = result
            .get("submission_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(!submission_id.is_empty());
        assert_eq!(success, Some(true));

        let _ = manager
            .agent_control()
            .shutdown_agent(agent_id)
            .await
            .expect("shutdown resumed agent");
    }

    #[tokio::test]
    async fn resume_agent_rejects_when_depth_limit_exceeded() {
        let (mut session, mut turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();

        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: session.conversation_id,
            depth: MAX_THREAD_SPAWN_DEPTH,
        });

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "resume_agent",
            function_payload(json!({"id": ThreadId::new().to_string()})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("resume should fail when depth limit exceeded");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "Agent depth limit reached. Solve the task yourself.".to_string()
            )
        );
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct WaitResult {
        status: HashMap<ThreadId, AgentStatus>,
        timed_out: bool,
    }

    #[tokio::test]
    async fn wait_rejects_non_positive_timeout() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [ThreadId::new().to_string()],
                "timeout_ms": 0
            })),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("non-positive timeout should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("timeout_ms must be greater than zero".to_string())
        );
    }

    #[tokio::test]
    async fn wait_rejects_invalid_id() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({"ids": ["invalid"]})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("invalid id should be rejected");
        };
        let FunctionCallError::RespondToModel(msg) = err else {
            panic!("expected respond-to-model error");
        };
        assert!(msg.starts_with("invalid agent id invalid:"));
    }

    #[tokio::test]
    async fn wait_rejects_empty_ids() {
        let (session, turn) = make_session_and_context().await;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({"ids": []})),
        );
        let Err(err) = CollabHandler.handle(invocation).await else {
            panic!("empty ids should be rejected");
        };
        assert_eq!(
            err,
            FunctionCallError::RespondToModel("ids must be non-empty".to_string())
        );
    }

    #[tokio::test]
    async fn wait_returns_not_found_for_missing_agents() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let id_a = ThreadId::new();
        let id_b = ThreadId::new();
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [id_a.to_string(), id_b.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::from([
                    (id_a, AgentStatus::NotFound),
                    (id_b, AgentStatus::NotFound),
                ]),
                timed_out: false
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn wait_times_out_when_status_is_not_final() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": MIN_WAIT_TIMEOUT_MS
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::new(),
                timed_out: true
            }
        );
        assert_eq!(success, None);

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_clamps_short_timeouts_to_minimum() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": 10
            })),
        );

        let early = timeout(Duration::from_millis(50), CollabHandler.handle(invocation)).await;
        assert!(
            early.is_err(),
            "wait should not return before the minimum timeout clamp"
        );

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
    }

    #[tokio::test]
    async fn wait_returns_final_status_without_timeout() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let mut status_rx = manager
            .agent_control()
            .subscribe_status(agent_id)
            .await
            .expect("subscribe should succeed");

        let _ = thread
            .thread
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");
        let _ = timeout(Duration::from_secs(1), status_rx.changed())
            .await
            .expect("shutdown status should arrive");

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "wait",
            function_payload(json!({
                "ids": [agent_id.to_string()],
                "timeout_ms": 1000
            })),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("wait should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: WaitResult =
            serde_json::from_str(&content).expect("wait result should be json");
        assert_eq!(
            result,
            WaitResult {
                status: HashMap::from([(agent_id, AgentStatus::Shutdown)]),
                timed_out: false
            }
        );
        assert_eq!(success, None);
    }

    #[tokio::test]
    async fn close_agent_submits_shutdown_and_returns_status() {
        let (mut session, turn) = make_session_and_context().await;
        let manager = thread_manager();
        session.services.agent_control = manager.agent_control();
        let config = turn.config.as_ref().clone();
        let thread = manager.start_thread(config).await.expect("start thread");
        let agent_id = thread.thread_id;
        let status_before = manager.agent_control().get_status(agent_id).await;

        let invocation = invocation(
            Arc::new(session),
            Arc::new(turn),
            "close_agent",
            function_payload(json!({"id": agent_id.to_string()})),
        );
        let output = CollabHandler
            .handle(invocation)
            .await
            .expect("close_agent should succeed");
        let ToolOutput::Function {
            body: FunctionCallOutputBody::Text(content),
            success,
            ..
        } = output
        else {
            panic!("expected function output");
        };
        let result: close_agent::CloseAgentResult =
            serde_json::from_str(&content).expect("close_agent result should be json");
        assert_eq!(result.status, status_before);
        assert_eq!(success, Some(true));

        let ops = manager.captured_ops();
        let submitted_shutdown = ops
            .iter()
            .any(|(id, op)| *id == agent_id && matches!(op, Op::Shutdown));
        assert_eq!(submitted_shutdown, true);

        let status_after = manager.agent_control().get_status(agent_id).await;
        assert_eq!(status_after, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn build_agent_spawn_config_uses_turn_context_values() {
        fn pick_allowed_sandbox_policy(
            constraint: &crate::config::Constrained<SandboxPolicy>,
            base: SandboxPolicy,
        ) -> SandboxPolicy {
            let candidates = [
                SandboxPolicy::new_read_only_policy(),
                SandboxPolicy::new_workspace_write_policy(),
                SandboxPolicy::DangerFullAccess,
            ];
            candidates
                .into_iter()
                .find(|candidate| *candidate != base && constraint.can_set(candidate).is_ok())
                .unwrap_or(base)
        }

        let (_session, mut turn) = make_session_and_context().await;
        let base_instructions = BaseInstructions {
            text: "base".to_string(),
        };
        turn.developer_instructions = Some("dev".to_string());
        turn.compact_prompt = Some("compact".to_string());
        turn.shell_environment_policy = ShellEnvironmentPolicy {
            use_profile: true,
            ..ShellEnvironmentPolicy::default()
        };
        let temp_dir = tempfile::tempdir().expect("temp dir");
        turn.cwd = temp_dir.path().to_path_buf();
        turn.codex_linux_sandbox_exe = Some(PathBuf::from("/bin/echo"));
        turn.sandbox_policy = pick_allowed_sandbox_policy(
            &turn.config.permissions.sandbox_policy,
            turn.config.permissions.sandbox_policy.get().clone(),
        );

        let config =
            build_agent_spawn_config(&base_instructions, &turn, 0, None).expect("spawn config");
        let mut expected = (*turn.config).clone();
        expected.base_instructions = Some(base_instructions.text);
        expected.model = Some(turn.model_info.slug.clone());
        expected.model_provider = turn.provider.clone();
        expected.model_reasoning_effort = turn.reasoning_effort;
        expected.model_reasoning_summary = turn.reasoning_summary;
        expected.developer_instructions = turn.developer_instructions.clone();
        expected.compact_prompt = turn.compact_prompt.clone();
        expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
        expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
        expected.cwd = turn.cwd.clone();
        expected
            .permissions
            .approval_policy
            .set(AskForApproval::Never)
            .expect("approval policy set");
        expected
            .permissions
            .sandbox_policy
            .set(turn.sandbox_policy)
            .expect("sandbox policy set");
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn build_agent_spawn_config_preserves_base_user_instructions() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config.user_instructions = Some("base-user".to_string());
        turn.user_instructions = Some("resolved-user".to_string());
        turn.config = Arc::new(base_config.clone());
        let base_instructions = BaseInstructions {
            text: "base".to_string(),
        };

        let config =
            build_agent_spawn_config(&base_instructions, &turn, 0, None).expect("spawn config");

        assert_eq!(config.user_instructions, base_config.user_instructions);
    }

    #[tokio::test]
    async fn build_agent_resume_config_clears_base_instructions() {
        let (_session, mut turn) = make_session_and_context().await;
        let mut base_config = (*turn.config).clone();
        base_config.base_instructions = Some("caller-base".to_string());
        turn.config = Arc::new(base_config);

        let config = build_agent_resume_config(&turn, 0).expect("resume config");

        let mut expected = (*turn.config).clone();
        expected.base_instructions = None;
        expected.model = Some(turn.model_info.slug.clone());
        expected.model_provider = turn.provider.clone();
        expected.model_reasoning_effort = turn.reasoning_effort;
        expected.model_reasoning_summary = turn.reasoning_summary;
        expected.developer_instructions = turn.developer_instructions.clone();
        expected.compact_prompt = turn.compact_prompt.clone();
        expected.permissions.shell_environment_policy = turn.shell_environment_policy.clone();
        expected.codex_linux_sandbox_exe = turn.codex_linux_sandbox_exe.clone();
        expected.cwd = turn.cwd.clone();
        expected
            .permissions
            .approval_policy
            .set(AskForApproval::Never)
            .expect("approval policy set");
        expected
            .permissions
            .sandbox_policy
            .set(turn.sandbox_policy)
            .expect("sandbox policy set");
        assert_eq!(config, expected);
    }

    #[tokio::test]
    async fn build_agent_spawn_config_applies_role_models() {
        let (_session, mut turn) = make_session_and_context().await;
        let base_instructions = BaseInstructions {
            text: "base".to_string(),
        };

        turn.config = Arc::new(Config {
            agents: AgentsToml {
                max_threads: Some(7),
                main_model: Some("main-role".to_string()),
                scout_model: Some("scout-role".to_string()),
                validator_model: None,
                plan_model: Some("plan-role".to_string()),
            },
            model: Some("fallback-model".to_string()),
            ..(*turn.config).clone()
        });

        let mut config =
            build_agent_spawn_config(&base_instructions, &turn, 0, None).expect("spawn config");
        let writable_policy = [
            SandboxPolicy::new_workspace_write_policy(),
            SandboxPolicy::DangerFullAccess,
        ]
        .into_iter()
        .find(|policy| config.permissions.sandbox_policy.can_set(policy).is_ok())
        .unwrap_or_else(|| config.permissions.sandbox_policy.get().clone());
        config
            .permissions
            .sandbox_policy
            .set(writable_policy.clone())
            .expect("test should start from a writable sandbox policy");

        let mut default_config = config.clone();
        AgentRole::Default
            .apply_to_config(&mut default_config)
            .expect("default role should inherit main model");
        assert_eq!(default_config.model, Some("main-role".to_string()));
        assert_eq!(
            *default_config.permissions.sandbox_policy.get(),
            writable_policy
        );

        let mut scout_config = config.clone();
        AgentRole::Scout
            .apply_to_config(&mut scout_config)
            .expect("scout should use scout override");
        assert_eq!(scout_config.model, Some("scout-role".to_string()));
        assert_eq!(
            *scout_config.permissions.sandbox_policy.get(),
            SandboxPolicy::new_read_only_policy()
        );

        let mut validator_config = config.clone();
        AgentRole::Validator
            .apply_to_config(&mut validator_config)
            .expect("validator should fallback to main model");
        assert_eq!(validator_config.model, Some("main-role".to_string()));
        assert_eq!(
            *validator_config.permissions.sandbox_policy.get(),
            writable_policy
        );

        let mut plan_config = config.clone();
        AgentRole::Plan
            .apply_to_config(&mut plan_config)
            .expect("plan should use plan override");
        assert_eq!(plan_config.model, Some("plan-role".to_string()));
        assert_eq!(
            *plan_config.permissions.sandbox_policy.get(),
            writable_policy
        );

        let mut fallback_config = config;
        fallback_config.agents = AgentsToml::default();
        fallback_config.model = Some("global-fallback".to_string());
        AgentRole::Scout
            .apply_to_config(&mut fallback_config)
            .expect("scout should fallback to global model");
        assert_eq!(fallback_config.model, Some("global-fallback".to_string()));
    }
}
