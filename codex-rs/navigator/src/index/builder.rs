use crate::atlas::rebuild_atlas;
use crate::index::classify::classify_categories;
use crate::index::classify::layer_from_path;
use crate::index::classify::module_path;
use crate::index::codeowners::OwnerResolver;
use crate::index::filter::PathFilter;
use crate::index::language::detect_language;
use crate::index::language::extract_symbols;
use crate::index::model::FileEntry;
use crate::index::model::FileFingerprint;
use crate::index::model::FileText;
use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::proto::Language;
use crate::proto::Range;
use anyhow::Context;
use anyhow::Result;
use blake3::Hasher;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

use regex::Regex;

pub(crate) const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOKENS_PER_FILE: usize = 256;
const MAX_TRIGRAMS_PER_FILE: usize = 4096;
const MAX_ATTENTION_MARKERS: u32 = 32;

#[derive(Clone, Debug)]
pub(crate) enum SkipReason {
    Oversize { bytes: u64 },
    NonUtf8,
    NoSymbols,
    ReadError(String),
}

#[derive(Clone, Debug)]
pub(crate) struct SkippedFile {
    pub path: String,
    pub reason: SkipReason,
}

pub(crate) struct IndexedFile {
    pub file: FileEntry,
    pub symbols: Vec<SymbolRecord>,
    pub text: FileText,
}

pub(crate) enum FileOutcome {
    Indexed(IndexedFile),
    IndexedTextOnly {
        file: IndexedFile,
        reason: SkipReason,
    },
    Skipped(SkipReason),
}

pub(crate) struct BuildArtifacts {
    pub snapshot: IndexSnapshot,
    pub skipped: Vec<SkippedFile>,
}

pub struct IndexBuilder<'a> {
    root: &'a Path,
    recent: HashSet<String>,
    churn: HashMap<String, u32>,
    owners: OwnerResolver,
    filter: Arc<PathFilter>,
}

impl<'a> IndexBuilder<'a> {
    pub fn new(
        root: &'a Path,
        recent: HashSet<String>,
        churn: HashMap<String, u32>,
        owners: OwnerResolver,
        filter: Arc<PathFilter>,
    ) -> Self {
        Self {
            root,
            recent,
            churn,
            owners,
            filter,
        }
    }

    pub fn build(&self) -> Result<BuildArtifacts> {
        let mut snapshot = IndexSnapshot::default();
        let mut token_map: HashMap<String, HashSet<String>> = HashMap::new();
        let mut trigram_map: HashMap<u32, HashSet<String>> = HashMap::new();
        let mut skipped = Vec::new();
        let filter = self.filter.clone();
        let walker = WalkBuilder::new(self.root)
            .hidden(false)
            .follow_links(true)
            .standard_filters(true)
            .filter_entry(move |entry| {
                let is_dir_hint = entry.file_type().map(|ft| ft.is_dir()).or_else(|| {
                    // Entry may point to a deleted path; treat as file.
                    Some(entry.path().is_dir())
                });
                !filter.is_ignored_path(entry.path(), is_dir_hint)
            })
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!("skipping entry: {err}");
                    continue;
                }
            };
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }
            let rel = match relative_path(self.root, entry.path()) {
                Some(r) => r,
                None => continue,
            };
            if self.filter.is_ignored_rel(&rel) {
                continue;
            }
            match self.process_file(entry.path(), &rel) {
                Ok(FileOutcome::Indexed(indexed)) => {
                    let IndexedFile {
                        file,
                        symbols,
                        text,
                    } = indexed;
                    let tokens = file.tokens.clone();
                    let trigrams = file.trigrams.clone();
                    for symbol in symbols {
                        snapshot.symbols.insert(symbol.id.clone(), symbol);
                    }
                    snapshot.files.insert(rel.clone(), file);
                    snapshot.text.insert(rel.clone(), text);
                    update_token_map(&mut token_map, &rel, tokens);
                    update_trigram_map(&mut trigram_map, &rel, &trigrams);
                }
                Ok(FileOutcome::IndexedTextOnly {
                    file: indexed,
                    reason,
                }) => {
                    let IndexedFile {
                        file,
                        symbols,
                        text,
                    } = indexed;
                    let tokens = file.tokens.clone();
                    let trigrams = file.trigrams.clone();
                    for symbol in symbols {
                        snapshot.symbols.insert(symbol.id.clone(), symbol);
                    }
                    snapshot.files.insert(rel.clone(), file);
                    snapshot.text.insert(rel.clone(), text);
                    update_token_map(&mut token_map, &rel, tokens);
                    update_trigram_map(&mut trigram_map, &rel, &trigrams);
                    skipped.push(SkippedFile {
                        path: rel.clone(),
                        reason,
                    });
                }
                Ok(FileOutcome::Skipped(reason)) => {
                    skipped.push(SkippedFile {
                        path: rel.clone(),
                        reason,
                    });
                }
                Err(err) => warn!("failed to index {rel}: {err:?}"),
            }
        }

        snapshot.token_to_files = token_map;
        snapshot.trigram_to_files = trigram_map;
        rebuild_atlas(&mut snapshot, self.root);
        Ok(BuildArtifacts { snapshot, skipped })
    }

    pub fn index_path(&self, rel_path: &str) -> Result<FileOutcome> {
        let path = self.root.join(rel_path);
        self.process_file(&path, rel_path)
    }

    fn process_file(&self, path: &Path, rel_path: &str) -> Result<FileOutcome> {
        let metadata = fs::metadata(path).with_context(|| format!("metadata for {rel_path}"))?;
        if metadata.len() as usize > MAX_FILE_BYTES {
            return Ok(FileOutcome::Skipped(SkipReason::Oversize {
                bytes: metadata.len(),
            }));
        }
        let bytes = match fs::read(path) {
            Ok(buf) => buf,
            Err(err) => {
                return Ok(FileOutcome::Skipped(SkipReason::ReadError(err.to_string())));
            }
        };
        let content = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) => return Ok(FileOutcome::Skipped(SkipReason::NonUtf8)),
        };
        let language = detect_language(Path::new(rel_path));
        let categories = classify_categories(rel_path, language.clone());
        let layer = layer_from_path(rel_path);
        let module = module_path(rel_path, language.clone());
        let lines: Vec<&str> = content.lines().collect();
        let line_count = lines.len() as u32;
        let candidates = extract_symbols(language.clone(), &lines);
        let dependencies = collect_dependencies(language.clone(), content);
        let tokens = collect_tokens(content);
        let trigrams = collect_trigrams(content);
        let attention = count_attention_markers(content);
        let owners = self.owners.owners_for(rel_path);
        let churn = self.churn.get(rel_path).copied().unwrap_or(0);
        let recent = self.recent.contains(rel_path);
        let fingerprint = build_fingerprint(&metadata, &bytes);

        let text = FileText::from_content(content)?;

        let mut symbol_ids = Vec::new();
        let mut symbol_records = Vec::new();
        for candidate in candidates {
            let id = symbol_id(rel_path, candidate.start_line, &candidate.identifier);
            let range = Range {
                start: candidate.start_line,
                end: candidate.end_line,
            };
            symbol_ids.push(id.clone());
            symbol_records.push(SymbolRecord {
                id,
                identifier: candidate.identifier,
                kind: candidate.kind,
                language: language.clone(),
                path: rel_path.to_string(),
                range,
                module: module.clone(),
                layer: layer.clone(),
                categories: categories.clone(),
                recent,
                preview: candidate.preview,
                doc_summary: candidate.doc_summary,
                dependencies: dependencies.clone(),
                attention,
                owners: owners.clone(),
                churn,
            });
        }

        if symbol_records.is_empty() {
            let file_entry = FileEntry {
                path: rel_path.to_string(),
                language,
                categories,
                recent,
                symbol_ids: Vec::new(),
                tokens,
                trigrams,
                line_count,
                attention,
                owners,
                churn,
                fingerprint,
            };
            return Ok(FileOutcome::IndexedTextOnly {
                file: IndexedFile {
                    file: file_entry,
                    symbols: Vec::new(),
                    text,
                },
                reason: SkipReason::NoSymbols,
            });
        }

        let file_entry = FileEntry {
            path: rel_path.to_string(),
            language,
            categories,
            recent,
            symbol_ids,
            tokens,
            trigrams,
            line_count,
            attention,
            owners,
            churn,
            fingerprint,
        };

        Ok(FileOutcome::Indexed(IndexedFile {
            file: file_entry,
            symbols: symbol_records,
            text,
        }))
    }
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn update_token_map(map: &mut HashMap<String, HashSet<String>>, path: &str, tokens: Vec<String>) {
    for token in tokens {
        map.entry(token).or_default().insert(path.to_string());
    }
}

fn update_trigram_map(map: &mut HashMap<u32, HashSet<String>>, path: &str, trigrams: &[u32]) {
    for trigram in trigrams {
        map.entry(*trigram).or_default().insert(path.to_string());
    }
}

fn collect_tokens(content: &str) -> Vec<String> {
    let mut unique = HashSet::new();
    let mut current = String::new();
    for ch in content.chars() {
        if unique.len() >= MAX_TOKENS_PER_FILE {
            break;
        }
        if is_token_char(ch) {
            current.push(ch);
        } else {
            push_token(&mut current, &mut unique);
        }
    }
    push_token(&mut current, &mut unique);
    unique.into_iter().collect()
}

fn collect_trigrams(content: &str) -> Vec<u32> {
    let mut set = HashSet::new();
    let lower = content.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for window in bytes.windows(3) {
        if set.len() >= MAX_TRIGRAMS_PER_FILE {
            break;
        }
        let value = ((window[0] as u32) << 16) | ((window[1] as u32) << 8) | window[2] as u32;
        set.insert(value);
    }
    set.into_iter().collect()
}

fn count_attention_markers(content: &str) -> u32 {
    if content.is_empty() {
        return 0;
    }
    let upper = content.to_ascii_uppercase();
    let mut count = 0u32;
    count = count.saturating_add(upper.matches("TODO").count() as u32);
    count = count.saturating_add(upper.matches("FIXME").count() as u32);
    count.min(MAX_ATTENTION_MARKERS)
}

fn push_token(buf: &mut String, set: &mut HashSet<String>) {
    if buf.len() >= 3 {
        let mut chars = buf.chars();
        if let Some(first) = chars.next()
            && (first.is_ascii_alphabetic() || first == '_')
        {
            let token = buf.to_ascii_lowercase();
            if !is_keyword(&token) {
                set.insert(token);
            }
        }
    }
    buf.clear();
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn collect_dependencies(language: Language, content: &str) -> Vec<String> {
    match language {
        Language::Rust => collect_matches(r"^\s*use\s+([A-Za-z0-9_:]+)", content),
        Language::Typescript | Language::Tsx | Language::Javascript => {
            collect_matches(r#"^\s*import\s+.*?['\"]([^'\"]+)['\"]"#, content)
        }
        Language::Python => collect_matches(
            r"^\s*(?:from\s+([A-Za-z0-9_\.]+)|import\s+([A-Za-z0-9_\.]+))",
            content,
        ),
        _ => Vec::new(),
    }
}

fn collect_matches(pattern: &str, content: &str) -> Vec<String> {
    let Ok(regex) = Regex::new(pattern) else {
        return Vec::new();
    };
    let mut deps = HashSet::new();
    for caps in regex.captures_iter(content) {
        for i in 1..caps.len() {
            if let Some(mat) = caps.get(i) {
                let val = mat.as_str().trim().to_string();
                if !val.is_empty() {
                    deps.insert(val);
                }
            }
        }
    }
    deps.into_iter().take(8).collect()
}

fn is_keyword(token: &str) -> bool {
    matches!(
        token,
        "fn" | "struct"
            | "impl"
            | "enum"
            | "class"
            | "const"
            | "let"
            | "pub"
            | "mod"
            | "type"
            | "return"
            | "if"
            | "else"
            | "while"
            | "for"
            | "match"
            | "def"
    )
}

fn build_fingerprint(metadata: &fs::Metadata, bytes: &[u8]) -> FileFingerprint {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut short = [0u8; 16];
    short.copy_from_slice(&digest.as_bytes()[..16]);
    FileFingerprint::new(metadata, short)
}

fn symbol_id(path: &str, line: u32, name: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(path.as_bytes());
    hasher.update(&line.to_le_bytes());
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    digest.to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn builder_respects_gitignore_patterns() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("ignored_dir")).unwrap();
        fs::write(root.join(".gitignore"), "ignored_dir/\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn visible() {}\n").unwrap();
        fs::write(root.join("ignored_dir/lib.rs"), "pub fn hidden() {}\n").unwrap();

        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
        );
        let snapshot = builder.build().unwrap().snapshot;

        assert!(snapshot.files.contains_key("src/lib.rs"));
        assert!(!snapshot.files.contains_key("ignored_dir/lib.rs"));
    }
}
