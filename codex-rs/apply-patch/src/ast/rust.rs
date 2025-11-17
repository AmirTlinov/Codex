use once_cell::sync::Lazy;
use std::sync::Mutex;
use tree_sitter::Node;
use tree_sitter::Parser;

use super::SymbolLocator;
use super::SymbolPath;
use super::SymbolResolution;
use super::SymbolTarget;
use super::body_range;
use super::extract_name_bytes;
use super::parse_with_cached_parser;
use super::range_from_node;

static PARSER: Lazy<Mutex<Parser>> = Lazy::new(|| {
    let mut parser = Parser::new();
    if let Err(err) = parser.set_language(&tree_sitter_rust::LANGUAGE.into()) {
        panic!("failed to load Rust grammar: {err}");
    }
    Mutex::new(parser)
});

pub(crate) fn parse_tree(source: &str) -> Result<tree_sitter::Tree, String> {
    parse_with_cached_parser(&PARSER, "rust", source)
}

fn node_name(node: Node, source: &str) -> Option<String> {
    if let Some((text, _)) = extract_name_bytes(node, source) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let bytes = source.as_bytes();
    if let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(name.to_string());
    }

    if let Some(typ) = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(bytes).ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let mut name = typ.to_string();
        if let Some(idx) = name.find('<') {
            name.truncate(idx);
        }
        if let Some(idx) = name.find(" where ") {
            name.truncate(idx);
        }
        let cleaned = name.replace(' ', "");
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }

    None
}

fn collect_named_ancestors(node: Node, source: &str) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        if let Some(name) = node_name(parent, source) {
            ancestors.push(name);
        }
        current = parent.parent();
    }
    ancestors.reverse();
    ancestors
}

fn matches_symbol(path: &SymbolPath, node: Node, source: &str) -> bool {
    if path.is_empty() {
        return false;
    }

    let Some(last_segment) = path.last() else {
        return false;
    };

    if !node_name(node, source)
        .map(|name| name == last_segment)
        .unwrap_or(false)
    {
        return false;
    }

    let parent_segments = path.parent_segments();
    if parent_segments.is_empty() {
        return true;
    }

    let ancestors = collect_named_ancestors(node, source);
    if ancestors.len() < parent_segments.len() {
        return false;
    }

    let start = ancestors.len() - parent_segments.len();
    ancestors[start..]
        .iter()
        .map(String::as_str)
        .eq(parent_segments.iter().map(String::as_str))
}

fn locate_symbol(path: &SymbolPath, source: &str, node: Node) -> Option<SymbolTarget> {
    if matches_symbol(path, node, source) {
        let name_range = extract_name_bytes(node, source).map(|(_, range)| range);
        Some(SymbolTarget {
            language: "rust",
            header_range: range_from_node(node),
            body_range: body_range(node),
            symbol_path: path.clone(),
            symbol_kind: node.kind().to_string(),
            name_range,
        })
    } else {
        None
    }
}

fn find_candidate(
    tree: &tree_sitter::Tree,
    source: &str,
    path: &SymbolPath,
) -> Option<SymbolTarget> {
    let mut stack = vec![tree.root_node()];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "function_item"
            | "function_definition"
            | "method_definition"
            | "function_declaration"
            | "function" => {
                if let Some(target) = locate_symbol(path, source, current) {
                    return Some(target);
                }
            }
            _ => {}
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

pub struct RustSymbolLocator;

impl RustSymbolLocator {
    pub fn instance() -> &'static dyn SymbolLocator {
        static INSTANCE: RustSymbolLocator = RustSymbolLocator;
        &INSTANCE
    }
}

impl SymbolLocator for RustSymbolLocator {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn locate(&self, source: &str, symbol: &SymbolPath) -> SymbolResolution {
        if symbol.is_empty() {
            return SymbolResolution::NotFound {
                reason: "empty symbol path".into(),
            };
        }
        let tree = match parse_tree(source) {
            Ok(tree) => tree,
            Err(err) => {
                return SymbolResolution::Unsupported { reason: err };
            }
        };
        match find_candidate(&tree, source, symbol) {
            Some(mut target) => {
                target.language = self.language();
                SymbolResolution::Match(target)
            }
            None => SymbolResolution::NotFound {
                reason: format!("symbol '{}' not found", symbol.last().unwrap_or("")),
            },
        }
    }
}
