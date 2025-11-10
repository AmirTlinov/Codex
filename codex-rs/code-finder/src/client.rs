use crate::metadata::DaemonMetadata;
use crate::project::ProjectProfile;
use crate::proto::IndexStatus;
use crate::proto::OpenRequest;
use crate::proto::OpenResponse;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;
use crate::proto::SnippetRequest;
use crate::proto::SnippetResponse;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use reqwest::header::AUTHORIZATION;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;
use tokio::process::Command;
use tokio::time::sleep;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_POLL_INTERVAL: Duration = Duration::from_millis(200);
const METADATA_WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    pub project_root: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub spawn: Option<DaemonSpawn>,
}

#[derive(Clone, Debug)]
pub struct DaemonSpawn {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct CodeFinderClient {
    project: ProjectProfile,
    http: reqwest::Client,
    base_url: String,
    secret: String,
}

impl CodeFinderClient {
    pub async fn new(opts: ClientOptions) -> Result<Self> {
        let project =
            ProjectProfile::detect(opts.project_root.as_deref(), opts.codex_home.as_deref())?;
        project.ensure_dirs()?;
        let metadata = ensure_daemon(&project, opts.spawn.as_ref()).await?;
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()?;
        let base_url = format!("http://127.0.0.1:{}", metadata.port);
        Ok(Self {
            project,
            http,
            base_url,
            secret: metadata.secret,
        })
    }

    pub fn project(&self) -> &ProjectProfile {
        &self.project
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        let url = format!("{}/v1/nav/search", self.base_url);
        let resp = self
            .http
            .post(url)
            .headers(self.auth_headers()?)
            .json(request)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("search request failed: {status} - {body}"));
        }
        Ok(resp.json().await?)
    }

    pub async fn open(&self, request: &OpenRequest) -> Result<OpenResponse> {
        let url = format!("{}/v1/nav/open", self.base_url);
        let resp = self
            .http
            .post(url)
            .headers(self.auth_headers()?)
            .json(request)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("open request failed: {status} - {body}"));
        }
        Ok(resp.json().await?)
    }

    pub async fn snippet(&self, request: &SnippetRequest) -> Result<SnippetResponse> {
        let url = format!("{}/v1/nav/snippet", self.base_url);
        let resp = self
            .http
            .post(url)
            .headers(self.auth_headers()?)
            .json(request)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("snippet request failed: {status} - {body}"));
        }
        Ok(resp.json().await?)
    }

    pub async fn health(&self) -> Result<IndexStatus> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .http
            .get(url)
            .headers(self.auth_headers()?)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("health request failed: {status} - {body}"));
        }
        Ok(resp.json().await?)
    }

    pub async fn reindex(&self) -> Result<IndexStatus> {
        let url = format!("{}/v1/nav/reindex", self.base_url);
        let resp = self
            .http
            .post(url)
            .headers(self.auth_headers()?)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("reindex request failed: {status} - {body}"));
        }
        Ok(resp.json().await?)
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.secret))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

async fn ensure_daemon(
    project: &ProjectProfile,
    spawn: Option<&DaemonSpawn>,
) -> Result<DaemonMetadata> {
    if let Ok(meta) = DaemonMetadata::load(&project.metadata_path())
        && meta.is_compatible()
        && ping(&meta).await.is_ok()
    {
        return Ok(meta);
    }

    let spawner = spawn.ok_or_else(|| {
        anyhow!("code-finder daemon is not running and no spawn command was provided")
    })?;
    spawn_daemon(project, spawner).await?;
    let meta = wait_for_metadata(project).await?;
    ping(&meta).await?;
    Ok(meta)
}

async fn spawn_daemon(_project: &ProjectProfile, spawn: &DaemonSpawn) -> Result<()> {
    let mut cmd = Command::new(&spawn.program);
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in &spawn.env {
        cmd.env(key, value);
    }
    cmd.spawn().context("failed to spawn code-finder daemon")?;
    Ok(())
}

async fn wait_for_metadata(project: &ProjectProfile) -> Result<DaemonMetadata> {
    let deadline = Instant::now() + METADATA_WAIT;
    loop {
        if let Ok(meta) = DaemonMetadata::load(&project.metadata_path())
            && meta.is_compatible()
        {
            return Ok(meta);
        }
        if Instant::now() > deadline {
            break;
        }
        sleep(METADATA_POLL_INTERVAL).await;
    }
    Err(anyhow!("timed out waiting for code-finder daemon metadata"))
}

async fn ping(meta: &DaemonMetadata) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let url = format!("http://127.0.0.1:{}/health", meta.port);
    let resp = client.get(url).bearer_auth(&meta.secret).send().await?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(anyhow!("daemon health check failed"))
    }
}
