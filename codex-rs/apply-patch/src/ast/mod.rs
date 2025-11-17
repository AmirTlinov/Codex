use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use tree_sitter::Language;
use tree_sitter::Parser;
use tree_sitter::Tree;

pub mod cpp;
pub mod go;
pub mod javascript;
pub mod python;
pub mod query;
pub mod rust;
pub mod semantic;
pub mod service;
pub mod shell;
pub mod typescript;

use cpp::CppSymbolLocator;
use go::GoSymbolLocator;
use javascript::JavaScriptSymbolLocator;
use python::PythonSymbolLocator;
use rust::RustSymbolLocator;
use shell::ShellSymbolLocator;
use typescript::TypeScriptSymbolLocator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineColumnRange {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl LineColumnRange {
    pub fn lines_only(&self) -> (usize, usize) {
        (self.start_line, self.end_line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolPath {
    segments: Vec<String>,
}

impl SymbolPath {
    pub fn new(segments: Vec<String>) -> Self {
        Self { segments }
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn last(&self) -> Option<&str> {
        self.segments.last().map(std::string::String::as_str)
    }

    pub fn replace_last(&self, new_value: impl Into<String>) -> Self {
        let mut segments = self.segments.clone();
        if let Some(last) = segments.last_mut() {
            *last = new_value.into();
        } else {
            segments.push(new_value.into());
        }
        SymbolPath::new(segments)
    }

    pub fn parent_segments(&self) -> &[String] {
        if self.segments.len() <= 1 {
            &[]
        } else {
            &self.segments[..self.segments.len() - 1]
        }
    }
}

impl fmt::Display for SymbolPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut segments = self.segments.iter();
        if let Some(first) = segments.next() {
            f.write_str(first)?;
            for segment in segments {
                write!(f, "::{segment}")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolTarget {
    pub language: &'static str,
    pub header_range: Range<usize>,
    pub body_range: Option<Range<usize>>,
    pub symbol_path: SymbolPath,
    pub symbol_kind: String,
    pub name_range: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolution {
    Match(SymbolTarget),
    Unsupported { reason: String },
    NotFound { reason: String },
}

pub trait SymbolLocator: Sync + Send {
    fn language(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn locate(&self, source: &str, symbol: &SymbolPath) -> SymbolResolution;
}

static REGISTRY: Lazy<Vec<&'static dyn SymbolLocator>> = Lazy::new(|| {
    vec![
        RustSymbolLocator::instance(),
        TypeScriptSymbolLocator::instance(),
        JavaScriptSymbolLocator::instance(),
        CppSymbolLocator::instance(),
        GoSymbolLocator::instance(),
        PythonSymbolLocator::instance(),
        ShellSymbolLocator::instance(),
    ]
});

pub fn parse_tree_for_language(language: &str, source: &str) -> Result<Tree, String> {
    match language {
        "rust" => rust::parse_tree(source),
        "typescript" => typescript::parse_tree(source),
        "javascript" => javascript::parse_tree(source),
        "cpp" => cpp::parse_tree(source),
        "go" => go::parse_tree(source),
        "python" => python::parse_tree(source),
        "shell" => shell::parse_tree(source),
        other => Err(format!("no parser registered for {other}")),
    }
}

pub fn tree_sitter_language(language: &str) -> Option<Language> {
    match language {
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "shell" => Some(tree_sitter_bash::LANGUAGE.into()),
        _ => None,
    }
}

pub fn resolve_locator(path: &Path) -> Option<&'static dyn SymbolLocator> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    REGISTRY
        .iter()
        .copied()
        .find(|locator| locator.extensions().iter().any(|e| *e == ext))
}

pub fn resolve_locator_by_language(language: &str) -> Option<&'static dyn SymbolLocator> {
    REGISTRY
        .iter()
        .copied()
        .find(|locator| locator.language() == language)
}

pub fn symbol_path_from_str(raw: &str) -> SymbolPath {
    let segments = raw
        .split("::")
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>();
    SymbolPath::new(segments)
}

pub(crate) fn parse_with_cached_parser(
    parser: &'static Lazy<Mutex<Parser>>,
    language: &'static str,
    source: &str,
) -> Result<Tree, String> {
    let mut parser = parser
        .lock()
        .map_err(|_| format!("failed to lock {language} parser"))?;
    parser
        .parse(source, None)
        .ok_or_else(|| format!("failed to parse {language} source"))
}

pub(crate) fn extract_name_bytes(
    node: tree_sitter::Node,
    source: &str,
) -> Option<(String, Range<usize>)> {
    let bytes = source.as_bytes();
    let name_node = node.child_by_field_name("name")?;
    let text = name_node.utf8_text(bytes).ok()?.to_string();
    Some((text, name_node.byte_range()))
}

pub(crate) fn range_from_node(node: tree_sitter::Node) -> Range<usize> {
    node.start_byte()..node.end_byte()
}

pub(crate) fn byte_range_to_line_col(range: Range<usize>, source: &str) -> LineColumnRange {
    fn line_col_at(bytes: &[u8], idx: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (pos, b) in bytes.iter().enumerate() {
            if pos == idx {
                break;
            }
            if *b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    let bytes = source.as_bytes();
    let (start_line, start_col) = line_col_at(bytes, range.start.min(bytes.len()));
    let end_index = if range.end == 0 {
        0
    } else {
        range.end.saturating_sub(1)
    };
    let (end_line, end_col) = line_col_at(bytes, end_index.min(bytes.len()));
    LineColumnRange {
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

pub(crate) fn body_range(node: tree_sitter::Node) -> Option<Range<usize>> {
    node.child_by_field_name("body")
        .or_else(|| node.child_by_field_name("block"))
        .or_else(|| node.child_by_field_name("suite"))
        .map(range_from_node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    fn assert_symbol_found(locator: &'static dyn SymbolLocator, source: &str, raw_path: &str) {
        let symbol = symbol_path_from_str(raw_path);
        match locator.locate(source, &symbol) {
            SymbolResolution::Match(target) => {
                assert_eq!(target.symbol_path, symbol);
                assert_eq!(target.language, locator.language());
                assert!(
                    target.body_range.is_some(),
                    "ожидался диапазон тела символа"
                );
                assert!(target.header_range.start <= target.header_range.end);
                assert!(!target.symbol_kind.is_empty());
            }
            other => panic!("ожидалось совпадение для {raw_path}, получено: {other:?}"),
        }
    }

    #[test]
    fn symbol_path_from_str_splits_and_trims() {
        let path = symbol_path_from_str(" module :: inner :: func ");
        assert_eq!(path.segments(), &["module", "inner", "func"]);
        assert!(!path.is_empty());
        assert_eq!(path.last(), Some("func"));
    }

    #[test]
    fn byte_range_to_line_col_counts_lines() {
        let source = "first\nsecond\nthird\n";
        let ranges = [0..5, 6..12, 13..18];
        let mut previous_start = 0;
        for range in ranges {
            let position = byte_range_to_line_col(range.clone(), source);
            let (start_line, end_line) = position.lines_only();
            assert!(start_line >= 1, "номера строк должны начинаться с 1");
            assert!(
                end_line >= start_line,
                "конец диапазона не может быть раньше начала"
            );
            assert!(
                start_line >= previous_start,
                "диапазоны должны двигаться вперёд по тексту"
            );
            previous_start = start_line;
        }
    }

    #[test]
    fn resolve_locator_matches_extension() {
        let rust = resolve_locator(Path::new("lib.rs")).expect("должен найти локатор");
        assert_eq!(rust.language(), "rust");
        assert!(resolve_locator(Path::new("README.md")).is_none());
    }

    #[test]
    fn rust_locator_finds_function() {
        let locator = RustSymbolLocator::instance();
        let source = "fn greet() {\n    println!(\"hi\");\n}\n";
        assert_symbol_found(locator, source, "greet");
    }

    #[test]
    fn typescript_locator_finds_function() {
        let locator = TypeScriptSymbolLocator::instance();
        let source = "export function greet(): void {\n  console.log('hi');\n}\n";
        assert_symbol_found(locator, source, "greet");
    }

    #[test]
    fn rust_locator_finds_nested_symbol() {
        let locator = RustSymbolLocator::instance();
        let source = "mod outer {\n    pub struct Greeter;\n    impl Greeter {\n        pub fn greet(&self) {}\n    }\n}\n";
        assert_symbol_found(locator, source, "outer::Greeter::greet");
    }

    #[test]
    fn typescript_locator_finds_class_method() {
        let locator = TypeScriptSymbolLocator::instance();
        let source = "export class Greeter {\n  greet(): void {\n    console.log('hi');\n  }\n}\n";
        assert_symbol_found(locator, source, "Greeter::greet");
    }

    #[test]
    fn python_locator_finds_method_in_class() {
        let locator = PythonSymbolLocator::instance();
        let source = "class Greeter:\n    def greet(self):\n        return 'hi'\n";
        assert_symbol_found(locator, source, "Greeter::greet");
    }

    #[test]
    fn python_locator_finds_function() {
        let locator = PythonSymbolLocator::instance();
        let source = "def greet():\n    return 'hi'\n";
        assert_symbol_found(locator, source, "greet");
    }

    #[test]
    fn locator_reports_missing_symbol() {
        let locator = RustSymbolLocator::instance();
        let source = "fn greet() {}\n";
        let missing = symbol_path_from_str("missing");
        match locator.locate(source, &missing) {
            SymbolResolution::NotFound { reason } => {
                assert!(reason.contains("missing"));
            }
            other => panic!("ожидался NotFound, получено: {other:?}"),
        }
    }
}
