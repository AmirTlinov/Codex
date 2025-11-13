use crate::proto::AtlasSnapshot;
use crate::proto::FileCategory;
use crate::proto::Language;
use crate::proto::Range;
use crate::proto::SymbolKind;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use snap::raw::Decoder;
use snap::raw::Encoder;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;

pub const DEFAULT_FRESHNESS_DAYS: u32 = 365;
const TEXT_BLOCK_TARGET_BYTES: usize = 32 * 1024;

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
    #[serde(default)]
    pub attention: u32,
    #[serde(default)]
    pub attention_density: u32,
    #[serde(default)]
    pub lint_suppressions: u32,
    #[serde(default)]
    pub lint_density: u32,
    #[serde(default)]
    pub churn: u32,
    #[serde(default = "default_freshness_days")]
    pub freshness_days: u32,
    #[serde(default)]
    pub owners: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    #[serde(default)]
    pub trigrams: Vec<u32>,
    #[serde(default)]
    pub line_count: u32,
    #[serde(default)]
    pub attention: u32,
    #[serde(default)]
    pub attention_density: u32,
    #[serde(default)]
    pub lint_suppressions: u32,
    #[serde(default)]
    pub lint_density: u32,
    #[serde(default)]
    pub churn: u32,
    #[serde(default = "default_freshness_days")]
    pub freshness_days: u32,
    #[serde(default)]
    pub owners: Vec<String>,
    pub fingerprint: FileFingerprint,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TextBlock {
    pub start_line: u32,
    pub line_count: u32,
    pub raw_len: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FileText {
    #[serde(default)]
    pub blocks: Vec<TextBlock>,
    #[serde(default)]
    pub line_offsets: Vec<u32>,
    #[serde(default)]
    pub bytes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct IndexSnapshot {
    pub symbols: HashMap<String, SymbolRecord>,
    pub files: HashMap<String, FileEntry>,
    pub token_to_files: HashMap<String, HashSet<String>>,
    #[serde(default)]
    pub trigram_to_files: HashMap<u32, HashSet<String>>,
    #[serde(default)]
    pub text: HashMap<String, FileText>,
    #[serde(default)]
    pub atlas: AtlasSnapshot,
}

impl IndexSnapshot {
    pub fn symbol(&self, id: &str) -> Option<&SymbolRecord> {
        self.symbols.get(id)
    }
}

const fn default_freshness_days() -> u32 {
    DEFAULT_FRESHNESS_DAYS
}

impl TextBlock {
    fn encode(
        start_line: u32,
        line_count: u32,
        slice: &[u8],
        encoder: &mut Encoder,
    ) -> Result<Self> {
        let compressed = encoder
            .compress_vec(slice)
            .context("compressing text block")?;
        Ok(Self {
            start_line,
            line_count,
            raw_len: slice.len() as u32,
            data: compressed,
        })
    }

    fn append_to(&self, target: &mut Vec<u8>, decoder: &mut Decoder) -> Result<()> {
        let raw = decoder.decompress_vec(&self.data).with_context(|| {
            format!(
                "decompressing text block starting at line {}",
                self.start_line
            )
        })?;
        if raw.len() != self.raw_len as usize {
            bail!(
                "text block length mismatch: expected {} got {}",
                self.raw_len,
                raw.len()
            );
        }
        target.extend_from_slice(&raw);
        Ok(())
    }
}

impl FileText {
    pub fn from_content(content: &str) -> Result<Self> {
        if content.is_empty() {
            return Ok(Self::default());
        }
        let mut line_offsets = Vec::new();
        let mut cursor = 0usize;
        for chunk in content.split_inclusive('\n') {
            line_offsets.push(cursor as u32);
            cursor += chunk.len();
        }

        let bytes = content.as_bytes();
        let mut blocks = Vec::new();
        let mut encoder = Encoder::new();
        let mut block_start_line = 1u32;
        let mut block_start_byte = 0usize;
        let mut block_line_count = 0u32;
        let mut block_bytes = 0usize;
        cursor = 0;
        for chunk in content.split_inclusive('\n') {
            let len = chunk.len();
            cursor += len;
            block_line_count += 1;
            block_bytes += len;
            if block_bytes >= TEXT_BLOCK_TARGET_BYTES {
                let slice = &bytes[block_start_byte..cursor];
                blocks.push(TextBlock::encode(
                    block_start_line,
                    block_line_count,
                    slice,
                    &mut encoder,
                )?);
                block_start_line += block_line_count;
                block_start_byte = cursor;
                block_line_count = 0;
                block_bytes = 0;
            }
        }
        if block_line_count > 0 {
            let slice = &bytes[block_start_byte..bytes.len()];
            blocks.push(TextBlock::encode(
                block_start_line,
                block_line_count,
                slice,
                &mut encoder,
            )?);
        }

        Ok(Self {
            blocks,
            line_offsets,
            bytes: bytes.len() as u32,
        })
    }

    pub fn decode(&self) -> Result<String> {
        if self.blocks.is_empty() {
            return Ok(String::new());
        }
        let mut decoder = Decoder::new();
        let mut buf = Vec::with_capacity(self.bytes as usize);
        for block in &self.blocks {
            block.append_to(&mut buf, &mut decoder)?;
        }
        String::from_utf8(buf).context("snapshot text is not valid UTF-8")
    }
}
