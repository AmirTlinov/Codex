#![cfg(not(debug_assertions))]

use crate::distribution::DistributionInfo;
use crate::distribution::UpdateChannel;
use crate::update_action;
use crate::update_action::UpdateAction;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_core::config::Config;
use codex_core::default_client::create_client;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

const BRANCH_VERSION_SHA_LEN: usize = 12;

pub fn get_upgrade_version(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup {
        return None;
    }

    let distribution = DistributionInfo::current();
    let update_source_key = distribution.update_source_key();
    let version_file = version_filepath(config);
    let info = read_version_info(&version_file)
        .ok()
        .filter(|info| info.update_source_key.as_deref() == Some(update_source_key.as_str()));

    if match &info {
        None => true,
        Some(info) => info.last_checked_at < Utc::now() - Duration::hours(20),
    } {
        // Refresh the cached latest version in the background so TUI startup
        // isn’t blocked by a network call. The UI reads the previously cached
        // value (if any) for this run; the next run shows the banner if needed.
        let version_file = version_file.clone();
        let distribution = distribution.clone();
        tokio::spawn(async move {
            check_for_update(&version_file, &distribution)
                .await
                .inspect_err(|e| tracing::error!("Failed to update version: {e}"))
        });
    }

    info.and_then(|info| match &distribution.update_channel {
        UpdateChannel::Disabled => None,
        UpdateChannel::GithubBranch { .. } => {
            (info.latest_version != distribution.display_version).then_some(info.latest_version)
        }
        UpdateChannel::GithubRelease { .. } => {
            is_newer(&info.latest_version, &distribution.display_version)
                .unwrap_or(false)
                .then_some(info.latest_version)
        }
    })
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct VersionInfo {
    latest_version: String,
    // ISO-8601 timestamp (RFC3339)
    last_checked_at: DateTime<Utc>,
    #[serde(default)]
    dismissed_version: Option<String>,
    #[serde(default)]
    update_source_key: Option<String>,
}

const VERSION_FILENAME: &str = "version.json";
// We use the latest version from the cask if installation is via homebrew -
// homebrew does not immediately pick up the latest release and can lag behind.
const HOMEBREW_CASK_API_URL: &str = "https://formulae.brew.sh/api/cask/codex.json";

#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

#[derive(Deserialize, Debug, Clone)]
struct BranchCommitInfo {
    sha: String,
}

#[derive(Deserialize, Debug, Clone)]
struct HomebrewCaskInfo {
    version: String,
}

fn version_filepath(config: &Config) -> PathBuf {
    config.codex_home.join(VERSION_FILENAME)
}

fn read_version_info(version_file: &Path) -> anyhow::Result<VersionInfo> {
    let contents = std::fs::read_to_string(version_file)?;
    Ok(serde_json::from_str(&contents)?)
}

async fn check_for_update(
    version_file: &Path,
    distribution: &DistributionInfo,
) -> anyhow::Result<()> {
    let latest_version = if should_use_homebrew_cask(distribution) {
        let HomebrewCaskInfo { version } = create_client()
            .get(HOMEBREW_CASK_API_URL)
            .send()
            .await?
            .error_for_status()?
            .json::<HomebrewCaskInfo>()
            .await?;
        version
    } else {
        fetch_latest_version(distribution).await?
    };

    let update_source_key = distribution.update_source_key();
    let prev_info = read_version_info(version_file)
        .ok()
        .filter(|info| info.update_source_key.as_deref() == Some(update_source_key.as_str()));
    let info = VersionInfo {
        latest_version,
        last_checked_at: Utc::now(),
        dismissed_version: prev_info.and_then(|p| p.dismissed_version),
        update_source_key: Some(update_source_key),
    };

    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

fn should_use_homebrew_cask(distribution: &DistributionInfo) -> bool {
    matches!(
        update_action::get_update_action(),
        Some(UpdateAction::BrewUpgrade)
    ) && !distribution.uses_custom_branding()
}

async fn fetch_latest_version(distribution: &DistributionInfo) -> anyhow::Result<String> {
    match &distribution.update_channel {
        UpdateChannel::Disabled => {
            anyhow::bail!("update channel is disabled")
        }
        UpdateChannel::GithubRelease {
            api_url,
            tag_prefix,
        } => {
            let ReleaseInfo { tag_name } = create_client()
                .get(api_url)
                .send()
                .await?
                .error_for_status()?
                .json::<ReleaseInfo>()
                .await?;
            extract_version_from_latest_tag(&tag_name, tag_prefix)
        }
        UpdateChannel::GithubBranch { api_url } => {
            let BranchCommitInfo { sha } = create_client()
                .get(api_url)
                .send()
                .await?
                .error_for_status()?
                .json::<BranchCommitInfo>()
                .await?;
            shorten_commit_sha(&sha)
        }
    }
}

fn shorten_commit_sha(sha: &str) -> anyhow::Result<String> {
    let trimmed = sha.trim();
    if trimmed.len() < BRANCH_VERSION_SHA_LEN {
        anyhow::bail!("Failed to parse commit sha '{trimmed}'")
    }
    Ok(trimmed[..BRANCH_VERSION_SHA_LEN].to_string())
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => Some(l > c),
        _ => None,
    }
}

fn extract_version_from_latest_tag(
    latest_tag_name: &str,
    tag_prefix: &str,
) -> anyhow::Result<String> {
    latest_tag_name
        .strip_prefix(tag_prefix)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse latest tag name '{latest_tag_name}'"))
}

/// Returns the latest version to show in a popup, if it should be shown.
/// This respects the user's dismissal choice for the current latest version.
pub fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup {
        return None;
    }

    let distribution = DistributionInfo::current();
    let version_file = version_filepath(config);
    let latest = get_upgrade_version(config)?;
    // If the user dismissed this exact version previously, do not show the popup.
    if let Ok(info) = read_version_info(&version_file)
        && info.update_source_key.as_deref() == Some(distribution.update_source_key().as_str())
        && info.dismissed_version.as_deref() == Some(latest.as_str())
    {
        return None;
    }
    Some(latest)
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub async fn dismiss_version(config: &Config, version: &str) -> anyhow::Result<()> {
    let distribution = DistributionInfo::current();
    let version_file = version_filepath(config);
    let mut info = match read_version_info(&version_file) {
        Ok(info)
            if info.update_source_key.as_deref()
                == Some(distribution.update_source_key().as_str()) =>
        {
            info
        }
        _ => return Ok(()),
    };
    info.dismissed_version = Some(version.to_string());
    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut iter = v.trim().split('.');
    let maj = iter.next()?.parse::<u64>().ok()?;
    let min = iter.next()?.parse::<u64>().ok()?;
    let pat = iter.next()?.parse::<u64>().ok()?;
    Some((maj, min, pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_from_brew_api_json() {
        //
        // https://formulae.brew.sh/api/cask/codex.json
        let cask_json = r#"{
            "token": "codex",
            "full_token": "codex",
            "tap": "homebrew/cask",
            "version": "0.96.0",
        }"#;
        let HomebrewCaskInfo { version } = serde_json::from_str::<HomebrewCaskInfo>(cask_json)
            .expect("failed to parse version from cask json");
        assert_eq!(version, "0.96.0");
    }

    #[test]
    fn extracts_version_from_latest_tag() {
        assert_eq!(
            extract_version_from_latest_tag("rust-v1.5.0", "rust-v")
                .expect("failed to parse version"),
            "1.5.0"
        );
    }

    #[test]
    fn latest_tag_without_prefix_is_invalid() {
        assert!(extract_version_from_latest_tag("v1.5.0", "rust-v").is_err());
    }

    #[test]
    fn shortens_branch_commit_sha_for_claudex_updates() {
        assert_eq!(
            shorten_commit_sha("1234567890abcdef1234567890abcdef12345678")
                .expect("failed to shorten sha"),
            "1234567890ab"
        );
    }

    #[test]
    fn prerelease_version_is_not_considered_newer() {
        assert_eq!(is_newer("0.11.0-beta.1", "0.11.0"), None);
        assert_eq!(is_newer("1.0.0-rc.1", "1.0.0"), None);
    }

    #[test]
    fn plain_semver_comparisons_work() {
        assert_eq!(is_newer("0.11.1", "0.11.0"), Some(true));
        assert_eq!(is_newer("0.11.0", "0.11.1"), Some(false));
        assert_eq!(is_newer("1.0.0", "0.9.9"), Some(true));
        assert_eq!(is_newer("0.9.9", "1.0.0"), Some(false));
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(parse_version(" 1.2.3 \n"), Some((1, 2, 3)));
        assert_eq!(is_newer(" 1.2.3 ", "1.2.2"), Some(true));
    }
}
