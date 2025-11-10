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
    if let Err(err) = parser.set_language(&tree_sitter_cpp::LANGUAGE.into()) {
        panic!("failed to load C++ grammar: {err}");
    }
    Mutex::new(parser)
});

fn parse_tree(source: &str) -> Result<tree_sitter::Tree, String> {
    parse_with_cached_parser(&PARSER, "c++", source)
}

fn node_name(node: Node, source: &str) -> Option<String> {
    if let Some((text, _)) = extract_name_bytes(node, source) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    node.child_by_field_name("declarator")
        .or_else(|| node.child_by_field_name("name"))
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(std::string::ToString::to_string)
}

fn matches_symbol(path: &SymbolPath, node: Node, source: &str) -> bool {
    if let Some(last) = path.last() {
        return node_name(node, source)
            .map(|name| name.contains(last))
            .unwrap_or(false);
    }
    false
}

fn locate_symbol(path: &SymbolPath, source: &str, node: Node) -> Option<SymbolTarget> {
    if matches_symbol(path, node, source) {
        Some(SymbolTarget {
            language: "cpp",
            header_range: range_from_node(node),
            body_range: body_range(node),
            symbol_path: path.clone(),
            symbol_kind: node.kind().to_string(),
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
            "function_definition"
            | "function_declaration"
            | "field_declaration"
            | "member_declaration" => {
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

pub struct CppSymbolLocator;

impl CppSymbolLocator {
    pub fn instance() -> &'static dyn SymbolLocator {
        static INSTANCE: CppSymbolLocator = CppSymbolLocator;
        &INSTANCE
    }
}

impl SymbolLocator for CppSymbolLocator {
    fn language(&self) -> &'static str {
        "cpp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cc", "cpp", "cxx", "hpp", "hh", "hxx", "h"]
    }

    fn locate(&self, source: &str, symbol: &SymbolPath) -> SymbolResolution {
        if symbol.is_empty() {
            return SymbolResolution::NotFound {
                reason: "empty symbol path".into(),
            };
        }
        let tree = match parse_tree(source) {
            Ok(tree) => tree,
            Err(err) => return SymbolResolution::Unsupported { reason: err },
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
