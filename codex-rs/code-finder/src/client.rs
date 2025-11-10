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
use std::cmp::min;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
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

    let log_path = daemon_log_path(project);
    let spawner = spawn.ok_or_else(|| {
        anyhow!("code-finder daemon is not running and no spawn command was provided")
    })?;
    spawn_daemon(spawner, &log_path).await?;
    let meta = wait_for_metadata(project, &log_path).await?;
    ping(&meta)
        .await
        .map_err(|err| attach_log(err, &log_path))?;
    Ok(meta)
}

async fn spawn_daemon(spawn: &DaemonSpawn, log_path: &Path) -> Result<()> {
    let mut cmd = Command::new(&spawn.program);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)?;
    let timestamp = OffsetDateTime::now_utc();
    let formatted = timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| timestamp.unix_timestamp().to_string());
    let _ = writeln!(
        log_file,
        "===== code-finder daemon start {formatted} =====",
    );
    log_file.flush().ok();
    let stderr = log_file.try_clone()?;
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    for (key, value) in &spawn.env {
        cmd.env(key, value);
    }
    cmd.spawn().context("failed to spawn code-finder daemon")?;
    Ok(())
}

async fn wait_for_metadata(project: &ProjectProfile, log_path: &Path) -> Result<DaemonMetadata> {
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
    Err(timeout_error_with_log(log_path))
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

fn daemon_log_path(project: &ProjectProfile) -> PathBuf {
    project.logs_dir().join("code-finder-daemon.log")
}

fn timeout_error_with_log(log_path: &Path) -> anyhow::Error {
    let base = anyhow!("timed out waiting for code-finder daemon metadata");
    attach_log(base, log_path)
}

fn attach_log(err: anyhow::Error, log_path: &Path) -> anyhow::Error {
    if let Some(tail) = read_log_tail(log_path) {
        err.context(format!(
            "See {} for daemon stderr. Last lines:\n{}",
            log_path.display(),
            tail
        ))
    } else {
        err.context(format!(
            "See {} for daemon stderr (log is empty)",
            log_path.display()
        ))
    }
}

fn read_log_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let len = metadata.len();
    let tail_bytes = 8192u64;
    let start = len.saturating_sub(tail_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    let lines: Vec<&str> = buf.lines().collect();
    if lines.is_empty() {
        return Some(buf);
    }
    let tail_line_count = min(lines.len(), 40);
    let start_idx = lines.len() - tail_line_count;
    Some(lines[start_idx..].join("\n"))
}
