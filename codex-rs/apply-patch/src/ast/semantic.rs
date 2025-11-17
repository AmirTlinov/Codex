use crate::ast::SymbolPath;
use crate::ast::extract_name_bytes;
use crate::ast::range_from_node;
use std::ops::Range;
use tree_sitter::Node;
use tree_sitter::Tree;

#[derive(Debug, Clone)]
pub struct SemanticBinding {
    pub symbol: SymbolPath,
    pub node_range: Range<usize>,
}

impl SemanticBinding {
    fn contains(&self, range: &Range<usize>) -> bool {
        self.node_range.start <= range.start && self.node_range.end >= range.end
    }
}

#[derive(Debug, Default, Clone)]
pub struct SemanticModel {
    bindings: Vec<SemanticBinding>,
}

impl SemanticModel {
    pub fn build(tree: &Tree, source: &str) -> Self {
        let mut bindings = Vec::new();
        let mut scope: Vec<String> = Vec::new();
        let root = tree.root_node();
        collect_bindings(root, source, &mut scope, &mut bindings);
        Self { bindings }
    }

    pub fn binding_covering(&self, range: &Range<usize>) -> Option<&SemanticBinding> {
        self.bindings
            .iter()
            .rev()
            .find(|binding| binding.contains(range))
    }
}

fn collect_bindings(
    node: Node,
    source: &str,
    scope: &mut Vec<String>,
    bindings: &mut Vec<SemanticBinding>,
) {
    let mut pushed = false;
    if let Some((name, _)) = extract_name_bytes(node, source) {
        scope.push(name);
        let symbol = SymbolPath::new(scope.clone());
        bindings.push(SemanticBinding {
            symbol,
            node_range: range_from_node(node),
        });
        pushed = true;
    }

    if node.child_count() > 0 {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                collect_bindings(cursor.node(), source, scope, bindings);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    if pushed {
        scope.pop();
    }
}
