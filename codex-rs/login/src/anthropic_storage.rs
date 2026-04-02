use chrono::DateTime;
use chrono::Utc;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::fmt::Debug;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::warn;

use crate::auth::AuthCredentialsStoreMode;

const ANTHROPIC_AUTH_FILE: &str = "anthropic-auth.json";
const KEYRING_SERVICE: &str = "Claudex Anthropic Auth";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicAuthMode {
    ApiKey,
    Oauth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AnthropicProfile {
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub organization_uuid: Option<String>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicOauthData {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub profile: AnthropicProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicAuthJson {
    pub auth_mode: AnthropicAuthMode,
    pub api_key: Option<String>,
    pub oauth: Option<AnthropicOauthData>,
    pub last_refresh: Option<DateTime<Utc>>,
}

pub(super) fn create_anthropic_storage(
    codex_home: PathBuf,
    store_mode: AuthCredentialsStoreMode,
) -> Arc<dyn AnthropicStorageBackend> {
    match store_mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAnthropicStorage::new(codex_home)),
        AuthCredentialsStoreMode::Keyring => Arc::new(KeyringAnthropicStorage::new(
            codex_home,
            default_keyring_store(),
        )),
        AuthCredentialsStoreMode::Auto => Arc::new(AutoAnthropicStorage::new(
            codex_home,
            default_keyring_store(),
        )),
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAnthropicStorage::default()),
    }
}

pub(super) fn anthropic_auth_file(codex_home: &Path) -> PathBuf {
    codex_home.join(ANTHROPIC_AUTH_FILE)
}

pub(super) trait AnthropicStorageBackend: Debug + Send + Sync {
    fn load(&self) -> std::io::Result<Option<AnthropicAuthJson>>;
    fn save(&self, auth: &AnthropicAuthJson) -> std::io::Result<()>;
    fn delete(&self) -> std::io::Result<bool>;
}

#[derive(Clone, Debug)]
struct FileAnthropicStorage {
    codex_home: PathBuf,
}

impl FileAnthropicStorage {
    fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn try_read(&self, path: &Path) -> std::io::Result<AnthropicAuthJson> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        serde_json::from_str(&contents).map_err(std::io::Error::other)
    }
}

impl AnthropicStorageBackend for FileAnthropicStorage {
    fn load(&self) -> std::io::Result<Option<AnthropicAuthJson>> {
        let path = anthropic_auth_file(&self.codex_home);
        match self.try_read(&path) {
            Ok(auth) => Ok(Some(auth)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn save(&self, auth: &AnthropicAuthJson) -> std::io::Result<()> {
        let path = anthropic_auth_file(&self.codex_home);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(auth).map_err(std::io::Error::other)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let path = anthropic_auth_file(&self.codex_home);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
}

#[derive(Clone, Debug)]
struct KeyringAnthropicStorage {
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
}

impl KeyringAnthropicStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            codex_home,
            keyring_store,
        }
    }

    fn load_from_keyring(&self, key: &str) -> std::io::Result<Option<AnthropicAuthJson>> {
        match self.keyring_store.load(KEYRING_SERVICE, key) {
            Ok(Some(serialized)) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                std::io::Error::other(format!(
                    "failed to deserialize Anthropic auth from keyring: {err}"
                ))
            }),
            Ok(None) => Ok(None),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to load Anthropic auth from keyring: {}",
                error.message()
            ))),
        }
    }

    fn save_to_keyring(&self, key: &str, value: &str) -> std::io::Result<()> {
        match self.keyring_store.save(KEYRING_SERVICE, key, value) {
            Ok(()) => Ok(()),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to save Anthropic auth to keyring: {}",
                error.message()
            ))),
        }
    }
}

impl AnthropicStorageBackend for KeyringAnthropicStorage {
    fn load(&self) -> std::io::Result<Option<AnthropicAuthJson>> {
        let key = compute_store_key(&self.codex_home)?;
        self.load_from_keyring(&key)
    }

    fn save(&self, auth: &AnthropicAuthJson) -> std::io::Result<()> {
        let key = compute_store_key(&self.codex_home)?;
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.save_to_keyring(&key, &serialized)?;
        if let Err(err) = FileAnthropicStorage::new(self.codex_home.clone()).delete() {
            warn!("failed to remove Anthropic auth fallback file: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let key = compute_store_key(&self.codex_home)?;
        let keyring_removed = self
            .keyring_store
            .delete(KEYRING_SERVICE, &key)
            .map_err(|err| {
                std::io::Error::other(format!(
                    "failed to delete Anthropic auth from keyring: {err}"
                ))
            })?;
        let file_removed = FileAnthropicStorage::new(self.codex_home.clone()).delete()?;
        Ok(keyring_removed || file_removed)
    }
}

#[derive(Clone, Debug)]
struct AutoAnthropicStorage {
    keyring_storage: Arc<KeyringAnthropicStorage>,
    file_storage: Arc<FileAnthropicStorage>,
}

impl AutoAnthropicStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            keyring_storage: Arc::new(KeyringAnthropicStorage::new(
                codex_home.clone(),
                keyring_store,
            )),
            file_storage: Arc::new(FileAnthropicStorage::new(codex_home)),
        }
    }
}

impl AnthropicStorageBackend for AutoAnthropicStorage {
    fn load(&self) -> std::io::Result<Option<AnthropicAuthJson>> {
        match self.keyring_storage.load() {
            Ok(Some(auth)) => Ok(Some(auth)),
            Ok(None) => self.file_storage.load(),
            Err(err) => {
                warn!(
                    "failed to load Anthropic auth from keyring, falling back to file storage: {err}"
                );
                self.file_storage.load()
            }
        }
    }

    fn save(&self, auth: &AnthropicAuthJson) -> std::io::Result<()> {
        match self.keyring_storage.save(auth) {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    "failed to save Anthropic auth to keyring, falling back to file storage: {err}"
                );
                self.file_storage.save(auth)
            }
        }
    }

    fn delete(&self) -> std::io::Result<bool> {
        let keyring_removed = self.keyring_storage.delete()?;
        let file_removed = self.file_storage.delete()?;
        Ok(keyring_removed || file_removed)
    }
}

#[derive(Debug, Default)]
struct EphemeralAnthropicStorage {
    auth: Mutex<Option<AnthropicAuthJson>>,
}

impl AnthropicStorageBackend for EphemeralAnthropicStorage {
    fn load(&self) -> std::io::Result<Option<AnthropicAuthJson>> {
        Ok(self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    fn save(&self, auth: &AnthropicAuthJson) -> std::io::Result<()> {
        *self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(auth.clone());
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let removed = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some();
        Ok(removed)
    }
}

fn compute_store_key(codex_home: &Path) -> std::io::Result<String> {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("anthropic|{truncated}"))
}

fn default_keyring_store() -> Arc<dyn KeyringStore> {
    static STORE: Lazy<Arc<dyn KeyringStore>> =
        Lazy::new(|| Arc::new(DefaultKeyringStore) as Arc<dyn KeyringStore>);
    Arc::clone(&STORE)
}
