use super::*;
use crate::config::AgentBackend;
use pretty_assertions::assert_eq;
use std::io;
use std::path::Path;
use tempfile::TempDir;

async fn read_config_toml(codex_home: &Path) -> io::Result<ConfigToml> {
    let contents = tokio::fs::read_to_string(codex_home.join("config.toml")).await?;
    toml::from_str(&contents).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

async fn read_config_text(codex_home: &Path) -> io::Result<String> {
    tokio::fs::read_to_string(codex_home.join("config.toml")).await
}

async fn write_seed_marker(codex_home: &Path) -> io::Result<()> {
    let config_text = read_config_text(codex_home).await?;
    tokio::fs::write(
        codex_home.join(CLAUDEX_HOME_SEEDED_FILENAME),
        format!(
            "version=1\nsource_home=~/.codex\nconfig_sha1={}\n",
            sha1_hex(config_text.as_bytes())
        ),
    )
    .await
}

async fn write_legacy_claudex_config(codex_home: &Path) -> io::Result<ConfigToml> {
    tokio::fs::create_dir_all(codex_home).await?;
    tokio::fs::write(
        codex_home.join("config.toml"),
        concat!(
            "model = \"gpt-5.4\"\n",
            "model_provider = \"openai\"\n",
            "model_reasoning_effort = \"xhigh\"\n",
            "plan_mode_reasoning_effort = \"xhigh\"\n",
            "personality = \"pragmatic\"\n",
        ),
    )
    .await?;
    read_config_toml(codex_home).await
}

#[tokio::test]
async fn applies_to_legacy_copied_claudex_defaults() -> io::Result<()> {
    let temp = TempDir::new()?;
    let codex_home = temp.path().join(".claudex");
    let config_toml = write_legacy_claudex_config(&codex_home).await?;
    write_seed_marker(&codex_home).await?;

    let status = maybe_migrate_claudex_home(&codex_home).await?;

    assert_eq!(status, ClaudexHomeMigrationStatus::Applied);
    assert!(codex_home.join(CLAUDEX_HOME_MIGRATION_FILENAME).exists());

    let persisted = read_config_toml(&codex_home).await?;
    assert_eq!(persisted.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(
        persisted.model_provider.as_deref(),
        Some(crate::CLAUDE_CODE_PROVIDER_ID)
    );
    assert_eq!(persisted.agent_backend, Some(AgentBackend::ClaudeCode));
    assert_eq!(
        persisted.model_reasoning_effort,
        Some(ReasoningEffort::XHigh)
    );
    assert_eq!(
        persisted.plan_mode_reasoning_effort,
        Some(ReasoningEffort::XHigh)
    );
    assert_eq!(persisted.personality, config_toml.personality);
    let persisted_text = read_config_text(&codex_home).await?;
    assert!(persisted_text.contains("model_reasoning_effort = \"max\""));
    assert!(persisted_text.contains("plan_mode_reasoning_effort = \"max\""));
    Ok(())
}

#[tokio::test]
async fn skips_non_legacy_claudex_home_and_preserves_user_choice() -> io::Result<()> {
    let temp = TempDir::new()?;
    let codex_home = temp.path().join(".claudex");
    tokio::fs::create_dir_all(&codex_home).await?;
    tokio::fs::write(
        codex_home.join("config.toml"),
        concat!(
            "model = \"claude-sonnet-4-6\"\n",
            "model_provider = \"claude_code\"\n",
            "agent_backend = \"claude_code\"\n",
            "model_reasoning_effort = \"high\"\n",
        ),
    )
    .await?;
    write_seed_marker(&codex_home).await?;
    let config_toml = read_config_toml(&codex_home).await?;

    let status = maybe_migrate_claudex_home(&codex_home).await?;

    assert_eq!(status, ClaudexHomeMigrationStatus::SkippedNonLegacyDefaults);
    assert!(codex_home.join(CLAUDEX_HOME_MIGRATION_FILENAME).exists());

    let persisted = read_config_toml(&codex_home).await?;
    assert_eq!(persisted, config_toml);
    Ok(())
}

#[tokio::test]
async fn skips_non_claudex_homes() -> io::Result<()> {
    let temp = TempDir::new()?;
    let config_toml = write_legacy_claudex_config(temp.path()).await?;

    let status = maybe_migrate_claudex_home(temp.path()).await?;

    assert_eq!(status, ClaudexHomeMigrationStatus::SkippedNonClaudexHome);
    assert!(!temp.path().join(CLAUDEX_HOME_MIGRATION_FILENAME).exists());
    let persisted = read_config_toml(temp.path()).await?;
    assert_eq!(persisted, config_toml);
    Ok(())
}

#[tokio::test]
async fn skips_unseeded_claudex_homes() -> io::Result<()> {
    let temp = TempDir::new()?;
    let codex_home = temp.path().join(".claudex");
    let config_toml = write_legacy_claudex_config(&codex_home).await?;

    let status = maybe_migrate_claudex_home(&codex_home).await?;

    assert_eq!(status, ClaudexHomeMigrationStatus::SkippedNoSeedMarker);
    let persisted = read_config_toml(&codex_home).await?;
    assert_eq!(persisted, config_toml);
    Ok(())
}

#[tokio::test]
async fn skips_seeded_homes_when_config_was_changed_after_copy() -> io::Result<()> {
    let temp = TempDir::new()?;
    let codex_home = temp.path().join(".claudex");
    write_legacy_claudex_config(&codex_home).await?;
    write_seed_marker(&codex_home).await?;
    tokio::fs::write(
        codex_home.join("config.toml"),
        concat!(
            "model = \"gpt-5.4\"\n",
            "model_provider = \"openai\"\n",
            "model_reasoning_effort = \"medium\"\n",
            "plan_mode_reasoning_effort = \"xhigh\"\n",
        ),
    )
    .await?;

    let status = maybe_migrate_claudex_home(&codex_home).await?;

    assert_eq!(
        status,
        ClaudexHomeMigrationStatus::SkippedSeededConfigChanged
    );
    assert!(codex_home.join(CLAUDEX_HOME_MIGRATION_FILENAME).exists());
    let persisted_text = read_config_text(&codex_home).await?;
    assert!(persisted_text.contains("model_reasoning_effort = \"medium\""));
    assert!(!persisted_text.contains("claude-opus-4-6"));
    Ok(())
}
