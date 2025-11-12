use crate::proto::PROTOCOL_VERSION;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonMetadata {
    pub project_hash: String,
    pub project_root: String,
    pub port: u16,
    pub secret: String,
    pub pid: u32,
    pub schema_version: u32,
    pub created_at: OffsetDateTime,
}

impl DaemonMetadata {
    pub fn new(
        project_hash: String,
        project_root: String,
        port: u16,
        secret: String,
        pid: u32,
    ) -> Self {
        Self {
            project_hash,
            project_root,
            port,
            secret,
            pid,
            schema_version: PROTOCOL_VERSION,
            created_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut file = fs::File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let meta = serde_json::from_slice(&buf)?;
        Ok(meta)
    }

    pub fn write_atomic(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("tmp");
        {
            let mut file = fs::File::create(&tmp_path)?;
            let data = serde_json::to_vec_pretty(self)?;
            file.write_all(&data)?;
            file.sync_all()?;
        }
        fs::rename(tmp_path, path)?;
        Ok(())
    }

    pub fn is_compatible(&self) -> bool {
        self.schema_version == PROTOCOL_VERSION
    }
}
