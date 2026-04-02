use crate::version::CODEX_CLI_VERSION;
use std::sync::LazyLock;
use url::Url;

const DEFAULT_PRODUCT_NAME: &str = "OpenAI Codex";
const DEFAULT_SHORT_PRODUCT_NAME: &str = "Codex";
const DEFAULT_INSTALL_URL: &str = "https://github.com/openai/codex";
const DEFAULT_RELEASE_NOTES_URL: &str = "https://github.com/openai/codex/releases/latest";
const DEFAULT_ANNOUNCEMENT_TIP_URL: &str =
    "https://raw.githubusercontent.com/openai/codex/main/announcement_tip.toml";
const DEFAULT_RELEASE_API_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest";
pub(crate) const DEFAULT_RELEASE_TAG_PREFIX: &str = "rust-v";

const ENV_PRODUCT_NAME: &str = "CODEX_DIST_PRODUCT_NAME";
const ENV_VERSION: &str = "CODEX_DIST_VERSION";
const ENV_INSTALL_URL: &str = "CODEX_DIST_INSTALL_URL";
const ENV_RELEASE_NOTES_URL: &str = "CODEX_DIST_RELEASE_NOTES_URL";
const ENV_ANNOUNCEMENT_TIP_URL: &str = "CODEX_DIST_ANNOUNCEMENT_TIP_URL";
const ENV_UPDATE_KIND: &str = "CODEX_DIST_UPDATE_KIND";
const ENV_UPDATE_REPO: &str = "CODEX_DIST_UPDATE_REPO";
const ENV_UPDATE_BRANCH: &str = "CODEX_DIST_UPDATE_BRANCH";
const ENV_UPDATE_TAG_PREFIX: &str = "CODEX_DIST_UPDATE_TAG_PREFIX";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateChannel {
    Disabled,
    GithubRelease { api_url: String, tag_prefix: String },
    GithubBranch { api_url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DistributionInfo {
    pub(crate) product_name: String,
    pub(crate) display_version: String,
    pub(crate) install_url: String,
    pub(crate) release_notes_url: String,
    pub(crate) announcement_tip_url: Option<String>,
    pub(crate) update_channel: UpdateChannel,
}

static CURRENT_DISTRIBUTION: LazyLock<DistributionInfo> =
    LazyLock::new(|| DistributionInfo::from_env(|key| std::env::var(key).ok()));

impl DistributionInfo {
    pub(crate) fn current() -> &'static Self {
        &CURRENT_DISTRIBUTION
    }

    fn from_env<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let product_name =
            get_trimmed(&get, ENV_PRODUCT_NAME).unwrap_or_else(|| DEFAULT_PRODUCT_NAME.to_string());
        let display_version =
            get_trimmed(&get, ENV_VERSION).unwrap_or_else(|| CODEX_CLI_VERSION.to_string());
        let install_url =
            get_trimmed(&get, ENV_INSTALL_URL).unwrap_or_else(|| DEFAULT_INSTALL_URL.to_string());
        let release_notes_url = get_trimmed(&get, ENV_RELEASE_NOTES_URL)
            .unwrap_or_else(|| DEFAULT_RELEASE_NOTES_URL.to_string());
        let announcement_tip_url = get_optional_override(&get, ENV_ANNOUNCEMENT_TIP_URL)
            .unwrap_or_else(|| Some(DEFAULT_ANNOUNCEMENT_TIP_URL.to_string()));
        let update_channel = parse_update_channel(&get);

        Self {
            product_name,
            display_version,
            install_url,
            release_notes_url,
            announcement_tip_url,
            update_channel,
        }
    }

    pub(crate) fn short_product_name(&self) -> &str {
        if self.product_name == DEFAULT_PRODUCT_NAME {
            DEFAULT_SHORT_PRODUCT_NAME
        } else {
            self.product_name.as_str()
        }
    }

    pub(crate) fn formatted_version_label(&self) -> String {
        format_version_label(&self.display_version)
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    pub(crate) fn update_source_key(&self) -> String {
        match &self.update_channel {
            UpdateChannel::Disabled => "disabled".to_string(),
            UpdateChannel::GithubRelease {
                api_url,
                tag_prefix,
            } => {
                format!("github-release:{api_url}:{tag_prefix}")
            }
            UpdateChannel::GithubBranch { api_url } => format!("github-branch:{api_url}"),
        }
    }

    pub(crate) fn uses_custom_branding(&self) -> bool {
        self.product_name != DEFAULT_PRODUCT_NAME
            || self.display_version != CODEX_CLI_VERSION
            || self.install_url != DEFAULT_INSTALL_URL
            || self.release_notes_url != DEFAULT_RELEASE_NOTES_URL
            || self.announcement_tip_url.as_deref() != Some(DEFAULT_ANNOUNCEMENT_TIP_URL)
            || !matches!(
                &self.update_channel,
                UpdateChannel::GithubRelease { api_url, tag_prefix }
                    if api_url == DEFAULT_RELEASE_API_URL
                        && tag_prefix == DEFAULT_RELEASE_TAG_PREFIX
            )
    }
}

pub(crate) fn format_version_label(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with('v')
        || trimmed.starts_with('V')
        || !starts_with_semver_core(trimmed)
    {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn starts_with_semver_core(version: &str) -> bool {
    let mut segments = version.split('.');
    let first = segments.next().unwrap_or_default();
    let second = segments.next().unwrap_or_default();
    let third = segments.next().unwrap_or_default();
    !first.is_empty()
        && !second.is_empty()
        && !third.is_empty()
        && first.chars().all(|ch| ch.is_ascii_digit())
        && second.chars().all(|ch| ch.is_ascii_digit())
        && third.chars().take_while(char::is_ascii_digit).count() > 0
}

fn parse_update_channel<F>(get: &F) -> UpdateChannel
where
    F: Fn(&str) -> Option<String>,
{
    let kind = get_trimmed(get, ENV_UPDATE_KIND);
    let repo = get_trimmed(get, ENV_UPDATE_REPO);
    let branch = get_trimmed(get, ENV_UPDATE_BRANCH);
    let tag_prefix = get_trimmed(get, ENV_UPDATE_TAG_PREFIX)
        .unwrap_or_else(|| DEFAULT_RELEASE_TAG_PREFIX.to_string());

    match kind.as_deref() {
        Some("disabled") => UpdateChannel::Disabled,
        Some("github-branch") => repo
            .as_deref()
            .zip(branch.as_deref())
            .and_then(|(repo, branch)| build_branch_api_url(repo, branch))
            .map(|api_url| UpdateChannel::GithubBranch { api_url })
            .unwrap_or(UpdateChannel::Disabled),
        Some("github-release") => repo
            .as_deref()
            .and_then(build_release_api_url)
            .map(|api_url| UpdateChannel::GithubRelease {
                api_url,
                tag_prefix: tag_prefix.clone(),
            })
            .unwrap_or(UpdateChannel::Disabled),
        _ => repo
            .as_deref()
            .and_then(build_release_api_url)
            .map(|api_url| UpdateChannel::GithubRelease {
                api_url,
                tag_prefix: tag_prefix.clone(),
            })
            .unwrap_or_else(|| UpdateChannel::GithubRelease {
                api_url: DEFAULT_RELEASE_API_URL.to_string(),
                tag_prefix,
            }),
    }
}

fn build_release_api_url(repo_slug: &str) -> Option<String> {
    let (owner, repo) = split_repo_slug(repo_slug)?;
    let mut url = Url::parse("https://api.github.com").ok()?;
    url.path_segments_mut()
        .ok()?
        .extend(["repos", owner, repo, "releases", "latest"]);
    Some(url.to_string())
}

fn build_branch_api_url(repo_slug: &str, branch: &str) -> Option<String> {
    let (owner, repo) = split_repo_slug(repo_slug)?;
    let mut url = Url::parse("https://api.github.com").ok()?;
    url.path_segments_mut()
        .ok()?
        .extend(["repos", owner, repo, "commits", branch.trim()]);
    Some(url.to_string())
}

fn split_repo_slug(repo_slug: &str) -> Option<(&str, &str)> {
    let trimmed = repo_slug.trim();
    let (owner, repo) = trimmed.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        None
    } else {
        Some((owner, repo))
    }
}

fn get_trimmed<F>(get: &F, key: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    get(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn get_optional_override<F>(get: &F, key: &str) -> Option<Option<String>>
where
    F: Fn(&str) -> Option<String>,
{
    get(key).map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    fn from_pairs(pairs: &[(&str, &str)]) -> DistributionInfo {
        let env = BTreeMap::from_iter(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        DistributionInfo::from_env(|key| env.get(key).cloned())
    }

    #[test]
    fn defaults_match_openai_codex() {
        let info = from_pairs(&[]);
        assert_eq!(
            info,
            DistributionInfo {
                product_name: DEFAULT_PRODUCT_NAME.to_string(),
                display_version: CODEX_CLI_VERSION.to_string(),
                install_url: DEFAULT_INSTALL_URL.to_string(),
                release_notes_url: DEFAULT_RELEASE_NOTES_URL.to_string(),
                announcement_tip_url: Some(DEFAULT_ANNOUNCEMENT_TIP_URL.to_string()),
                update_channel: UpdateChannel::GithubRelease {
                    api_url: DEFAULT_RELEASE_API_URL.to_string(),
                    tag_prefix: DEFAULT_RELEASE_TAG_PREFIX.to_string(),
                },
            }
        );
    }

    #[test]
    fn branch_channel_supports_custom_claudex_distribution() {
        let info = from_pairs(&[
            (ENV_PRODUCT_NAME, "Claudex"),
            (ENV_VERSION, "2e845297cd"),
            (
                ENV_INSTALL_URL,
                "https://github.com/AmirTlinov/Codex/tree/amir/claude-reflective-agent",
            ),
            (
                ENV_RELEASE_NOTES_URL,
                "https://github.com/AmirTlinov/Codex/commits/amir/claude-reflective-agent",
            ),
            (ENV_ANNOUNCEMENT_TIP_URL, ""),
            (ENV_UPDATE_KIND, "github-branch"),
            (ENV_UPDATE_REPO, "AmirTlinov/Codex"),
            (ENV_UPDATE_BRANCH, "amir/claude-reflective-agent"),
        ]);

        assert_eq!(info.product_name, "Claudex");
        assert_eq!(info.display_version, "2e845297cd");
        assert_eq!(info.short_product_name(), "Claudex");
        assert_eq!(info.announcement_tip_url, None);
        assert_eq!(
            info.update_channel,
            UpdateChannel::GithubBranch {
                api_url: "https://api.github.com/repos/AmirTlinov/Codex/commits/amir%2Fclaude-reflective-agent".to_string(),
            }
        );
        assert_eq!(
            info.update_source_key(),
            "github-branch:https://api.github.com/repos/AmirTlinov/Codex/commits/amir%2Fclaude-reflective-agent"
        );
        assert!(info.uses_custom_branding());
    }

    #[test]
    fn format_version_label_only_prefixes_semver_versions() {
        assert_eq!(format_version_label("0.118.0"), "v0.118.0");
        assert_eq!(
            format_version_label("0.118.0-cldx+2e845297cd"),
            "v0.118.0-cldx+2e845297cd"
        );
        assert_eq!(format_version_label("2e845297cd"), "2e845297cd");
        assert_eq!(format_version_label("v0.118.0"), "v0.118.0");
    }
}
