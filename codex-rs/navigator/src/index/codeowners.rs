use globset::GlobBuilder;
use globset::GlobMatcher;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

const CODEOWNERS_CANDIDATES: &[&str] = &["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"];

#[derive(Clone, Default)]
pub struct OwnerResolver {
    matchers: Vec<(GlobMatcher, Vec<String>)>,
}

impl OwnerResolver {
    pub fn load(root: &Path) -> Self {
        for rel in CODEOWNERS_CANDIDATES {
            let path = root.join(rel);
            if path.exists()
                && let Some(resolver) = Self::from_file(&path) {
                    return resolver;
                }
        }
        Self::default()
    }

    fn from_file(path: &PathBuf) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        let mut matchers = Vec::new();
        for line in data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let pattern = match parts.next() {
                Some(value) => value,
                None => continue,
            };
            let owners: Vec<String> = parts
                .map(normalize_owner)
                .filter(|owner| !owner.is_empty())
                .collect();
            if owners.is_empty() {
                continue;
            }
            if let Some(matcher) = compile_pattern(pattern) {
                matchers.push((matcher, owners));
            }
        }
        if matchers.is_empty() {
            None
        } else {
            Some(Self { matchers })
        }
    }

    pub fn owners_for(&self, path: &str) -> Vec<String> {
        if self.matchers.is_empty() {
            return Vec::new();
        }
        let mut owners = Vec::new();
        for (matcher, names) in &self.matchers {
            if matcher.is_match(path) {
                owners = names.clone();
            }
        }
        owners
    }
}

fn compile_pattern(raw: &str) -> Option<GlobMatcher> {
    let mut pattern = raw.trim();
    if pattern.is_empty() {
        return None;
    }
    if pattern.starts_with('/') {
        pattern = &pattern[1..];
    }
    let glob_string = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    GlobBuilder::new(&glob_string)
        .literal_separator(false)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}

fn normalize_owner(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('@')
        .trim_matches('/')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn owners_resolve_last_match() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CODEOWNERS");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "# sample owners").unwrap();
        writeln!(file, "*.rs @rust").unwrap();
        writeln!(file, "src/lib.rs @core @platform").unwrap();
        drop(file);
        let resolver = OwnerResolver::load(dir.path());
        assert_eq!(
            resolver.owners_for("src/other.rs"),
            vec!["rust".to_string()]
        );
        assert_eq!(
            resolver.owners_for("src/lib.rs"),
            vec!["core".to_string(), "platform".to_string()]
        );
        assert!(resolver.owners_for("docs/readme.md").is_empty());
    }
}
