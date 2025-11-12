use crate::index::model::IndexSnapshot;
use anyhow::Result;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use tracing::warn;

pub enum SnapshotLoad {
    Missing,
    Loaded(Box<IndexSnapshot>),
    ResetAfterCorruption,
}

pub fn load_snapshot(path: &Path) -> Result<SnapshotLoad> {
    if !path.exists() {
        return Ok(SnapshotLoad::Missing);
    }
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    match bincode::deserialize(&buf) {
        Ok(snapshot) => Ok(SnapshotLoad::Loaded(Box::new(snapshot))),
        Err(err) => {
            warn!(
                "navigator snapshot at {:?} is unreadable ({err}); rebuilding from scratch",
                path
            );
            if let Err(remove_err) = fs::remove_file(path) {
                warn!(
                    "failed to remove corrupted snapshot {:?}: {remove_err}",
                    path
                );
            }
            Ok(SnapshotLoad::ResetAfterCorruption)
        }
    }
}

pub fn save_snapshot(path: &Path, tmp_path: &Path, snapshot: &IndexSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = bincode::serialize(snapshot)?;
    {
        let mut file = fs::File::create(tmp_path)?;
        file.write_all(&data)?;
        file.sync_all()?;
    }
    fs::rename(tmp_path, path)?;
    Ok(())
}
