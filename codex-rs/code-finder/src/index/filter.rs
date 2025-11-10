use std::path::Component;
use std::path::Path;

const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target"];

pub(crate) fn has_skipped_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .is_some_and(|value| SKIPPED_DIRS.contains(&value)),
        _ => false,
    })
}

pub(crate) fn path_in_skipped_dir(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .map(has_skipped_component)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_skipped_dir_anywhere_in_path() {
        let path = PathBuf::from("/tmp/project/node_modules/pkg/lib.rs");
        assert!(has_skipped_component(&path));
    }

    #[test]
    fn false_when_no_skipped_segments() {
        let path = PathBuf::from("/tmp/project/src/lib.rs");
        assert!(!has_skipped_component(&path));
    }

    #[test]
    fn path_in_skipped_dir_respects_root_prefix() {
        let root = PathBuf::from("/tmp/project");
        let path = PathBuf::from("/tmp/project/target/debug/foo");
        assert!(path_in_skipped_dir(&path, &root));

        let outside = PathBuf::from("/tmp/other/target/foo");
        assert!(!path_in_skipped_dir(&outside, &root));
    }
}
