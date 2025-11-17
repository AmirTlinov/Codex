use std::path::Path;

/// Supported programming languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Bash,
    Unknown,
}

impl Language {
    /// Detect language from file extension
    pub fn from_path(path: &Path) -> Self {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(Self::from_extension)
            .unwrap_or(Language::Unknown)
    }

    /// Detect language from file extension string
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" => Language::Rust,
            "py" | "pyw" | "pyi" => Language::Python,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "go" => Language::Go,
            "java" => Language::Java,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Language::Cpp,
            "cs" => Language::CSharp,
            "rb" => Language::Ruby,
            "sh" | "bash" => Language::Bash,
            _ => Language::Unknown,
        }
    }

    /// Get the language name as string
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Go => "go",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::CSharp => "csharp",
            Language::Ruby => "ruby",
            Language::Bash => "bash",
            Language::Unknown => "unknown",
        }
    }

    /// Check if language is supported for AST parsing
    pub fn has_tree_sitter_support(self) -> bool {
        matches!(self, Language::Bash)
        // TODO: Add more languages as tree-sitter parsers are added
    }

    /// Get estimated tokens per line for this language
    pub fn avg_tokens_per_line(self) -> f32 {
        match self {
            Language::Rust | Language::Cpp | Language::CSharp => 8.0,
            Language::Python | Language::JavaScript | Language::TypeScript => 6.0,
            Language::Go | Language::Java => 7.0,
            Language::C => 7.5,
            Language::Ruby => 5.5,
            Language::Bash => 5.0,
            Language::Unknown => 6.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Language::Rust);
        assert_eq!(Language::from_extension("py"), Language::Python);
        assert_eq!(Language::from_extension("ts"), Language::TypeScript);
        assert_eq!(Language::from_extension("go"), Language::Go);
        assert_eq!(Language::from_extension("unknown"), Language::Unknown);
    }

    #[test]
    fn test_language_from_path() {
        assert_eq!(
            Language::from_path(Path::new("main.rs")),
            Language::Rust
        );
        assert_eq!(
            Language::from_path(Path::new("script.py")),
            Language::Python
        );
        assert_eq!(
            Language::from_path(Path::new("index.ts")),
            Language::TypeScript
        );
    }

    #[test]
    fn test_language_name() {
        assert_eq!(Language::Rust.name(), "rust");
        assert_eq!(Language::Python.name(), "python");
        assert_eq!(Language::Unknown.name(), "unknown");
    }

    #[test]
    fn test_tokens_per_line() {
        assert!(Language::Rust.avg_tokens_per_line() > 0.0);
        assert!(Language::Python.avg_tokens_per_line() > 0.0);
    }
}
