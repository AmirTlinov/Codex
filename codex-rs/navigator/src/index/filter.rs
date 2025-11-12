use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use ignore::Match;
use ignore::gitignore::Gitignore;
use ignore::gitignore::GitignoreBuilder;

const DEFAULT_PATTERNS: &[&str] = &["target/", ".git/", "node_modules/"];

#[derive(Debug, Clone)]
pub struct PathFilter {
    root: PathBuf,
    matcher: Gitignore,
}

impl PathFilter {
    pub fn new(root: &Path) -> Result<Self> {
        let mut builder = GitignoreBuilder::new(root);
        for pattern in DEFAULT_PATTERNS {
            builder.add_line(None, pattern)?;
        }
        add_ignore_file(&mut builder, root.join(".gitignore"));
        add_ignore_file(&mut builder, root.join(".codexignore"));
        let matcher = builder.build()?;
        Ok(Self {
            root: root.to_path_buf(),
            matcher,
        })
    }

    pub fn is_ignored_path(&self, path: &Path, is_dir_hint: Option<bool>) -> bool {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let is_dir = is_dir_hint.unwrap_or_else(|| abs.is_dir());
        matches!(
            self.matcher.matched_path_or_any_parents(&abs, is_dir),
            Match::Ignore(_)
        )
    }

    pub fn is_ignored_rel(&self, rel: &str) -> bool {
        let rel_path = self.root.join(rel);
        self.is_ignored_path(&rel_path, None)
    }
}

fn add_ignore_file(builder: &mut GitignoreBuilder, path: PathBuf) {
    if path.exists() {
        let _ = builder.add(path);
    }
}

#[cfg(test)]
mod tests {
    use super::PathFilter;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn default_patterns_ignore_target() {
        let dir = tempdir().unwrap();
        let filter = PathFilter::new(dir.path()).unwrap();
        let target_path = dir.path().join("target").join("foo.rs");
        assert!(filter.is_ignored_path(&target_path, Some(false)));
    }

    #[test]
    fn gitignore_patterns_are_loaded() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
        let filter = PathFilter::new(dir.path()).unwrap();
        let dist = dir.path().join("dist/output.js");
        assert!(filter.is_ignored_path(&dist, Some(false)));
    }

    #[test]
    fn regular_files_are_not_ignored() {
        let dir = tempdir().unwrap();
        let filter = PathFilter::new(dir.path()).unwrap();
        let file = dir.path().join("src/lib.rs");
        assert!(!filter.is_ignored_path(&file, Some(false)));
    }
}
