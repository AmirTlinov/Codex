use crate::ApplyPatchError;
use crate::ast::semantic::SemanticModel;
use crate::ast::tree_sitter_language;
use crate::parser::ParseError::InvalidPatchError;
use std::ops::Range;
use tree_sitter::Query;
use tree_sitter::QueryCursor;
use tree_sitter::StreamingIterator;
use tree_sitter::Tree;

use crate::ast::SymbolPath;

#[derive(Debug, Clone)]
pub struct AstQueryMatch {
    pub capture_name: String,
    pub byte_range: Range<usize>,
    pub symbol: Option<SymbolPath>,
}

pub fn run_query(
    language: &str,
    tree: &Tree,
    source: &str,
    query_source: &str,
    semantic: &SemanticModel,
) -> Result<Vec<AstQueryMatch>, ApplyPatchError> {
    let lang = tree_sitter_language(language).ok_or_else(|| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "No tree-sitter language registered for {language}"
        )))
    })?;

    let query = Query::new(&lang, query_source).map_err(|err| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "Invalid tree-sitter query for {language}: {err}"
        )))
    })?;

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut results = Vec::new();
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let node = capture.node;
            let byte_range = node.byte_range();
            let capture_name = capture_names
                .get(capture.index as usize)
                .cloned()
                .unwrap_or_else(|| capture.index.to_string());
            let symbol = semantic
                .binding_covering(&byte_range)
                .map(|binding| binding.symbol.clone());
            results.push(AstQueryMatch {
                capture_name,
                byte_range,
                symbol,
            });
        }
    }
    Ok(results)
}
