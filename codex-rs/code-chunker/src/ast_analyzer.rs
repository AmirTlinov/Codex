use crate::language::Language;
use crate::{ChunkType, CodeChunk, ChunkMetadata};
use log::debug;

/// AST-based code analyzer
pub struct AstAnalyzer {
    language: Language,
}

impl AstAnalyzer {
    pub fn new(language: Language) -> Self {
        Self { language }
    }

    /// Extract imports and context from code
    pub fn extract_context(&self, content: &str, max_lines: usize) -> Vec<String> {
        let mut context = Vec::new();

        for (i, line) in content.lines().enumerate() {
            if i >= max_lines {
                break;
            }

            let trimmed = line.trim();

            // Extract imports based on language
            let is_import = match self.language {
                Language::Rust => {
                    trimmed.starts_with("use ")
                        || trimmed.starts_with("extern crate ")
                        || trimmed.starts_with("mod ")
                }
                Language::Python => {
                    trimmed.starts_with("import ") || trimmed.starts_with("from ")
                }
                Language::JavaScript | Language::TypeScript => {
                    trimmed.starts_with("import ")
                        || trimmed.starts_with("export ")
                        || trimmed.starts_with("require(")
                }
                Language::Go => trimmed.starts_with("import "),
                Language::Java | Language::CSharp => trimmed.starts_with("import ") || trimmed.starts_with("using "),
                _ => false,
            };

            if is_import {
                context.push(line.to_string());
            }
        }

        context
    }

    /// Detect chunk type from code
    pub fn detect_chunk_type(&self, content: &str) -> ChunkType {
        let first_line = content.lines().next().unwrap_or("").trim();

        match self.language {
            Language::Rust => self.detect_rust_type(first_line, content),
            Language::Python => self.detect_python_type(first_line, content),
            Language::JavaScript | Language::TypeScript => self.detect_js_type(first_line, content),
            _ => ChunkType::Other,
        }
    }

    fn detect_rust_type(&self, first_line: &str, _content: &str) -> ChunkType {
        if first_line.starts_with("fn ") {
            ChunkType::Function
        } else if first_line.starts_with("pub fn ") {
            ChunkType::Function
        } else if first_line.starts_with("async fn ") || first_line.starts_with("pub async fn ") {
            ChunkType::Function
        } else if first_line.starts_with("struct ") || first_line.starts_with("pub struct ") {
            ChunkType::Struct
        } else if first_line.starts_with("enum ") || first_line.starts_with("pub enum ") {
            ChunkType::Enum
        } else if first_line.starts_with("impl ") {
            ChunkType::Impl
        } else if first_line.starts_with("trait ") || first_line.starts_with("pub trait ") {
            ChunkType::Interface
        } else if first_line.starts_with("type ") || first_line.starts_with("pub type ") {
            ChunkType::Type
        } else if first_line.starts_with("const ") || first_line.starts_with("pub const ") {
            ChunkType::Const
        } else if first_line.starts_with("mod ") || first_line.starts_with("pub mod ") {
            ChunkType::Module
        } else if first_line.starts_with("//") || first_line.starts_with("///") {
            ChunkType::Comment
        } else {
            ChunkType::Other
        }
    }

    fn detect_python_type(&self, first_line: &str, _content: &str) -> ChunkType {
        if first_line.starts_with("def ") {
            ChunkType::Function
        } else if first_line.starts_with("async def ") {
            ChunkType::Function
        } else if first_line.starts_with("class ") {
            ChunkType::Class
        } else if first_line.starts_with("import ") || first_line.starts_with("from ") {
            ChunkType::Import
        } else if first_line.starts_with('#') {
            ChunkType::Comment
        } else {
            ChunkType::Other
        }
    }

    fn detect_js_type(&self, first_line: &str, _content: &str) -> ChunkType {
        if first_line.starts_with("function ") || first_line.starts_with("async function ") {
            ChunkType::Function
        } else if first_line.starts_with("class ") {
            ChunkType::Class
        } else if first_line.starts_with("interface ") {
            ChunkType::Interface
        } else if first_line.starts_with("const ") || first_line.starts_with("let ") || first_line.starts_with("var ") {
            ChunkType::Variable
        } else if first_line.starts_with("import ") || first_line.starts_with("export ") {
            ChunkType::Import
        } else if first_line.starts_with("//") || first_line.starts_with("/*") {
            ChunkType::Comment
        } else {
            ChunkType::Other
        }
    }

    /// Extract symbol name from code
    pub fn extract_symbol_name(&self, content: &str, chunk_type: ChunkType) -> Option<String> {
        let first_line = content.lines().next()?;

        match chunk_type {
            ChunkType::Function | ChunkType::Method => {
                self.extract_function_name(first_line)
            }
            ChunkType::Class | ChunkType::Struct => {
                self.extract_class_name(first_line)
            }
            ChunkType::Enum => {
                self.extract_enum_name(first_line)
            }
            _ => None,
        }
    }

    fn extract_function_name(&self, line: &str) -> Option<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        match self.language {
            Language::Rust => {
                // fn name() or pub fn name() or async fn name()
                parts.iter()
                    .skip_while(|&&p| p != "fn")
                    .nth(1)
                    .and_then(|name| name.split('(').next())
                    .map(|s| s.to_string())
            }
            Language::Python => {
                // def name() or async def name()
                parts.iter()
                    .skip_while(|&&p| p != "def")
                    .nth(1)
                    .and_then(|name| name.split('(').next())
                    .map(|s| s.to_string())
            }
            Language::JavaScript | Language::TypeScript => {
                // function name() or class name
                parts.get(1)
                    .and_then(|name| name.split('(').next())
                    .map(|s| s.to_string())
            }
            _ => None,
        }
    }

    fn extract_class_name(&self, line: &str) -> Option<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        parts.iter()
            .skip_while(|&&p| p != "class" && p != "struct")
            .nth(1)
            .and_then(|name| name.split(|c| c == '(' || c == ':' || c == '<' || c == '{').next())
            .map(|s| s.to_string())
    }

    fn extract_enum_name(&self, line: &str) -> Option<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        parts.iter()
            .skip_while(|&&p| p != "enum")
            .nth(1)
            .and_then(|name| name.split(|c| c == '{' || c == '<').next())
            .map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_extract_rust_context() {
        let analyzer = AstAnalyzer::new(Language::Rust);
        let code = r#"use std::collections::HashMap;
use serde::Serialize;

fn main() {
    println!("Hello");
}
"#;

        let context = analyzer.extract_context(code, 10);
        assert_eq!(context.len(), 2);
        assert!(context[0].contains("HashMap"));
        assert!(context[1].contains("Serialize"));
    }

    #[test]
    fn test_detect_rust_types() {
        let analyzer = AstAnalyzer::new(Language::Rust);

        assert_eq!(analyzer.detect_chunk_type("fn test() {}"), ChunkType::Function);
        assert_eq!(analyzer.detect_chunk_type("pub fn test() {}"), ChunkType::Function);
        assert_eq!(analyzer.detect_chunk_type("async fn test() {}"), ChunkType::Function);
        assert_eq!(analyzer.detect_chunk_type("struct Data {}"), ChunkType::Struct);
        assert_eq!(analyzer.detect_chunk_type("enum Kind {}"), ChunkType::Enum);
        assert_eq!(analyzer.detect_chunk_type("impl Trait for Type {}"), ChunkType::Impl);
    }

    #[test]
    fn test_extract_function_name() {
        let analyzer = AstAnalyzer::new(Language::Rust);

        let code = "pub async fn process_data(input: &str) -> Result<()> {";
        let name = analyzer.extract_function_name(code);
        assert_eq!(name, Some("process_data".to_string()));
    }

    #[test]
    fn test_extract_class_name() {
        let analyzer = AstAnalyzer::new(Language::Python);

        let code = "class MyDataProcessor:";
        let name = analyzer.extract_class_name(code);
        assert_eq!(name, Some("MyDataProcessor".to_string()));
    }
}
