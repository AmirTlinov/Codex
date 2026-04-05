use crate::config::ConfigToml;
use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use codex_config::CONFIG_TOML_FILE;
use codex_protocol::openai_models::ReasoningEffort;
use sha1::Digest;
use sha1::Sha1;
use std::io;
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use toml::Value as TomlValue;
use toml_edit::value;

pub const CLAUDEX_HOME_MIGRATION_FILENAME: &str = ".claudex_home_migration";
pub const CLAUDEX_HOME_SEEDED_FILENAME: &str = ".claudex_seeded_from_codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudexHomeMigrationStatus {
    SkippedNonClaudexHome,
    SkippedMarker,
    SkippedNoSeedMarker,
    SkippedNoConfig,
    SkippedSeededConfigChanged,
    SkippedNonLegacyDefaults,
    Applied,
}

pub async fn maybe_migrate_claudex_home(
    codex_home: &Path,
) -> io::Result<ClaudexHomeMigrationStatus> {
    if !is_claudex_home(codex_home) {
        return Ok(ClaudexHomeMigrationStatus::SkippedNonClaudexHome);
    }

    let marker_path = codex_home.join(CLAUDEX_HOME_MIGRATION_FILENAME);
    if tokio::fs::try_exists(&marker_path).await? {
        return Ok(ClaudexHomeMigrationStatus::SkippedMarker);
    }
    let seed_marker_path = codex_home.join(CLAUDEX_HOME_SEEDED_FILENAME);
    if !tokio::fs::try_exists(&seed_marker_path).await? {
        return Ok(ClaudexHomeMigrationStatus::SkippedNoSeedMarker);
    }

    let config_path = codex_home.join(CONFIG_TOML_FILE);
    if !tokio::fs::try_exists(&config_path).await? {
        return Ok(ClaudexHomeMigrationStatus::SkippedNoConfig);
    }
    let config_text = tokio::fs::read_to_string(&config_path).await?;
    if !seed_marker_matches_current_config(&seed_marker_path, &config_text).await? {
        create_marker(&marker_path).await?;
        return Ok(ClaudexHomeMigrationStatus::SkippedSeededConfigChanged);
    }
    let raw_toml = toml::from_str::<TomlValue>(&config_text)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let config_toml = crate::config::deserialize_config_toml_with_base(raw_toml, codex_home)?;

    if !looks_like_legacy_copied_defaults(&config_toml) {
        create_marker(&marker_path).await?;
        return Ok(ClaudexHomeMigrationStatus::SkippedNonLegacyDefaults);
    }

    ConfigEditsBuilder::new(codex_home)
        .with_edits([
            ConfigEdit::SetModel {
                model: Some("claude-opus-4-6".to_string()),
                effort: None,
            },
            ConfigEdit::SetModelProvider {
                provider_id: Some(crate::CLAUDE_CODE_PROVIDER_ID.to_string()),
            },
            ConfigEdit::SetPath {
                segments: vec!["model_reasoning_effort".to_string()],
                value: value("max"),
            },
            ConfigEdit::SetPath {
                segments: vec!["agent_backend".to_string()],
                value: value("claude_code"),
            },
            ConfigEdit::SetPath {
                segments: vec!["plan_mode_reasoning_effort".to_string()],
                value: value("max"),
            },
        ])
        .apply()
        .await
        .map_err(|err| {
            io::Error::other(format!("failed to persist Claudex home migration: {err}"))
        })?;

    create_marker(&marker_path).await?;
    Ok(ClaudexHomeMigrationStatus::Applied)
}

fn is_claudex_home(codex_home: &Path) -> bool {
    codex_home
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".claudex")
}

fn looks_like_legacy_copied_defaults(config_toml: &ConfigToml) -> bool {
    matches!(config_toml.model.as_deref(), None | Some("gpt-5.4"))
        && matches!(config_toml.model_provider.as_deref(), None | Some("openai"))
        && config_toml.agent_backend.is_none()
        && matches!(
            config_toml.model_reasoning_effort,
            None | Some(ReasoningEffort::XHigh)
        )
        && matches!(
            config_toml.plan_mode_reasoning_effort,
            None | Some(ReasoningEffort::XHigh)
        )
}

async fn create_marker(marker_path: &Path) -> io::Result<()> {
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker_path)
        .await
    {
        Ok(mut file) => file.write_all(b"v1\n").await,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

async fn seed_marker_matches_current_config(
    seed_marker_path: &Path,
    config_text: &str,
) -> io::Result<bool> {
    let marker_contents = tokio::fs::read_to_string(seed_marker_path).await?;
    let expected_sha1 = marker_contents
        .lines()
        .find_map(|line| line.strip_prefix("config_sha1="))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(expected_sha1) = expected_sha1 else {
        return Ok(false);
    };
    Ok(expected_sha1 == sha1_hex(config_text.as_bytes()))
}

fn sha1_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha1::digest(bytes))
}

#[cfg(test)]
#[path = "claudex_home_migration_tests.rs"]
mod tests;
