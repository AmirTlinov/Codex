use crate::index::model::IndexSnapshot;
use anyhow::Result;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;

pub fn load_snapshot(path: &Path) -> Result<Option<IndexSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let snapshot: IndexSnapshot = bincode::deserialize(&buf)?;
    Ok(Some(snapshot))
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
