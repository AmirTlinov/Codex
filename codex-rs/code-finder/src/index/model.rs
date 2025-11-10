use crate::proto::FileCategory;
use crate::proto::Language;
use crate::proto::Range;
use crate::proto::SymbolKind;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub id: String,
    pub identifier: String,
    pub kind: SymbolKind,
    pub language: Language,
    pub path: String,
    pub range: Range,
    pub module: Option<String>,
    pub layer: Option<String>,
    pub categories: Vec<FileCategory>,
    pub recent: bool,
    pub preview: String,
    pub doc_summary: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub mtime: Option<u64>,
    pub size: u64,
    pub digest: [u8; 16],
}

impl FileFingerprint {
    pub fn new(metadata: &std::fs::Metadata, digest: [u8; 16]) -> Self {
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        Self {
            mtime,
            size: metadata.len(),
            digest,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub language: Language,
    pub categories: Vec<FileCategory>,
    pub recent: bool,
    pub symbol_ids: Vec<String>,
    pub tokens: Vec<String>,
    pub fingerprint: FileFingerprint,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct IndexSnapshot {
    pub symbols: HashMap<String, SymbolRecord>,
    pub files: HashMap<String, FileEntry>,
    pub token_to_files: HashMap<String, HashSet<String>>,
}

impl IndexSnapshot {
    pub fn symbol(&self, id: &str) -> Option<&SymbolRecord> {
        self.symbols.get(id)
    }
}
