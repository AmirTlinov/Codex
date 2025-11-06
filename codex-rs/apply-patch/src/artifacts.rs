use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtifactSummary {
    pub log: Option<PathBuf>,
    pub conflicts: Vec<PathBuf>,
    pub unapplied: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnappliedKind {
    Add,
    Update,
    Delete,
}

impl UnappliedKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnappliedKind::Add => "add",
            UnappliedKind::Update => "update",
            UnappliedKind::Delete => "delete",
        }
    }
}

impl Default for UnappliedKind {
    fn default() -> Self {
        UnappliedKind::Update
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnappliedEntry {
    pub path: PathBuf,
    pub kind: UnappliedKind,
    pub contents: String,
}

pub fn summarize_unapplied(entries: &[UnappliedEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            format!(
                "Unapplied {} for {}:
{}",
                entry.kind.as_str(),
                entry.path.display(),
                entry.contents
            )
        })
        .collect()
}
