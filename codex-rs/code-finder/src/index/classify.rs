use crate::proto::FileCategory;
use crate::proto::Language;
use std::path::Path;

pub fn classify_categories(path: &str, language: Language) -> Vec<FileCategory> {
    let mut categories = Vec::new();
    if is_doc(path, language) {
        categories.push(FileCategory::Docs);
    }
    if is_dep_file(path) {
        categories.push(FileCategory::Deps);
    }
    if is_test(path) {
        categories.push(FileCategory::Tests);
    }
    if categories.is_empty() {
        categories.push(FileCategory::Source);
    } else {
        let has_primary = categories
            .iter()
            .any(|cat| matches!(cat, FileCategory::Docs | FileCategory::Deps));
        if !categories.contains(&FileCategory::Source) && !has_primary {
            categories.push(FileCategory::Source);
        }
    }
    categories
}

pub fn layer_from_path(path: &str) -> Option<String> {
    path.split('/')
        .find(|segment| !segment.is_empty())
        .map(std::string::ToString::to_string)
}

pub fn module_path(path: &str, language: Language) -> Option<String> {
    let mut parts: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(std::string::ToString::to_string)
        .collect();
    let last = parts.pop()?;
    let mut stem = Path::new(&last)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(last);
    if matches!(language, Language::Rust)
        && matches!(stem.as_str(), "mod" | "lib" | "main")
        && let Some(parent) = parts.pop()
    {
        stem = parent;
    }
    if !stem.is_empty() {
        parts.push(stem);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("::"))
    }
}

fn is_doc(path: &str, language: Language) -> bool {
    path.starts_with("docs/")
        || matches!(language, Language::Markdown)
        || path.ends_with(".md")
        || path.ends_with(".rst")
        || path.ends_with(".adoc")
}

fn is_dep_file(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|s| s.to_str()),
        Some("Cargo.toml")
            | Some("Cargo.lock")
            | Some("package.json")
            | Some("pnpm-lock.yaml")
            | Some("yarn.lock")
            | Some("requirements.txt")
            | Some("pyproject.toml")
            | Some("go.mod")
            | Some("go.sum")
    )
}

fn is_test(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/__tests__/")
        || path.contains("/spec/")
        || path.ends_with("_test.rs")
        || path.ends_with("_test.ts")
        || path.ends_with("_spec.ts")
        || path.ends_with("_test.py")
        || path.contains("tests/")
}
