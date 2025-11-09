use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::env;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use codex_app_server_protocol::AuthMode;

use crate::token_data::PlanType;
use crate::token_data::TokenData;
use crate::token_data::parse_id_token;

#[cfg(test)]
use crate::token_data::IdTokenInfo;
#[cfg(test)]
use crate::token_data::KnownPlan;
#[cfg(test)]
use chrono::DateTime;

mod storage;
pub use storage::AuthCredentialsStoreMode;
pub use storage::AuthDotJson;

use storage::AuthStorageBackend;
use storage::create_auth_storage;

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";

/// Responses API client id for the OAuth refresh flow.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Debug, Clone)]
pub struct CodexAuth {
    pub mode: AuthMode,
    pub(crate) api_key: Option<String>,
    auth_state: Arc<Mutex<Option<AuthDotJson>>>,
    storage: Option<Arc<dyn AuthStorageBackend>>, // None when sourced from env vars.
    client: reqwest::Client,
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode && self.api_key == other.api_key
    }
}

impl CodexAuth {
    pub async fn refresh_token(&self) -> io::Result<String> {
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| io::Error::other("cannot refresh token without persistent storage"))?;

        let token_data = self
            .get_current_token_data()
            .ok_or_else(|| io::Error::other("token data is not available."))?;
        let refresh_token = token_data.refresh_token.clone();

        let refresh_response = try_refresh_token(refresh_token, &self.client).await?;
        let updated = update_tokens(
            storage.as_ref(),
            refresh_response.id_token,
            refresh_response.access_token,
            refresh_response.refresh_token,
        )
        .await?;

        if let Ok(mut guard) = self.auth_state.lock() {
            *guard = Some(updated.clone());
        }

        let access = updated
            .tokens
            .map(|tokens| tokens.access_token)
            .ok_or_else(|| io::Error::other("token data is not available after refresh."))?;
        Ok(access)
    }

    pub fn from_codex_home(
        codex_home: &Path,
        mode: AuthCredentialsStoreMode,
    ) -> io::Result<Option<CodexAuth>> {
        load_auth(codex_home, mode, false)
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::from_api_key_with_client(api_key, crate::default_client::create_client())
    }

    fn from_api_key_with_client(api_key: &str, client: reqwest::Client) -> Self {
        Self {
            mode: AuthMode::ApiKey,
            api_key: Some(api_key.to_owned()),
            auth_state: Arc::new(Mutex::new(None)),
            storage: None,
            client,
        }
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let auth_dot_json = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: Default::default(),
                access_token: "Access Token".to_string(),
                refresh_token: "test".to_string(),
                account_id: Some("account_id".to_string()),
            }),
            last_refresh: Some(Utc::now()),
        };

        Self {
            mode: AuthMode::ChatGPT,
            api_key: None,
            auth_state: Arc::new(Mutex::new(Some(auth_dot_json))),
            storage: None,
            client: crate::default_client::create_client(),
        }
    }

    pub async fn get_token(&self) -> io::Result<String> {
        match self.mode {
            AuthMode::ApiKey => Ok(self.api_key.clone().unwrap_or_default()),
            AuthMode::ChatGPT => {
                let tokens = self.get_token_data().await?;
                Ok(tokens.access_token)
            }
        }
    }

    pub async fn get_token_data(&self) -> io::Result<TokenData> {
        let auth_state = self
            .auth_state
            .lock()
            .map_err(|_| io::Error::other("failed to lock auth state"))?
            .clone();
        match auth_state {
            Some(AuthDotJson {
                tokens: Some(tokens),
                last_refresh: Some(last_refresh),
                ..
            }) => {
                if last_refresh < Utc::now() - chrono::Duration::days(28) {
                    let refresh_token = tokens.refresh_token.clone();
                    let refresh_response = tokio::time::timeout(
                        Duration::from_secs(60),
                        try_refresh_token(refresh_token, &self.client),
                    )
                    .await
                    .map_err(|_| io::Error::other("timed out while refreshing OpenAI API key"))??;

                    let storage = self
                        .storage
                        .as_ref()
                        .ok_or_else(|| io::Error::other("cannot refresh token without storage"))?;
                    let updated_auth = update_tokens(
                        storage.as_ref(),
                        refresh_response.id_token,
                        refresh_response.access_token,
                        refresh_response.refresh_token,
                    )
                    .await?;

                    if let Some(tokens) = updated_auth.tokens.clone() {
                        if let Ok(mut guard) = self.auth_state.lock() {
                            *guard = Some(updated_auth);
                        }
                        return Ok(tokens);
                    }

                    return Err(io::Error::other(
                        "token data is not available after refresh.",
                    ));
                }
                Ok(tokens)
            }
            _ => Err(io::Error::other("Token data is not available.")),
        }
    }

    pub fn get_account_id(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.account_id)
    }

    pub fn get_account_email(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.id_token.email)
    }

    pub(crate) fn get_plan_type(&self) -> Option<PlanType> {
        self.get_current_token_data()
            .and_then(|t| t.id_token.chatgpt_plan_type)
    }

    fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        self.auth_state.lock().ok().and_then(|state| state.clone())
    }

    fn get_current_token_data(&self) -> Option<TokenData> {
        self.get_current_auth_json().and_then(|t| t.tokens)
    }

    fn from_storage(
        storage: Arc<dyn AuthStorageBackend>,
        client: reqwest::Client,
        auth_dot_json: AuthDotJson,
    ) -> Self {
        Self {
            mode: AuthMode::ChatGPT,
            api_key: None,
            auth_state: Arc::new(Mutex::new(Some(auth_dot_json))),
            storage: Some(storage),
            client,
        }
    }
}

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    env::var(CODEX_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn get_auth_file(codex_home: &Path) -> PathBuf {
    storage::get_auth_file(codex_home)
}

pub fn try_read_auth_json(auth_file: &Path) -> io::Result<AuthDotJson> {
    let mut file = File::open(auth_file)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let auth_dot_json: AuthDotJson = serde_json::from_str(&contents)?;
    Ok(auth_dot_json)
}

pub fn write_auth_json(auth_file: &Path, auth_dot_json: &AuthDotJson) -> io::Result<()> {
    if let Some(parent) = auth_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json_data = serde_json::to_string_pretty(auth_dot_json)?;
    let mut options = OpenOptions::new();
    options.truncate(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(auth_file)?;
    file.write_all(json_data.as_bytes())?;
    file.flush()?;
    Ok(())
}

pub fn login_with_api_key(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    api_key: &str,
) -> io::Result<()> {
    let storage = create_auth_storage(codex_home.to_path_buf(), mode);
    let auth_dot_json = AuthDotJson {
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
    };
    storage.save(&auth_dot_json)
}

pub fn logout(codex_home: &Path, mode: AuthCredentialsStoreMode) -> io::Result<bool> {
    let storage = create_auth_storage(codex_home.to_path_buf(), mode);
    storage.delete()
}

fn load_auth(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    enable_codex_api_key_env: bool,
) -> io::Result<Option<CodexAuth>> {
    if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
        let client = crate::default_client::create_client();
        return Ok(Some(CodexAuth::from_api_key_with_client(
            api_key.as_str(),
            client,
        )));
    }

    let storage = create_auth_storage(codex_home.to_path_buf(), mode);
    let client = crate::default_client::create_client();
    let auth_dot_json = match storage.load()? {
        Some(auth) => auth,
        None => {
            if let Some(api_key) = read_openai_api_key_from_env() {
                return Ok(Some(CodexAuth::from_api_key_with_client(
                    api_key.as_str(),
                    client,
                )));
            }
            return Ok(None);
        }
    };

    if let Some(api_key) = auth_dot_json.openai_api_key.as_ref() {
        return Ok(Some(CodexAuth::from_api_key_with_client(api_key, client)));
    }

    Ok(Some(CodexAuth::from_storage(
        storage,
        client,
        AuthDotJson {
            openai_api_key: None,
            tokens: auth_dot_json.tokens,
            last_refresh: auth_dot_json.last_refresh,
        },
    )))
}

async fn update_tokens(
    storage: &dyn AuthStorageBackend,
    id_token: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
) -> io::Result<AuthDotJson> {
    let mut auth_dot_json = storage
        .load()?
        .ok_or_else(|| io::Error::other("auth storage missing during refresh"))?;

    let tokens = auth_dot_json.tokens.get_or_insert_with(TokenData::default);
    tokens.id_token = parse_id_token(&id_token).map_err(io::Error::other)?;
    if let Some(access_token) = access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth_dot_json.last_refresh = Some(Utc::now());
    storage.save(&auth_dot_json)?;
    Ok(auth_dot_json)
}

async fn try_refresh_token(
    refresh_token: String,
    client: &reqwest::Client,
) -> io::Result<RefreshResponse> {
    let refresh_request = RefreshRequest {
        client_id: CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
        scope: "openid profile email",
    };

    let response = client
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/json")
        .json(&refresh_request)
        .send()
        .await
        .map_err(io::Error::other)?;

    if response.status().is_success() {
        let refresh_response = response
            .json::<RefreshResponse>()
            .await
            .map_err(io::Error::other)?;
        Ok(refresh_response)
    } else {
        Err(io::Error::other(format!(
            "Failed to refresh token: {}",
            response.status()
        )))
    }
}

#[derive(Serialize)]
struct RefreshRequest {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: String,
    scope: &'static str,
}

#[derive(Deserialize, Clone)]
struct RefreshResponse {
    id_token: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Clone, Debug)]
struct CachedAuth {
    auth: Option<CodexAuth>,
}

#[derive(Debug)]
pub struct AuthManager {
    codex_home: PathBuf,
    inner: RwLock<CachedAuth>,
    enable_codex_api_key_env: bool,
    store_mode: AuthCredentialsStoreMode,
}

impl AuthManager {
    pub fn new(
        codex_home: PathBuf,
        store_mode: AuthCredentialsStoreMode,
        enable_codex_api_key_env: bool,
    ) -> Self {
        let auth = load_auth(&codex_home, store_mode, enable_codex_api_key_env)
            .ok()
            .flatten();
        Self {
            codex_home,
            inner: RwLock::new(CachedAuth { auth }),
            enable_codex_api_key_env,
            store_mode,
        }
    }

    pub fn shared(
        codex_home: PathBuf,
        store_mode: AuthCredentialsStoreMode,
        enable_codex_api_key_env: bool,
    ) -> Arc<Self> {
        Arc::new(Self::new(codex_home, store_mode, enable_codex_api_key_env))
    }

    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        let cached = CachedAuth { auth: Some(auth) };
        Arc::new(Self {
            codex_home: PathBuf::new(),
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            store_mode: AuthCredentialsStoreMode::File,
        })
    }

    pub fn auth(&self) -> Option<CodexAuth> {
        self.inner.read().ok().and_then(|c| c.auth.clone())
    }

    pub fn reload(&self) -> bool {
        let new_auth = load_auth(
            &self.codex_home,
            self.store_mode,
            self.enable_codex_api_key_env,
        )
        .ok()
        .flatten();
        if let Ok(mut guard) = self.inner.write() {
            let changed = !AuthManager::auths_equal(&guard.auth, &new_auth);
            guard.auth = new_auth;
            changed
        } else {
            false
        }
    }

    fn auths_equal(a: &Option<CodexAuth>, b: &Option<CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    pub async fn refresh_token(&self) -> io::Result<Option<String>> {
        let auth = match self.auth() {
            Some(a) => a,
            None => return Ok(None),
        };
        match auth.refresh_token().await {
            Ok(token) => {
                self.reload();
                Ok(Some(token))
            }
            Err(e) => Err(e),
        }
    }

    pub fn logout(&self) -> io::Result<bool> {
        let storage = create_auth_storage(self.codex_home.clone(), self.store_mode);
        let removed = storage.delete()?;
        self.reload();
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::tempdir;

    const LAST_REFRESH: &str = "2025-08-06T20:41:36.232376Z";

    #[tokio::test]
    async fn roundtrip_auth_dot_json() {
        let codex_home = tempdir().unwrap();
        let storage = create_auth_storage(
            codex_home.path().to_path_buf(),
            AuthCredentialsStoreMode::File,
        );
        use crate::test_helpers::fixtures::make_test_jwt;
        let fake_jwt = make_test_jwt(Some("user@example.com"), Some("pro"));

        let auth_dot_json = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("user@example.com".to_string()),
                    chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Pro)),
                    raw_jwt: fake_jwt,
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: None,
            }),
            last_refresh: Some(Utc::now()),
        };
        storage.save(&auth_dot_json).unwrap();
        let loaded = storage.load().unwrap();
        assert_eq!(loaded, Some(auth_dot_json));
    }

    #[test]
    fn login_with_api_key_overwrites_existing_auth_json() {
        let dir = tempdir().unwrap();
        let auth_path = dir.path().join("auth.json");
        let stale_auth = json!({
            "OPENAI_API_KEY": "sk-old",
            "tokens": {
                "id_token": "stale.header.payload",
                "access_token": "stale-access",
                "refresh_token": "stale-refresh",
                "account_id": "stale-acc"
            }
        });
        std::fs::write(
            &auth_path,
            serde_json::to_string_pretty(&stale_auth).unwrap(),
        )
        .unwrap();

        super::login_with_api_key(dir.path(), AuthCredentialsStoreMode::File, "sk-new")
            .expect("login_with_api_key should succeed");

        let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
        let auth = storage.load().expect("auth storage should parse");
        assert_eq!(
            auth.and_then(|json| json.openai_api_key),
            Some("sk-new".to_string())
        );
    }

    #[tokio::test]
    async fn loads_api_key_from_auth() {
        let dir = tempdir().unwrap();
        let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
        let auth_dot_json = AuthDotJson {
            openai_api_key: Some("sk-test-key".to_string()),
            tokens: None,
            last_refresh: None,
        };
        storage.save(&auth_dot_json).unwrap();

        let auth = super::load_auth(dir.path(), AuthCredentialsStoreMode::File, false)
            .unwrap()
            .unwrap();
        assert_eq!(auth.mode, AuthMode::ApiKey);
        assert_eq!(auth.api_key, Some("sk-test-key".to_string()));
        assert!(auth.get_token_data().await.is_err());
    }

    #[test]
    fn logout_removes_auth_file() -> Result<(), io::Error> {
        let dir = tempdir()?;
        let auth_dot_json = AuthDotJson {
            openai_api_key: Some("sk-test-key".to_string()),
            tokens: None,
            last_refresh: None,
        };
        let storage = create_auth_storage(dir.path().to_path_buf(), AuthCredentialsStoreMode::File);
        storage.save(&auth_dot_json)?;
        assert!(dir.path().join("auth.json").exists());
        let removed = super::logout(dir.path(), AuthCredentialsStoreMode::File)?;
        assert!(removed);
        assert!(!dir.path().join("auth.json").exists());
        Ok(())
    }

    struct AuthFileParams {
        openai_api_key: Option<String>,
        chatgpt_plan_type: String,
    }

    fn write_auth_file(params: AuthFileParams, codex_home: &Path) -> io::Result<String> {
        let storage = create_auth_storage(codex_home.to_path_buf(), AuthCredentialsStoreMode::File);
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }
        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": "user@example.com",
            "email_verified": true,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "bc3618e3-489d-4d49-9362-1561dc53ba53",
                "chatgpt_plan_type": params.chatgpt_plan_type,
                "chatgpt_user_id": "user-12345",
                "user_id": "user-12345",
            }
        });
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let header_b64 = b64(&serde_json::to_vec(&header)?);
        let payload_b64 = b64(&serde_json::to_vec(&payload)?);
        let signature_b64 = b64(b"sig");
        let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

        let auth_json_data = json!({
            "OPENAI_API_KEY": params.openai_api_key,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": "test-access-token",
                "refresh_token": "test-refresh-token"
            },
            "last_refresh": LAST_REFRESH,
        });
        let auth_dot_json: AuthDotJson = serde_json::from_value(auth_json_data)?;
        storage.save(&auth_dot_json)?;
        Ok(fake_jwt)
    }

    #[tokio::test]
    async fn pro_account_with_no_api_key_uses_chatgpt_auth() {
        let codex_home = tempdir().unwrap();
        let fake_jwt = write_auth_file(
            AuthFileParams {
                openai_api_key: None,
                chatgpt_plan_type: "pro".to_string(),
            },
            codex_home.path(),
        )
        .expect("failed to write auth file");

        let CodexAuth {
            api_key,
            mode,
            auth_state,
            storage: _,
            ..
        } = super::load_auth(codex_home.path(), AuthCredentialsStoreMode::File, false)
            .unwrap()
            .unwrap();
        assert_eq!(None, api_key);
        assert_eq!(AuthMode::ChatGPT, mode);

        let guard = auth_state.lock().unwrap();
        let auth_dot_json = guard.as_ref().expect("AuthDotJson should exist");
        assert_eq!(
            &AuthDotJson {
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token: IdTokenInfo {
                        email: Some("user@example.com".to_string()),
                        chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Pro)),
                        raw_jwt: fake_jwt,
                    },
                    access_token: "test-access-token".to_string(),
                    refresh_token: "test-refresh-token".to_string(),
                    account_id: None,
                }),
                last_refresh: Some(
                    DateTime::parse_from_rfc3339(LAST_REFRESH)
                        .unwrap()
                        .with_timezone(&Utc)
                ),
            },
            auth_dot_json
        )
    }
}
