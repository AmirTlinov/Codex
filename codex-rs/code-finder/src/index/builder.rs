use crate::index::classify::classify_categories;
use crate::index::classify::layer_from_path;
use crate::index::classify::module_path;
use crate::index::filter::PathFilter;
use crate::index::language::detect_language;
use crate::index::language::extract_symbols;
use crate::index::model::FileEntry;
use crate::index::model::FileFingerprint;
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

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOKENS_PER_FILE: usize = 256;

pub struct IndexBuilder<'a> {
    root: &'a Path,
    recent: HashSet<String>,
    filter: Arc<PathFilter>,
}

impl<'a> IndexBuilder<'a> {
    pub fn new(root: &'a Path, recent: HashSet<String>, filter: Arc<PathFilter>) -> Self {
        Self {
            root,
            recent,
            filter,
        }
    }

    pub fn build(&self) -> Result<IndexSnapshot> {
        let mut snapshot = IndexSnapshot::default();
        let mut token_map: HashMap<String, HashSet<String>> = HashMap::new();
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
                Ok(Some((file_entry, symbols, tokens))) => {
                    for symbol in symbols {
                        snapshot.symbols.insert(symbol.id.clone(), symbol);
                    }
                    snapshot.files.insert(rel.clone(), file_entry);
                    update_token_map(&mut token_map, &rel, tokens);
                }
                Ok(None) => {}
                Err(err) => warn!("failed to index {rel}: {err:?}"),
            }
        }

        snapshot.token_to_files = token_map;
        Ok(snapshot)
    }

    #[allow(clippy::type_complexity)]
    fn process_file(
        &self,
        path: &Path,
        rel_path: &str,
    ) -> Result<Option<(FileEntry, Vec<SymbolRecord>, Vec<String>)>> {
        let metadata = fs::metadata(path).with_context(|| format!("metadata for {rel_path}"))?;
        if metadata.len() as usize > MAX_FILE_BYTES {
            return Ok(None);
        }
        let bytes = fs::read(path).with_context(|| format!("read {rel_path}"))?;
        let content = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) => return Ok(None),
        };
        let language = detect_language(Path::new(rel_path));
        let categories = classify_categories(rel_path, language.clone());
        let layer = layer_from_path(rel_path);
        let module = module_path(rel_path, language.clone());
        let lines: Vec<&str> = content.lines().collect();
        let candidates = extract_symbols(language.clone(), &lines);
        let dependencies = collect_dependencies(language.clone(), content);
        let tokens = collect_tokens(content);
        let recent = self.recent.contains(rel_path);
        let fingerprint = build_fingerprint(&metadata, &bytes);

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
            });
        }

        if symbol_records.is_empty() {
            return Ok(None);
        }

        let file_entry = FileEntry {
            path: rel_path.to_string(),
            language,
            categories,
            recent,
            symbol_ids,
            tokens: tokens.clone(),
            fingerprint,
        };

        Ok(Some((file_entry, symbol_records, tokens)))
    }
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
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap();

        assert!(snapshot.files.contains_key("src/lib.rs"));
        assert!(!snapshot.files.contains_key("ignored_dir/lib.rs"));
    }
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn update_token_map(map: &mut HashMap<String, HashSet<String>>, path: &str, tokens: Vec<String>) {
    for token in tokens {
        map.entry(token).or_default().insert(path.to_string());
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
