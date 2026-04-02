#[cfg(not(debug_assertions))]
use crate::distribution::DistributionInfo;
#[cfg(any(not(debug_assertions), test))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(not(debug_assertions))]
const ENV_CUSTOM_UPDATE_COMMAND: &str = "CODEX_DIST_UPDATE_COMMAND";

/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update via `npm install -g @openai/codex@latest`.
    NpmGlobalLatest,
    /// Update via `bun install -g @openai/codex@latest`.
    BunGlobalLatest,
    /// Update via `brew upgrade codex`.
    BrewUpgrade,
    /// Update by rerunning the downstream `claudex` installer script.
    ClaudexInstaller { script_path: PathBuf },
}

impl UpdateAction {
    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(&self) -> (String, Vec<String>) {
        match self {
            UpdateAction::NpmGlobalLatest => (
                "npm".to_string(),
                vec!["install".into(), "-g".into(), "@openai/codex".into()],
            ),
            UpdateAction::BunGlobalLatest => (
                "bun".to_string(),
                vec!["install".into(), "-g".into(), "@openai/codex".into()],
            ),
            UpdateAction::BrewUpgrade => (
                "brew".to_string(),
                vec!["upgrade".into(), "--cask".into(), "codex".into()],
            ),
            UpdateAction::ClaudexInstaller { script_path } => {
                (script_path.display().to_string(), Vec::new())
            }
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(&self) -> String {
        if matches!(self, UpdateAction::ClaudexInstaller { .. }) {
            return "scripts/install-claudex.sh".to_string();
        }

        let (command, args) = self.command_args();
        shlex::try_join(std::iter::once(command.as_str()).chain(args.iter().map(String::as_str)))
            .unwrap_or_else(|_| format!("{command} {}", args.join(" ")))
    }
}

#[cfg(not(debug_assertions))]
pub(crate) fn get_update_action() -> Option<UpdateAction> {
    let distribution = DistributionInfo::current();
    let custom_update_command = std::env::var_os(ENV_CUSTOM_UPDATE_COMMAND).map(PathBuf::from);
    if let Some(action) = custom_update_action(
        distribution.uses_custom_branding(),
        custom_update_command.as_deref(),
    ) {
        return Some(action);
    }

    let exe = std::env::current_exe().unwrap_or_default();
    let managed_by_npm = std::env::var_os("CODEX_MANAGED_BY_NPM").is_some();
    let managed_by_bun = std::env::var_os("CODEX_MANAGED_BY_BUN").is_some();

    detect_update_action(
        cfg!(target_os = "macos"),
        &exe,
        managed_by_npm,
        managed_by_bun,
    )
}

#[cfg(any(not(debug_assertions), test))]
fn custom_update_action(
    uses_custom_branding: bool,
    update_command: Option<&Path>,
) -> Option<UpdateAction> {
    if !uses_custom_branding {
        return None;
    }

    let script_path = update_command?.to_path_buf();
    script_path
        .is_file()
        .then_some(UpdateAction::ClaudexInstaller { script_path })
}

#[cfg(any(not(debug_assertions), test))]
fn detect_update_action(
    is_macos: bool,
    current_exe: &std::path::Path,
    managed_by_npm: bool,
    managed_by_bun: bool,
) -> Option<UpdateAction> {
    if managed_by_npm {
        Some(UpdateAction::NpmGlobalLatest)
    } else if managed_by_bun {
        Some(UpdateAction::BunGlobalLatest)
    } else if is_macos
        && (current_exe.starts_with("/opt/homebrew") || current_exe.starts_with("/usr/local"))
    {
        Some(UpdateAction::BrewUpgrade)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn detects_update_action_without_env_mutation() {
        assert_eq!(
            detect_update_action(
                /*is_macos*/ false,
                std::path::Path::new("/any/path"),
                /*managed_by_npm*/ false,
                /*managed_by_bun*/ false
            ),
            None
        );
        assert_eq!(
            detect_update_action(
                /*is_macos*/ false,
                std::path::Path::new("/any/path"),
                /*managed_by_npm*/ true,
                /*managed_by_bun*/ false
            ),
            Some(UpdateAction::NpmGlobalLatest)
        );
        assert_eq!(
            detect_update_action(
                /*is_macos*/ false,
                std::path::Path::new("/any/path"),
                /*managed_by_npm*/ false,
                /*managed_by_bun*/ true
            ),
            Some(UpdateAction::BunGlobalLatest)
        );
        assert_eq!(
            detect_update_action(
                /*is_macos*/ true,
                std::path::Path::new("/opt/homebrew/bin/codex"),
                /*managed_by_npm*/ false,
                /*managed_by_bun*/ false
            ),
            Some(UpdateAction::BrewUpgrade)
        );
        assert_eq!(
            detect_update_action(
                /*is_macos*/ true,
                std::path::Path::new("/usr/local/bin/codex"),
                /*managed_by_npm*/ false,
                /*managed_by_bun*/ false
            ),
            Some(UpdateAction::BrewUpgrade)
        );
    }

    #[test]
    fn custom_claudex_update_action_takes_precedence() {
        let installer = NamedTempFile::new().expect("failed to create installer temp file");
        let installer_path = installer.path().to_path_buf();

        assert_eq!(
            custom_update_action(/*uses_custom_branding*/ true, Some(&installer_path)),
            Some(UpdateAction::ClaudexInstaller {
                script_path: installer_path,
            })
        );
        assert_eq!(
            custom_update_action(/*uses_custom_branding*/ false, Some(installer.path())),
            None
        );
    }
}
