use crate::proto::Language;
use crate::proto::SymbolKind;
use once_cell::sync::Lazy;
use regex::Regex;

fn compile_regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|err| panic!("invalid regex literal {pattern}: {err}"))
}

#[derive(Clone, Debug)]
pub struct SymbolCandidate {
    pub identifier: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub preview: String,
    pub doc_summary: Option<String>,
}

pub fn detect_language(path: &std::path::Path) -> Language {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if ext == "rs" => Language::Rust,
        Some(ext) if ext == "ts" => Language::Typescript,
        Some(ext) if ext == "tsx" => Language::Tsx,
        Some(ext) if ext == "js" || ext == "jsx" => Language::Javascript,
        Some(ext) if ext == "py" => Language::Python,
        Some(ext) if ext == "go" => Language::Go,
        Some(ext) if ext == "sh" || ext == "bash" => Language::Bash,
        Some(ext) if ext == "md" || ext == "markdown" => Language::Markdown,
        Some(ext) if ext == "json" => Language::Json,
        Some(ext) if ext == "yaml" || ext == "yml" => Language::Yaml,
        Some(ext) if ext == "toml" => Language::Toml,
        _ => Language::Unknown,
    }
}

pub fn extract_symbols(language: Language, lines: &[&str]) -> Vec<SymbolCandidate> {
    match language {
        Language::Rust => extract_rust(lines),
        Language::Typescript | Language::Tsx | Language::Javascript => extract_typescript(lines),
        Language::Python => extract_python(lines),
        Language::Go => extract_go(lines),
        Language::Bash => extract_bash(lines),
        Language::Markdown => extract_markdown(lines),
        Language::Json | Language::Yaml | Language::Toml => {
            extract_document(lines, SymbolKind::Document)
        }
        _ => extract_fallback(lines),
    }
}

fn extract_rust(lines: &[&str]) -> Vec<SymbolCandidate> {
    static FN_REGEX: Lazy<Regex> = Lazy::new(|| {
        compile_regex(
            r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe)\s+|extern\s+(?:"[^"]+"|\w+)\s+)*fn\s+([A-Za-z0-9_]+)"#,
        )
    });
    static STRUCT_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z0-9_]+)"));
    static ENUM_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z0-9_]+)"));
    static TRAIT_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z0-9_]+)"));
    static MOD_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z0-9_]+)"));
    static CONST_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Za-z0-9_]+)"));

    let mut symbols = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let mut push_candidate = |name: String, kind: SymbolKind| {
            let doc = doc_block(lines, idx, "///");
            symbols.push(SymbolCandidate {
                identifier: name,
                kind,
                start_line: (idx + 1) as u32,
                end_line: (idx + 1) as u32,
                preview: trimmed.to_string(),
                doc_summary: doc,
            });
        };

        if let Some(caps) = FN_REGEX.captures(trimmed) {
            let Some(name_match) = caps.get(1) else {
                continue;
            };
            let name = name_match.as_str().to_string();
            let prev = lines.get(idx.wrapping_sub(1)).copied().unwrap_or_default();
            let kind = if prev.contains("#[test]") || name.starts_with("test_") {
                SymbolKind::Test
            } else {
                SymbolKind::Function
            };
            push_candidate(name, kind);
            continue;
        }
        if let Some(caps) = STRUCT_REGEX.captures(trimmed) {
            push_candidate(caps[1].to_string(), SymbolKind::Struct);
            continue;
        }
        if let Some(caps) = ENUM_REGEX.captures(trimmed) {
            push_candidate(caps[1].to_string(), SymbolKind::Enum);
            continue;
        }
        if let Some(caps) = TRAIT_REGEX.captures(trimmed) {
            push_candidate(caps[1].to_string(), SymbolKind::Trait);
            continue;
        }
        if let Some(caps) = MOD_REGEX.captures(trimmed) {
            push_candidate(caps[1].to_string(), SymbolKind::Module);
            continue;
        }
        if let Some(caps) = CONST_REGEX.captures(trimmed) {
            push_candidate(caps[1].to_string(), SymbolKind::Constant);
            continue;
        }
    }
    symbols
}

fn extract_typescript(lines: &[&str]) -> Vec<SymbolCandidate> {
    static FN_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:export\s+)?function\s+([A-Za-z0-9_]+)"));
    static CLASS_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:export\s+)?class\s+([A-Za-z0-9_]+)"));
    static INTERFACE_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:export\s+)?interface\s+([A-Za-z0-9_]+)"));
    static TYPE_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:export\s+)?type\s+([A-Za-z0-9_]+)"));
    static CONST_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*(?:export\s+)?const\s+([A-Za-z0-9_]+)\s*="));

    extract_with_regex(
        lines,
        &[
            (&FN_REGEX, SymbolKind::Function),
            (&CLASS_REGEX, SymbolKind::Class),
            (&INTERFACE_REGEX, SymbolKind::Interface),
            (&TYPE_REGEX, SymbolKind::TypeAlias),
            (&CONST_REGEX, SymbolKind::Constant),
        ],
        "//",
    )
}

fn extract_python(lines: &[&str]) -> Vec<SymbolCandidate> {
    static DEF_REGEX: Lazy<Regex> = Lazy::new(|| compile_regex(r"^\s*def\s+([A-Za-z0-9_]+)\s*\("));
    static CLASS_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*class\s+([A-Za-z0-9_]+)\s*[:(]"));
    extract_with_regex(
        lines,
        &[
            (&DEF_REGEX, SymbolKind::Function),
            (&CLASS_REGEX, SymbolKind::Class),
        ],
        "#",
    )
}

fn extract_go(lines: &[&str]) -> Vec<SymbolCandidate> {
    static FUNC_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*func\s+([A-Za-z0-9_]+)\s*\("));
    static METHOD_REGEX: Lazy<Regex> =
        Lazy::new(|| compile_regex(r"^\s*func\s+\([^)]*\)\s*([A-Za-z0-9_]+)\s*\("));
    static TYPE_REGEX: Lazy<Regex> = Lazy::new(|| compile_regex(r"^\s*type\s+([A-Za-z0-9_]+)\s"));
    extract_with_regex(
        lines,
        &[
            (&METHOD_REGEX, SymbolKind::Method),
            (&FUNC_REGEX, SymbolKind::Function),
            (&TYPE_REGEX, SymbolKind::TypeAlias),
        ],
        "//",
    )
}

fn extract_bash(lines: &[&str]) -> Vec<SymbolCandidate> {
    static FN_REGEX: Lazy<Regex> = Lazy::new(|| compile_regex(r"^\s*([A-Za-z0-9_]+)\s*\(\)\s*\{"));
    extract_with_regex(lines, &[(&FN_REGEX, SymbolKind::Function)], "#")
}

fn extract_markdown(lines: &[&str]) -> Vec<SymbolCandidate> {
    static HEADING: Lazy<Regex> = Lazy::new(|| compile_regex(r"^(#+)\s+(.+)"));
    let mut symbols = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(caps) = HEADING.captures(line) {
            let text = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            symbols.push(SymbolCandidate {
                identifier: text.to_string(),
                kind: SymbolKind::Document,
                start_line: (idx + 1) as u32,
                end_line: (idx + 1) as u32,
                preview: line.trim().to_string(),
                doc_summary: None,
            });
        }
    }
    symbols
}

fn extract_document(lines: &[&str], kind: SymbolKind) -> Vec<SymbolCandidate> {
    if lines.is_empty() {
        return Vec::new();
    }
    let preview = lines
        .iter()
        .take(3)
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    vec![SymbolCandidate {
        identifier: kind_name(&kind).to_string(),
        kind,
        start_line: 1,
        end_line: lines.len() as u32,
        preview,
        doc_summary: None,
    }]
}

fn extract_fallback(lines: &[&str]) -> Vec<SymbolCandidate> {
    extract_document(lines, SymbolKind::Document)
}

fn extract_with_regex(
    lines: &[&str],
    patterns: &[(&Regex, SymbolKind)],
    doc_prefix: &str,
) -> Vec<SymbolCandidate> {
    let mut symbols = Vec::new();
    for (idx, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        for (regex, kind) in patterns {
            if let Some(caps) = regex.captures(line) {
                let identifier = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
                if identifier.is_empty() {
                    continue;
                }
                let doc = doc_block(lines, idx, doc_prefix);
                symbols.push(SymbolCandidate {
                    identifier,
                    kind: (*kind).clone(),
                    start_line: (idx + 1) as u32,
                    end_line: (idx + 1) as u32,
                    preview: line.to_string(),
                    doc_summary: doc,
                });
                break;
            }
        }
    }
    symbols
}

fn doc_block(lines: &[&str], idx: usize, prefix: &str) -> Option<String> {
    if idx == 0 {
        return None;
    }
    let mut collected = Vec::new();
    let mut current = idx;
    while current > 0 {
        current -= 1;
        let line = lines[current].trim();
        if line.starts_with(prefix) {
            let content = line
                .trim_start_matches(prefix)
                .trim_start_matches(|c: char| c == ':' || c.is_whitespace());
            collected.push(content.to_string());
        } else if line.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if collected.is_empty() {
        None
    } else {
        collected.reverse();
        Some(collected.join(" "))
    }
}

fn kind_name(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Document => "document",
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Module => "module",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::Constant => "constant",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Test => "test",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_function_with_doc() {
        let lines = vec![
            "/// adds two numbers",
            "pub fn add(a: i32, b: i32) -> i32 {",
            "    a + b",
            "}",
        ];
        let symbols = extract_rust(&lines);
        assert_eq!(symbols.len(), 1);
        let sym = &symbols[0];
        assert_eq!(sym.identifier, "add");
        assert_eq!(sym.kind, SymbolKind::Function);
        assert_eq!(sym.start_line, 2);
        assert_eq!(sym.doc_summary.as_deref(), Some("adds two numbers"));
    }

    #[test]
    fn detects_typescript_class() {
        let lines = vec!["export class UserService {", "  constructor() {}", "}"];
        let symbols = extract_typescript(&lines);
        assert!(!symbols.is_empty());
        assert_eq!(symbols[0].identifier, "UserService");
        assert_eq!(symbols[0].kind, SymbolKind::Class);
    }

    #[test]
    fn detects_markdown_headings() {
        let lines = vec!["# Title", "Something", "## Details"];
        let symbols = extract_markdown(&lines);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].identifier, "Title");
        assert_eq!(symbols[1].identifier, "Details");
    }

    #[test]
    fn detects_async_rust_function() {
        let lines = vec!["pub(crate) async fn fetch_data() {}", "fn helper() {}"];
        let symbols = extract_rust(&lines);
        assert!(symbols.iter().any(|sym| sym.identifier == "fetch_data"));
    }
}
