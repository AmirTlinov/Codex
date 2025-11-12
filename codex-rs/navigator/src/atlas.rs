use crate::index::model::FileEntry;
use crate::index::model::IndexSnapshot;
use crate::proto::AtlasHint;
use crate::proto::AtlasHintSummary;
use crate::proto::AtlasNode;
use crate::proto::AtlasNodeKind;
use crate::proto::AtlasSnapshot;
use crate::proto::FileCategory;
use crate::proto::NavHit;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use time::OffsetDateTime;

#[derive(Debug, Deserialize)]
struct CargoWorkspace {
    #[serde(default)]
    workspace: Option<WorkspaceSection>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkspaceSection {
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Debug, Clone)]
struct WorkspaceMember {
    name: String,
    path: String,
}

#[derive(Default)]
struct AtlasMetrics {
    files: usize,
    symbols: usize,
    loc: usize,
    docs: usize,
    tests: usize,
    deps: usize,
    recent: usize,
}

#[derive(Default)]
struct NodeAccumulator {
    metrics: AtlasMetrics,
    children: BTreeMap<String, NodeAccumulator>,
}

mod focus {
    use crate::proto::AtlasNode;

    #[derive(Debug, Clone)]
    pub struct AtlasFocus<'a> {
        pub node: &'a AtlasNode,
        pub breadcrumb: Vec<&'a AtlasNode>,
        pub matched: bool,
    }

    impl<'a> AtlasFocus<'a> {
        pub fn new(node: &'a AtlasNode, breadcrumb: Vec<&'a AtlasNode>, matched: bool) -> Self {
            Self {
                node,
                breadcrumb,
                matched,
            }
        }
    }
}

pub use focus::AtlasFocus;

pub fn atlas_focus<'a>(root: &'a AtlasNode, target: Option<&str>) -> AtlasFocus<'a> {
    let Some(requested) = target.map(str::trim).filter(|token| !token.is_empty()) else {
        return AtlasFocus::new(root, vec![root], false);
    };
    let normalized_name = normalize_name_token(requested);
    let normalized_path = normalize_path_token(requested);
    if let Some(trail) = atlas_breadcrumb_internal(root, &normalized_name, &normalized_path) {
        let node = trail.last().copied().unwrap_or(root);
        return AtlasFocus::new(node, trail, true);
    }
    AtlasFocus::new(root, vec![root], false)
}

pub fn find_atlas_node<'a>(root: &'a AtlasNode, target: &str) -> Option<&'a AtlasNode> {
    let normalized_name = normalize_name_token(target);
    let normalized_path = normalize_path_token(target);
    atlas_breadcrumb_internal(root, &normalized_name, &normalized_path)
        .and_then(|trail| trail.last().copied())
}

pub fn atlas_breadcrumb<'a>(root: &'a AtlasNode, target: &str) -> Option<Vec<&'a AtlasNode>> {
    let normalized_name = normalize_name_token(target);
    let normalized_path = normalize_path_token(target);
    atlas_breadcrumb_internal(root, &normalized_name, &normalized_path)
}

fn atlas_breadcrumb_internal<'a>(
    node: &'a AtlasNode,
    normalized_name: &str,
    normalized_path: &str,
) -> Option<Vec<&'a AtlasNode>> {
    if atlas_node_matches(node, normalized_name, normalized_path) {
        return Some(vec![node]);
    }
    for child in &node.children {
        if let Some(mut trail) = atlas_breadcrumb_internal(child, normalized_name, normalized_path)
        {
            let mut breadcrumb = Vec::with_capacity(trail.len() + 1);
            breadcrumb.push(node);
            breadcrumb.append(&mut trail);
            return Some(breadcrumb);
        }
    }
    None
}

fn atlas_node_matches(node: &AtlasNode, normalized_name: &str, normalized_path: &str) -> bool {
    if normalized_name == normalize_name_token(&node.name) {
        return true;
    }
    if normalized_path.is_empty() {
        return false;
    }
    node.path
        .as_deref()
        .map(|path| normalize_path_token(path) == normalized_path)
        .unwrap_or(false)
}

fn normalize_name_token(token: &str) -> String {
    token.trim().to_ascii_lowercase()
}

fn normalize_path_token(token: &str) -> String {
    token
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\''))
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

pub fn rebuild_atlas(snapshot: &mut IndexSnapshot, project_root: &Path) {
    snapshot.atlas = build_snapshot(snapshot, project_root);
}

pub fn build_search_hint(snapshot: &IndexSnapshot, hits: &[NavHit]) -> Option<AtlasHint> {
    let root = snapshot.atlas.root.as_ref()?;
    if hits.is_empty() {
        return None;
    }
    let target = select_target_path(hits);
    let focus = atlas_focus(root, target.as_deref());
    let breadcrumb = focus
        .breadcrumb
        .iter()
        .map(|node| node.name.clone())
        .collect();
    let mut top_children: Vec<AtlasHintSummary> =
        focus.node.children.iter().map(hint_summary).collect();
    top_children.sort_by(|a, b| b.file_count.cmp(&a.file_count));
    top_children.truncate(5);
    Some(AtlasHint {
        target,
        matched: focus.matched,
        breadcrumb,
        focus: hint_summary(focus.node),
        top_children,
    })
}

fn build_snapshot(snapshot: &IndexSnapshot, project_root: &Path) -> AtlasSnapshot {
    let members = discover_workspace_members(project_root);
    let root_name = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let mut children = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    for member in members {
        if let Some(node) = build_crate_node(&member, snapshot) {
            seen_paths.insert(member.path.clone());
            children.push(node);
        }
    }
    children.sort_by(|a, b| a.name.cmp(&b.name));

    let mut root_acc = NodeAccumulator::default();
    for child in &children {
        root_acc.metrics.files += child.file_count;
        root_acc.metrics.symbols += child.symbol_count;
        root_acc.metrics.loc += child.loc;
        root_acc.metrics.docs += child.doc_files;
        root_acc.metrics.tests += child.test_files;
        root_acc.metrics.deps += child.dep_files;
        root_acc.metrics.recent += child.recent_files;
    }

    if children.is_empty() {
        // fall back to a single synthetic node that summarizes every indexed file.
        let mut accumulator = NodeAccumulator::default();
        for (path, entry) in snapshot.files.iter() {
            accumulator.add_file(path, entry);
        }
        if accumulator.metrics.files == 0 {
            return AtlasSnapshot::default();
        }
        return AtlasSnapshot {
            generated_at: Some(OffsetDateTime::now_utc()),
            root: Some(accumulator.into_node(root_name, AtlasNodeKind::Workspace, None)),
        };
    }

    let root_node = AtlasNode {
        name: root_name,
        kind: AtlasNodeKind::Workspace,
        path: Some(String::from(".")),
        file_count: root_acc.metrics.files,
        symbol_count: root_acc.metrics.symbols,
        loc: root_acc.metrics.loc,
        doc_files: root_acc.metrics.docs,
        test_files: root_acc.metrics.tests,
        dep_files: root_acc.metrics.deps,
        recent_files: root_acc.metrics.recent,
        children,
    };

    AtlasSnapshot {
        generated_at: Some(OffsetDateTime::now_utc()),
        root: Some(root_node),
    }
}

fn build_crate_node(member: &WorkspaceMember, snapshot: &IndexSnapshot) -> Option<AtlasNode> {
    let prefix = format!("{}/", member.path.trim_end_matches('/'));
    let mut accumulator = NodeAccumulator::default();
    for (path, entry) in snapshot.files.iter() {
        if path == &member.path || path.starts_with(&prefix) {
            let rel = path
                .strip_prefix(&member.path)
                .unwrap_or(path)
                .trim_start_matches('/');
            accumulator.add_file(rel, entry);
        }
    }
    if accumulator.metrics.files == 0 {
        return None;
    }
    Some(accumulator.into_node(
        member.name.clone(),
        AtlasNodeKind::Crate,
        Some(member.path.clone()),
    ))
}

impl NodeAccumulator {
    fn add_file(&mut self, relative_path: &str, entry: &FileEntry) {
        self.metrics.ingest(entry);
        let normalized = relative_path.trim_matches('/');
        if normalized.is_empty() {
            return;
        }
        let mut segments = normalized.split('/');
        if let Some(segment) = segments.next() {
            let remainder = segments.collect::<Vec<_>>().join("/");
            let child = self.children.entry(segment.to_string()).or_default();
            child.add_file(&remainder, entry);
        }
    }

    fn into_node(self, name: String, kind: AtlasNodeKind, path: Option<String>) -> AtlasNode {
        let mut children = Vec::new();
        for (segment, child) in self.children {
            let child_path = match &path {
                Some(parent) if !parent.is_empty() => Some(format!("{parent}/{segment}")),
                _ => Some(segment.clone()),
            };
            children.push(child.into_node(segment, AtlasNodeKind::Module, child_path));
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        AtlasNode {
            name,
            kind,
            path,
            file_count: self.metrics.files,
            symbol_count: self.metrics.symbols,
            loc: self.metrics.loc,
            doc_files: self.metrics.docs,
            test_files: self.metrics.tests,
            dep_files: self.metrics.deps,
            recent_files: self.metrics.recent,
            children,
        }
    }
}

impl AtlasMetrics {
    fn ingest(&mut self, entry: &FileEntry) {
        self.files += 1;
        self.symbols += entry.symbol_ids.len();
        self.loc += entry.line_count as usize;
        if entry.recent {
            self.recent += 1;
        }
        for category in &entry.categories {
            match category {
                FileCategory::Docs => self.docs += 1,
                FileCategory::Tests => self.tests += 1,
                FileCategory::Deps => self.deps += 1,
                FileCategory::Source => {}
            }
        }
    }
}

fn discover_workspace_members(project_root: &Path) -> Vec<WorkspaceMember> {
    let cargo_path = project_root.join("Cargo.toml");
    let contents = match fs::read_to_string(&cargo_path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let parsed: CargoWorkspace = match toml::from_str(&contents) {
        Ok(doc) => doc,
        Err(_) => return Vec::new(),
    };
    let workspace = match parsed.workspace {
        Some(section) => section,
        None => return Vec::new(),
    };
    let mut members = Vec::new();
    for raw in workspace.members {
        let trimmed = raw.trim().trim_matches('"');
        if trimmed.is_empty() {
            continue;
        }
        let name = Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(trimmed)
            .to_string();
        let rel = trimmed.trim_start_matches("./").to_string();
        members.push(WorkspaceMember { name, path: rel });
    }
    if members.is_empty() {
        members.push(WorkspaceMember {
            name: "workspace".to_string(),
            path: String::new(),
        });
    }
    members
}

fn hint_summary(node: &AtlasNode) -> AtlasHintSummary {
    AtlasHintSummary {
        name: node.name.clone(),
        kind: node.kind.clone(),
        file_count: node.file_count,
        symbol_count: node.symbol_count,
        loc: node.loc,
        recent_files: node.recent_files,
        doc_files: node.doc_files,
        test_files: node.test_files,
        dep_files: node.dep_files,
    }
}

fn select_target_path(hits: &[NavHit]) -> Option<String> {
    for hit in hits {
        if let Some(dir) = parent_directory(&hit.path) {
            return Some(dir);
        }
    }
    hits.first().map(|hit| hit.path.clone())
}

fn parent_directory(path: &str) -> Option<String> {
    let parent = Path::new(path).parent()?;
    let normalized = parent.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::FileEntry;
    use crate::index::model::FileFingerprint;
    use crate::proto::FileCategory;
    use crate::proto::Language;
    use crate::proto::NavHit;
    use crate::proto::SymbolKind;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn atlas_builds_tree_from_workspace_members() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let cargo = r#"[workspace]
members = ["core", "tui"]
"#;
        fs::write(root.join("Cargo.toml"), cargo).unwrap();
        fs::create_dir_all(root.join("core")).unwrap();
        fs::create_dir_all(root.join("tui")).unwrap();

        let mut snapshot = IndexSnapshot {
            files: sample_files(),
            ..Default::default()
        };
        rebuild_atlas(&mut snapshot, root);
        let atlas = snapshot.atlas.clone();
        assert!(atlas.root.is_some());
        let root_node = atlas.root.unwrap();
        assert_eq!(root_node.children.len(), 2);
        let core = root_node
            .children
            .iter()
            .find(|node| node.name == "core")
            .expect("core node");
        assert_eq!(core.file_count, 1);
        assert_eq!(core.symbol_count, 2);
        assert_eq!(core.kind, AtlasNodeKind::Crate);
        assert_eq!(core.loc, 20);
    }

    #[test]
    fn atlas_focus_matches_node_by_name() {
        let mut snapshot = IndexSnapshot {
            files: sample_files(),
            ..Default::default()
        };
        rebuild_atlas(&mut snapshot, Path::new("."));
        let atlas = snapshot.atlas;
        let root = atlas.root.expect("root node");
        let focus = crate::atlas_focus(&root, Some("core"));
        assert!(focus.matched);
        assert_eq!(focus.node.name, "core");
        assert_eq!(focus.breadcrumb.len(), 2);
    }

    #[test]
    fn atlas_focus_falls_back_to_root() {
        let mut snapshot = IndexSnapshot {
            files: sample_files(),
            ..Default::default()
        };
        rebuild_atlas(&mut snapshot, Path::new("."));
        let atlas = snapshot.atlas;
        let root = atlas.root.expect("root node");
        let focus = crate::atlas_focus(&root, Some("missing"));
        assert!(!focus.matched);
        assert_eq!(focus.node.name, root.name);
        assert_eq!(focus.breadcrumb.len(), 1);
    }

    #[test]
    fn build_search_hint_returns_focus_for_hit_directory() {
        let mut snapshot = IndexSnapshot {
            files: sample_files(),
            ..Default::default()
        };
        rebuild_atlas(&mut snapshot, Path::new("."));
        let hit = sample_hit("core/src/lib.rs");
        let hint = build_search_hint(&snapshot, &[hit]).expect("atlas hint");
        assert!(
            hint.breadcrumb
                .starts_with(&["workspace".into(), "core".into()])
        );
        assert_eq!(hint.focus.name, "src");
        assert!(!hint.top_children.is_empty());
    }

    #[test]
    fn build_search_hint_returns_none_without_hits() {
        let mut snapshot = IndexSnapshot {
            files: sample_files(),
            ..Default::default()
        };
        rebuild_atlas(&mut snapshot, Path::new("."));
        assert!(build_search_hint(&snapshot, &[]).is_none());
    }

    fn sample_files() -> HashMap<String, FileEntry> {
        let mut files = HashMap::new();
        files.insert(
            "core/src/lib.rs".to_string(),
            FileEntry {
                path: "core/src/lib.rs".to_string(),
                language: Language::Rust,
                categories: vec![FileCategory::Source],
                recent: true,
                symbol_ids: vec!["1".into(), "2".into()],
                tokens: Vec::new(),
                trigrams: Vec::new(),
                line_count: 20,
                attention: 0,
                churn: 0,
                owners: Vec::new(),
                fingerprint: FileFingerprint {
                    mtime: None,
                    size: 10,
                    digest: [0; 16],
                },
            },
        );
        files.insert(
            "tui/src/main.rs".to_string(),
            FileEntry {
                path: "tui/src/main.rs".to_string(),
                language: Language::Rust,
                categories: vec![FileCategory::Source],
                recent: false,
                symbol_ids: vec!["3".into()],
                tokens: Vec::new(),
                trigrams: Vec::new(),
                line_count: 12,
                attention: 0,
                churn: 0,
                owners: Vec::new(),
                fingerprint: FileFingerprint {
                    mtime: None,
                    size: 10,
                    digest: [0; 16],
                },
            },
        );
        files
    }

    fn sample_hit(path: &str) -> NavHit {
        NavHit {
            id: format!("literal::{path}#1"),
            path: path.to_string(),
            line: 1,
            kind: SymbolKind::Document,
            language: Language::Rust,
            module: None,
            layer: None,
            categories: vec![FileCategory::Source],
            recent: true,
            preview: String::new(),
            score: 0.0,
            references: None,
            help: None,
            context_snippet: None,
            owners: Vec::new(),
        }
    }
}
