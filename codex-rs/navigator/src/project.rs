use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use blake3::Hasher;
use dirs::home_dir;
use dunce::canonicalize;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const DATA_DIR_NAME: &str = "navigator";
const METADATA_FILENAME: &str = "daemon.json";
const LOCK_FILENAME: &str = "daemon.lock";
const INDEX_FILENAME: &str = "index.bin";
const HEALTH_FILENAME: &str = "health.bin";
const QUERIES_DIR: &str = "queries";
const LOGS_DIR: &str = "logs";
const TMP_SUFFIX: &str = ".tmp";

#[derive(Debug, Clone)]
pub struct ProjectProfile {
    project_root: PathBuf,
    codex_home: PathBuf,
    hash: String,
}

impl ProjectProfile {
    pub fn detect(project_root: Option<&Path>, codex_home: Option<&Path>) -> Result<Self> {
        let root = detect_project_root(project_root)?;
        let codex_home = detect_codex_home(codex_home)?;
        let hash = hash_project_root(&root)?;
        Ok(Self {
            project_root: root,
            codex_home,
            hash,
        })
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn data_dir(&self) -> PathBuf {
        self.codex_home.join(DATA_DIR_NAME).join(&self.hash)
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.data_dir().join(METADATA_FILENAME)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_dir().join(LOCK_FILENAME)
    }

    pub fn index_path(&self) -> PathBuf {
        self.data_dir().join(INDEX_FILENAME)
    }

    pub fn temp_index_path(&self) -> PathBuf {
        self.data_dir()
            .join(format!("{INDEX_FILENAME}{TMP_SUFFIX}"))
    }

    pub fn health_path(&self) -> PathBuf {
        self.data_dir().join(HEALTH_FILENAME)
    }

    pub fn temp_health_path(&self) -> PathBuf {
        self.data_dir()
            .join(format!("{HEALTH_FILENAME}{TMP_SUFFIX}"))
    }

    pub fn queries_dir(&self) -> PathBuf {
        self.data_dir().join(QUERIES_DIR)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir().join(LOGS_DIR)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.data_dir())?;
        fs::create_dir_all(self.queries_dir())?;
        fs::create_dir_all(self.logs_dir())?;
        Ok(())
    }

    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub fn shared_daemon_dir(&self) -> PathBuf {
        self.codex_home.join(DATA_DIR_NAME)
    }

    pub fn shared_metadata_path(&self) -> PathBuf {
        self.shared_daemon_dir().join(METADATA_FILENAME)
    }

    pub fn shared_log_path(&self) -> PathBuf {
        self.shared_daemon_dir().join("navigator-daemon.log")
    }
}

fn detect_project_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return canonicalize(root).context("failed to canonicalize project root");
    }

    let cwd = std::env::current_dir().context("failed to resolve current dir")?;
    if let Some(root) = git_toplevel(&cwd) {
        return Ok(root);
    }

    canonicalize(&cwd).context("failed to canonicalize working directory")
}

fn detect_codex_home(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return canonicalize(path).context("failed to canonicalize CODEX_HOME override");
    }

    if let Ok(env_home) = std::env::var("CODEX_HOME")
        && !env_home.is_empty()
    {
        return canonicalize(&env_home).context("failed to canonicalize CODEX_HOME");
    }

    let mut home = home_dir().ok_or_else(|| anyhow!("Could not locate home directory"))?;
    home.push(".codex");
    Ok(home)
}

fn hash_project_root(root: &Path) -> Result<String> {
    let canonical =
        canonicalize(root).context("failed to canonicalize project root for hashing")?;
    let mut hasher = Hasher::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut short = String::with_capacity(16);
    for byte in digest.as_bytes().iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(&mut short, "{byte:02x}");
    }
    Ok(short)
}

fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let root = stdout.trim();
    if root.is_empty() {
        return None;
    }
    canonicalize(root).ok()
}
