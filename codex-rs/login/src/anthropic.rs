use std::collections::HashMap;
use std::io;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use rand::RngCore;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use tiny_http::Header;
use tiny_http::Request;
use tiny_http::Response;
use tiny_http::Server;
use tiny_http::StatusCode as TinyStatusCode;
use tracing::warn;

use crate::anthropic_storage::AnthropicAuthJson;
use crate::anthropic_storage::AnthropicAuthMode;
use crate::anthropic_storage::AnthropicOauthData;
use crate::anthropic_storage::AnthropicProfile;
use crate::anthropic_storage::AnthropicStorageBackend;
use crate::anthropic_storage::create_anthropic_storage;
use crate::auth::AuthCredentialsStoreMode;
use crate::pkce::PkceCodes;
use crate::pkce::generate_pkce;
use codex_client::build_reqwest_client_with_custom_ca;
use codex_utils_template::Template;
use once_cell::sync::Lazy;
use url::Url;

const ANTHROPIC_DEFAULT_PORT: u16 = 4545;
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE_URL_OVERRIDE_ENV_VAR: &str = "CLAUDEX_ANTHROPIC_AUTHORIZE_URL";
const ANTHROPIC_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CLAUDEX_ANTHROPIC_TOKEN_URL";
const ANTHROPIC_PROFILE_URL_OVERRIDE_ENV_VAR: &str = "CLAUDEX_ANTHROPIC_PROFILE_URL";
const ANTHROPIC_CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CLAUDEX_ANTHROPIC_CLIENT_ID";
const ANTHROPIC_SCOPES: &[&str] = &[
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];
const REFRESH_SKEW_SECONDS: i64 = 60;

static LOGIN_SUCCESS_TEMPLATE: Lazy<Template> = Lazy::new(|| {
    Template::parse(include_str!("assets/success.html"))
        .unwrap_or_else(|err| panic!("Anthropic success page template must parse: {err}"))
});
static LOGIN_ERROR_TEMPLATE: Lazy<Template> = Lazy::new(|| {
    Template::parse(include_str!("assets/error.html"))
        .unwrap_or_else(|err| panic!("Anthropic error page template must parse: {err}"))
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicRuntimeAuth {
    ApiKey(String),
    OauthAccessToken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicAccountDisplay {
    pub auth_mode: AnthropicAuthMode,
    pub email: Option<String>,
    pub subscription_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnthropicLoginServerOptions {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub port: u16,
    pub open_browser: bool,
    pub force_state: Option<String>,
}

impl AnthropicLoginServerOptions {
    pub fn new(codex_home: PathBuf, auth_credentials_store_mode: AuthCredentialsStoreMode) -> Self {
        Self {
            codex_home,
            auth_credentials_store_mode,
            port: ANTHROPIC_DEFAULT_PORT,
            open_browser: true,
            force_state: None,
        }
    }
}

pub struct AnthropicLoginServer {
    pub auth_url: String,
    pub actual_port: u16,
    server_handle: tokio::task::JoinHandle<io::Result<()>>,
    shutdown_handle: AnthropicShutdownHandle,
}

impl AnthropicLoginServer {
    pub async fn block_until_done(self) -> io::Result<()> {
        self.server_handle.await.map_err(|err| {
            io::Error::other(format!("Anthropic login server task failed: {err:?}"))
        })?
    }

    pub fn cancel(&self) {
        self.shutdown_handle.shutdown();
    }

    pub fn cancel_handle(&self) -> AnthropicShutdownHandle {
        self.shutdown_handle.clone()
    }
}

#[derive(Clone, Debug)]
pub struct AnthropicShutdownHandle {
    shutdown_notify: Arc<tokio::sync::Notify>,
}

impl AnthropicShutdownHandle {
    pub fn shutdown(&self) {
        self.shutdown_notify.notify_waiters();
    }
}

pub fn login_with_anthropic_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    let auth = AnthropicAuthJson {
        auth_mode: AnthropicAuthMode::ApiKey,
        api_key: Some(api_key.to_string()),
        oauth: None,
        last_refresh: Some(Utc::now()),
    };
    create_anthropic_storage(codex_home.to_path_buf(), auth_credentials_store_mode).save(&auth)
}

pub fn load_anthropic_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AnthropicAuthJson>> {
    create_anthropic_storage(codex_home.to_path_buf(), auth_credentials_store_mode).load()
}

pub fn logout_anthropic(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<bool> {
    create_anthropic_storage(codex_home.to_path_buf(), auth_credentials_store_mode).delete()
}

pub async fn resolve_anthropic_account_display(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AnthropicAccountDisplay>> {
    let auth = load_anthropic_auth(codex_home, auth_credentials_store_mode)?;
    Ok(auth.and_then(account_display_from_auth))
}

pub async fn resolve_anthropic_account_display_after_refresh(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AnthropicAccountDisplay>> {
    let auth = ensure_anthropic_auth_fresh(codex_home, auth_credentials_store_mode).await?;
    Ok(auth.and_then(account_display_from_auth))
}

pub async fn resolve_anthropic_runtime_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AnthropicRuntimeAuth>> {
    let auth = ensure_anthropic_auth_fresh(codex_home, auth_credentials_store_mode).await?;
    Ok(match auth {
        Some(AnthropicAuthJson {
            auth_mode: AnthropicAuthMode::ApiKey,
            api_key: Some(api_key),
            ..
        }) => Some(AnthropicRuntimeAuth::ApiKey(api_key)),
        Some(AnthropicAuthJson {
            auth_mode: AnthropicAuthMode::Oauth,
            oauth: Some(oauth),
            ..
        }) => Some(AnthropicRuntimeAuth::OauthAccessToken(oauth.access_token)),
        _ => None,
    })
}

pub fn account_display_from_auth(auth: AnthropicAuthJson) -> Option<AnthropicAccountDisplay> {
    if auth.auth_mode == AnthropicAuthMode::Oauth
        && auth
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.expires_at)
            .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return None;
    }

    Some(AnthropicAccountDisplay {
        auth_mode: auth.auth_mode,
        email: auth
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.profile.email.clone()),
        subscription_type: auth
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.profile.subscription_type.clone()),
    })
}

pub fn run_anthropic_login_server(
    opts: AnthropicLoginServerOptions,
) -> io::Result<AnthropicLoginServer> {
    let pkce = generate_pkce();
    let state = opts.force_state.clone().unwrap_or_else(generate_state);
    let server = bind_server(opts.port)?;
    let actual_port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "unable to determine Anthropic login callback port",
            ));
        }
    };
    let server = Arc::new(server);
    let redirect_uri = format!("http://localhost:{actual_port}/callback");
    let auth_url = build_authorize_url(&redirect_uri, &pkce, &state);

    if opts.open_browser {
        let _ = webbrowser::open(&auth_url);
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Request>(16);
    let _reader = {
        let server = server.clone();
        thread::spawn(move || -> io::Result<()> {
            while let Ok(request) = server.recv() {
                tx.blocking_send(request)
                    .map_err(|err| io::Error::other(format!("send login request: {err}")))?;
            }
            Ok(())
        })
    };

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let server_handle = {
        let shutdown_notify = shutdown_notify.clone();
        let server = server;
        tokio::spawn(async move {
            let result = loop {
                tokio::select! {
                    _ = shutdown_notify.notified() => {
                        break Err(io::Error::other("Anthropic login was not completed"));
                    }
                    maybe_req = rx.recv() => {
                        let Some(req) = maybe_req else {
                            break Err(io::Error::other("Anthropic login was not completed"));
                        };
                        let url_raw = req.url().to_string();
                        let response = process_request(&url_raw, &opts, &redirect_uri, &pkce, &state).await;
                        let exit_result = match response {
                            HandledRequest::Response(response) => {
                                let _ = tokio::task::spawn_blocking(move || req.respond(response)).await;
                                None
                            }
                            HandledRequest::ResponseAndExit { headers, body, result } => {
                                let _ = tokio::task::spawn_blocking(move || send_response_with_disconnect(req, headers, body)).await;
                                Some(result)
                            }
                        };
                        if let Some(result) = exit_result {
                            break result;
                        }
                    }
                }
            };
            server.unblock();
            result
        })
    };

    Ok(AnthropicLoginServer {
        auth_url,
        actual_port,
        server_handle,
        shutdown_handle: AnthropicShutdownHandle { shutdown_notify },
    })
}

async fn ensure_anthropic_auth_fresh(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AnthropicAuthJson>> {
    let storage = create_anthropic_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    let Some(auth) = storage.load()? else {
        return Ok(None);
    };
    match auth.auth_mode {
        AnthropicAuthMode::ApiKey => Ok(Some(auth)),
        AnthropicAuthMode::Oauth => maybe_refresh_oauth(auth, storage).await.map(Some),
    }
}

async fn maybe_refresh_oauth(
    auth: AnthropicAuthJson,
    storage: Arc<dyn AnthropicStorageBackend>,
) -> io::Result<AnthropicAuthJson> {
    let Some(oauth) = auth.oauth.as_ref() else {
        return Ok(auth);
    };
    let Some(expires_at) = oauth.expires_at else {
        return Ok(auth);
    };
    if expires_at > Utc::now() + chrono::Duration::seconds(REFRESH_SKEW_SECONDS) {
        return Ok(auth);
    }
    if oauth.refresh_token.trim().is_empty() {
        return Ok(auth);
    }

    let refreshed = refresh_tokens(&oauth.refresh_token).await?;
    let profile = fetch_profile(&refreshed.access_token)
        .await
        .unwrap_or_else(|_| oauth.profile.clone());
    let refreshed_auth = AnthropicAuthJson {
        auth_mode: AnthropicAuthMode::Oauth,
        api_key: None,
        oauth: Some(AnthropicOauthData {
            access_token: refreshed.access_token,
            refresh_token: refreshed
                .refresh_token
                .unwrap_or_else(|| oauth.refresh_token.clone()),
            expires_at: Some(Utc::now() + chrono::Duration::seconds(refreshed.expires_in as i64)),
            scopes: parse_scopes(refreshed.scope.as_deref()),
            profile,
        }),
        last_refresh: Some(Utc::now()),
    };
    storage.save(&refreshed_auth)?;
    Ok(refreshed_auth)
}

enum HandledRequest {
    Response(Response<Cursor<Vec<u8>>>),
    ResponseAndExit {
        headers: Vec<Header>,
        body: Vec<u8>,
        result: io::Result<()>,
    },
}

async fn process_request(
    url_raw: &str,
    opts: &AnthropicLoginServerOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> HandledRequest {
    let parsed_url = match Url::parse(&format!("http://localhost{url_raw}")) {
        Ok(url) => url,
        Err(err) => {
            warn!("failed to parse Anthropic callback url: {err}");
            return HandledRequest::Response(
                Response::from_string("Bad Request").with_status_code(400),
            );
        }
    };

    match parsed_url.path() {
        "/callback" | "/auth/callback" => {
            let params: HashMap<String, String> = parsed_url.query_pairs().into_owned().collect();
            if params.get("state").map(String::as_str) != Some(state) {
                return error_page(
                    "State mismatch. Please return to Claudex and try again.",
                    io::ErrorKind::PermissionDenied,
                );
            }
            if let Some(error_code) = params.get("error") {
                let message = params
                    .get("error_description")
                    .cloned()
                    .unwrap_or_else(|| error_code.clone());
                return error_page(message, io::ErrorKind::PermissionDenied);
            }
            let Some(code) = params.get("code") else {
                return error_page(
                    "Missing authorization code. Please return to Claudex and try again.",
                    io::ErrorKind::InvalidData,
                );
            };

            match exchange_code_for_tokens(redirect_uri, pkce, state, code).await {
                Ok(tokens) => {
                    let profile = fetch_profile(&tokens.access_token)
                        .await
                        .unwrap_or_default();
                    let auth = AnthropicAuthJson {
                        auth_mode: AnthropicAuthMode::Oauth,
                        api_key: None,
                        oauth: Some(AnthropicOauthData {
                            access_token: tokens.access_token,
                            refresh_token: tokens.refresh_token.unwrap_or_default(),
                            expires_at: Some(
                                Utc::now() + chrono::Duration::seconds(tokens.expires_in as i64),
                            ),
                            scopes: parse_scopes(tokens.scope.as_deref()),
                            profile,
                        }),
                        last_refresh: Some(Utc::now()),
                    };
                    let storage = create_anthropic_storage(
                        opts.codex_home.clone(),
                        opts.auth_credentials_store_mode,
                    );
                    match storage.save(&auth) {
                        Ok(()) => success_page(),
                        Err(err) => error_page(
                            format!(
                                "Anthropic login succeeded but credentials could not be saved: {err}"
                            ),
                            io::ErrorKind::Other,
                        ),
                    }
                }
                Err(err) => error_page(
                    format!("Anthropic token exchange failed: {err}"),
                    io::ErrorKind::Other,
                ),
            }
        }
        "/cancel" => HandledRequest::ResponseAndExit {
            headers: Vec::new(),
            body: b"Anthropic login cancelled".to_vec(),
            result: Err(io::Error::other("Anthropic login cancelled")),
        },
        _ => HandledRequest::Response(Response::from_string("Not Found").with_status_code(404)),
    }
}

fn success_page() -> HandledRequest {
    let body = LOGIN_SUCCESS_TEMPLATE
        .render(&HashMap::<String, String>::new())
        .unwrap_or_else(|err| format!("Anthropic login succeeded. Return to Claudex.\n\n{err}"))
        .into_bytes();
    HandledRequest::ResponseAndExit {
        headers: html_headers(),
        body,
        result: Ok(()),
    }
}

fn error_page(message: impl Into<String>, kind: io::ErrorKind) -> HandledRequest {
    let message = message.into();
    let body = LOGIN_ERROR_TEMPLATE
        .render(&HashMap::from([("message".to_string(), message.clone())]))
        .unwrap_or_else(|_| message.clone())
        .into_bytes();
    HandledRequest::ResponseAndExit {
        headers: html_headers(),
        body,
        result: Err(io::Error::new(kind, message)),
    }
}

fn html_headers() -> Vec<Header> {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .map(|header| vec![header])
        .unwrap_or_default()
}

fn build_authorize_url(redirect_uri: &str, pkce: &PkceCodes, state: &str) -> String {
    let mut url = Url::parse(&anthropic_authorize_url())
        .unwrap_or_else(|err| panic!("valid Anthropic authorize url: {err}"));
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("code", "true");
        query.append_pair("client_id", &anthropic_client_id());
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", &ANTHROPIC_SCOPES.join(" "));
        query.append_pair("code_challenge", &pkce.code_challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", state);
    }
    url.to_string()
}

fn anthropic_authorize_url() -> String {
    std::env::var(ANTHROPIC_AUTHORIZE_URL_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTHROPIC_AUTHORIZE_URL.to_string())
}

fn anthropic_token_url() -> String {
    std::env::var(ANTHROPIC_TOKEN_URL_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTHROPIC_TOKEN_URL.to_string())
}

fn anthropic_profile_url() -> String {
    std::env::var(ANTHROPIC_PROFILE_URL_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTHROPIC_PROFILE_URL.to_string())
}

fn anthropic_client_id() -> String {
    std::env::var(ANTHROPIC_CLIENT_ID_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ANTHROPIC_CLIENT_ID.to_string())
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn bind_server(port: u16) -> io::Result<Server> {
    let bind_address = format!("127.0.0.1:{port}");
    let mut cancel_attempted = false;
    let mut attempts = 0;
    const MAX_ATTEMPTS: u32 = 10;
    const RETRY_DELAY: Duration = Duration::from_millis(200);

    loop {
        match Server::http(&bind_address) {
            Ok(server) => return Ok(server),
            Err(err) => {
                attempts += 1;
                let is_addr_in_use = err
                    .downcast_ref::<io::Error>()
                    .is_some_and(|io_err| io_err.kind() == io::ErrorKind::AddrInUse);
                if is_addr_in_use {
                    if !cancel_attempted {
                        cancel_attempted = true;
                        let _ = send_cancel_request(port);
                    }
                    thread::sleep(RETRY_DELAY);
                    if attempts >= MAX_ATTEMPTS {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!("port {bind_address} is already in use"),
                        ));
                    }
                    continue;
                }
                return Err(io::Error::other(err));
            }
        }
    }
}

fn send_cancel_request(port: u16) -> io::Result<()> {
    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET /cancel HTTP/1.1\r\n")?;
    stream.write_all(format!("Host: 127.0.0.1:{port}\r\n").as_bytes())?;
    stream.write_all(b"Connection: close\r\n\r\n")?;
    let mut buf = [0u8; 64];
    let _ = stream.read(&mut buf);
    Ok(())
}

fn send_response_with_disconnect(
    req: Request,
    mut headers: Vec<Header>,
    body: Vec<u8>,
) -> io::Result<()> {
    let status = TinyStatusCode(200);
    let mut writer = req.into_writer();
    write!(
        writer,
        "HTTP/1.1 {} {}\r\n",
        status.0,
        status.default_reason_phrase()
    )?;
    headers.retain(|header| !header.field.equiv("Connection"));
    if let Ok(header) = Header::from_bytes(&b"Connection"[..], &b"close"[..]) {
        headers.push(header);
    }
    let content_length = format!("{}", body.len());
    if let Ok(header) = Header::from_bytes(&b"Content-Length"[..], content_length.as_bytes()) {
        headers.push(header);
    }
    for header in headers {
        write!(
            writer,
            "{}: {}\r\n",
            header.field.as_str(),
            header.value.as_str()
        )?;
    }
    writer.write_all(b"\r\n")?;
    writer.write_all(&body)?;
    writer.flush()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}

#[derive(Serialize)]
struct AuthorizationCodeRequest<'a> {
    grant_type: &'a str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Serialize)]
struct RefreshTokenRequest<'a> {
    grant_type: &'a str,
    refresh_token: &'a str,
    client_id: &'a str,
    scope: String,
}

async fn exchange_code_for_tokens(
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
    code: &str,
) -> io::Result<TokenResponse> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let response = client
        .post(anthropic_token_url())
        .header("Content-Type", "application/json")
        .json(&AuthorizationCodeRequest {
            grant_type: "authorization_code",
            code,
            redirect_uri,
            client_id: &anthropic_client_id(),
            code_verifier: &pkce.code_verifier,
            state,
        })
        .send()
        .await
        .map_err(std::io::Error::other)?;

    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io::Error::other(format!(
            "Anthropic token exchange failed: {status}: {body}"
        )));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(io::Error::other)
}

async fn refresh_tokens(refresh_token: &str) -> io::Result<TokenResponse> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let response = client
        .post(anthropic_token_url())
        .header("Content-Type", "application/json")
        .json(&RefreshTokenRequest {
            grant_type: "refresh_token",
            refresh_token,
            client_id: &anthropic_client_id(),
            scope: ANTHROPIC_SCOPES.join(" "),
        })
        .send()
        .await
        .map_err(io::Error::other)?;

    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io::Error::other(format!(
            "Anthropic token refresh failed: {status}: {body}"
        )));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(io::Error::other)
}

#[derive(Deserialize)]
struct OAuthProfileResponse {
    account: OAuthProfileAccount,
    organization: OAuthProfileOrganization,
}

#[derive(Deserialize)]
struct OAuthProfileAccount {
    email: Option<String>,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct OAuthProfileOrganization {
    uuid: Option<String>,
    organization_type: Option<String>,
    rate_limit_tier: Option<String>,
}

async fn fetch_profile(access_token: &str) -> io::Result<AnthropicProfile> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let response = client
        .get(anthropic_profile_url())
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(io::Error::other)?;

    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io::Error::other(format!(
            "Anthropic profile fetch failed: {status}: {body}"
        )));
    }

    let profile = response
        .json::<OAuthProfileResponse>()
        .await
        .map_err(io::Error::other)?;
    Ok(AnthropicProfile {
        email: profile.account.email,
        display_name: profile.account.display_name,
        organization_uuid: profile.organization.uuid,
        subscription_type: map_subscription_type(profile.organization.organization_type.as_deref()),
        rate_limit_tier: profile.organization.rate_limit_tier,
    })
}

fn map_subscription_type(organization_type: Option<&str>) -> Option<String> {
    match organization_type {
        Some("claude_max") => Some("max".to_string()),
        Some("claude_pro") => Some("pro".to_string()),
        Some("claude_enterprise") => Some("enterprise".to_string()),
        Some("claude_team") => Some("team".to_string()),
        _ => None,
    }
}

fn parse_scopes(scope: Option<&str>) -> Vec<String> {
    scope
        .unwrap_or_default()
        .split(' ')
        .filter(|entry| !entry.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn anthropic_api_key_round_trips_from_storage() {
        let tempdir = TempDir::new().expect("create tempdir");
        login_with_anthropic_api_key(
            tempdir.path(),
            "sk-ant-test",
            AuthCredentialsStoreMode::File,
        )
        .expect("save anthropic api key");

        let auth = load_anthropic_auth(tempdir.path(), AuthCredentialsStoreMode::File)
            .expect("load anthropic auth")
            .expect("anthropic auth should exist");
        assert_eq!(auth.auth_mode, AnthropicAuthMode::ApiKey);
        assert_eq!(auth.api_key.as_deref(), Some("sk-ant-test"));

        let runtime_auth =
            resolve_anthropic_runtime_auth(tempdir.path(), AuthCredentialsStoreMode::File)
                .await
                .expect("resolve runtime auth");
        assert_eq!(
            runtime_auth,
            Some(AnthropicRuntimeAuth::ApiKey("sk-ant-test".to_string()))
        );

        let removed = logout_anthropic(tempdir.path(), AuthCredentialsStoreMode::File)
            .expect("logout anthropic");
        assert_eq!(removed, true);
    }

    #[test]
    fn parse_scopes_ignores_empty_segments() {
        assert_eq!(
            parse_scopes(Some("user:profile  user:inference ")),
            vec!["user:profile".to_string(), "user:inference".to_string()]
        );
    }
}
