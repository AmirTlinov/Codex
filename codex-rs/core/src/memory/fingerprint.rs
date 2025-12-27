use crate::config::types::MemoryStalenessMode;
use crate::memory::block::Fingerprint;
use crate::memory::block::SourceKind;
use crate::memory::block::SourceRef;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::Duration;
use tokio::time::timeout;

const GIT_HASH_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn fingerprint_for_source(
    source: &SourceRef,
    cwd: &Path,
    mode: MemoryStalenessMode,
) -> io::Result<Fingerprint> {
    let path = resolve_source_path(source, cwd);
    fingerprint_for_path(&path, mode).await
}

pub async fn fingerprint_for_path(
    path: &Path,
    mode: MemoryStalenessMode,
) -> io::Result<Fingerprint> {
    match mode {
        MemoryStalenessMode::GitOid => match fingerprint_git_oid(path).await {
            Ok(fingerprint) => Ok(fingerprint),
            Err(_) => fingerprint_mtime_size(path).await,
        },
        MemoryStalenessMode::MtimeSize => fingerprint_mtime_size(path).await,
    }
}

pub fn resolve_source_path(source: &SourceRef, cwd: &Path) -> PathBuf {
    let path = Path::new(&source.locator);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub async fn fill_missing_file_fingerprints(
    sources: &mut [SourceRef],
    cwd: &Path,
    mode: MemoryStalenessMode,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for source in sources.iter_mut() {
        if source.kind != SourceKind::FilePath || source.fingerprint.is_some() {
            continue;
        }
        match fingerprint_for_source(source, cwd, mode).await {
            Ok(fingerprint) => source.fingerprint = Some(fingerprint),
            Err(err) => warnings.push(format!(
                "failed to fingerprint {locator}: {err}",
                locator = source.locator
            )),
        }
    }
    warnings
}

async fn fingerprint_git_oid(path: &Path) -> io::Result<Fingerprint> {
    let output = timeout(
        GIT_HASH_TIMEOUT,
        Command::new("git").arg("hash-object").arg(path).output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "git hash-object timed out"))?
    .map_err(|err| io::Error::other(format!("{err}")))?;

    if !output.status.success() {
        return Err(io::Error::other("git hash-object failed"));
    }

    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(Fingerprint::GitOid { oid })
}

async fn fingerprint_mtime_size(path: &Path) -> io::Result<Fingerprint> {
    let metadata = tokio::fs::metadata(path).await?;
    let modified = metadata.modified()?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mtime_ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    Ok(Fingerprint::MtimeSize {
        mtime_ns,
        size_bytes: metadata.len(),
    })
}
