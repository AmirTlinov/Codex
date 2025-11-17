use crate::ast_analyzer::AstAnalyzer;
use crate::config::{ChunkerConfig, ChunkingStrategy};
use crate::error::ChunkerError;
use crate::language::Language;
use crate::strategy::{FixedChunkingStrategy, SlidingWindowStrategy, TokenEstimator};
use crate::{CodeChunk, ChunkMetadata, ChunkType};
use log::{debug, info};
use std::path::Path;

/// Main code chunker
pub struct Chunker {
    config: ChunkerConfig,
}

impl Chunker {
    /// Create a new chunker with the given configuration
    pub fn new(config: ChunkerConfig) -> Self {
        if let Err(e) = config.validate() {
            panic!("Invalid chunker configuration: {e}");
        }

        Self { config }
    }

    /// Create a chunker with default configuration
    pub fn default() -> Self {
        Self::new(ChunkerConfig::default())
    }

    /// Chunk a file from disk
    pub fn chunk_file(&self, path: &Path) -> Result<Vec<CodeChunk>, ChunkerError> {
        let content = std::fs::read_to_string(path)?;
        let file_path = path.to_str().unwrap_or("unknown");

        self.chunk_str(&content, Some(file_path))
    }

    /// Chunk a string of code
    pub fn chunk_str(
        &self,
        content: &str,
        file_path: Option<&str>,
    ) -> Result<Vec<CodeChunk>, ChunkerError> {
        let file_path = file_path.unwrap_or("unknown");
        let language = self.detect_language(file_path, content);

        info!(
            "Chunking {} ({} strategy, {} language)",
            file_path,
            match self.config.strategy {
                ChunkingStrategy::Fixed => "fixed",
                ChunkingStrategy::Semantic => "semantic",
                ChunkingStrategy::Adaptive => "adaptive",
                ChunkingStrategy::SlidingWindow => "sliding-window",
            },
            language.name()
        );

        let chunks = match self.config.strategy {
            ChunkingStrategy::Fixed => self.chunk_fixed(content, file_path, language),
            ChunkingStrategy::Semantic => self.chunk_semantic(content, file_path, language),
            ChunkingStrategy::Adaptive => self.chunk_adaptive(content, file_path, language),
            ChunkingStrategy::SlidingWindow => self.chunk_sliding_window(content, file_path, language),
        }?;

        debug!("Generated {} chunks for {}", chunks.len(), file_path);
        Ok(chunks)
    }

    /// Detect language from file path and content
    fn detect_language(&self, file_path: &str, _content: &str) -> Language {
        Language::from_path(Path::new(file_path))
    }

    /// Fixed-size chunking
    fn chunk_fixed(
        &self,
        content: &str,
        file_path: &str,
        language: Language,
    ) -> Result<Vec<CodeChunk>, ChunkerError> {
        let strategy = FixedChunkingStrategy::new(self.config.clone(), language);
        Ok(strategy.chunk(content, file_path))
    }

    /// Sliding window chunking
    fn chunk_sliding_window(
        &self,
        content: &str,
        file_path: &str,
        language: Language,
    ) -> Result<Vec<CodeChunk>, ChunkerError> {
        let strategy = SlidingWindowStrategy::new(self.config.clone(), language);
        Ok(strategy.chunk(content, file_path))
    }

    /// Semantic (AST-based) chunking
    fn chunk_semantic(
        &self,
        content: &str,
        file_path: &str,
        language: Language,
    ) -> Result<Vec<CodeChunk>, ChunkerError> {
        let analyzer = AstAnalyzer::new(language);
        let estimator = TokenEstimator::new(language);

        // Extract context (imports, etc.)
        let context = if self.config.include_context {
            analyzer.extract_context(content, self.config.max_context_lines)
        } else {
            Vec::new()
        };

        // Split into logical blocks
        let blocks = self.split_into_blocks(content, language);

        let mut chunks = Vec::new();

        for block in blocks {
            let chunk_type = analyzer.detect_chunk_type(&block.content);
            let symbol_name = analyzer.extract_symbol_name(&block.content, chunk_type);

            // Add context if needed
            let final_content = if chunk_type.needs_context() && !context.is_empty() {
                let context_str = context.join("\n");
                format!("{}\n\n{}", context_str, block.content)
            } else {
                block.content.clone()
            };

            let estimated_tokens = estimator.estimate(&final_content);

            let metadata = ChunkMetadata {
                language: Some(language.name().to_string()),
                chunk_type: Some(chunk_type),
                symbol_name,
                context_imports: context.clone(),
                parent_scope: None,
                estimated_tokens,
            };

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                start_line: block.start_line,
                end_line: block.end_line,
                content: final_content,
                metadata,
            });
        }

        Ok(chunks)
    }

    /// Adaptive chunking (combines semantic + fixed fallback)
    fn chunk_adaptive(
        &self,
        content: &str,
        file_path: &str,
        language: Language,
    ) -> Result<Vec<CodeChunk>, ChunkerError> {
        // Try semantic chunking first
        let semantic_chunks = self.chunk_semantic(content, file_path, language)?;

        // Check if any chunks are too large
        let estimator = TokenEstimator::new(language);
        let mut final_chunks = Vec::new();

        for chunk in semantic_chunks {
            if chunk.metadata.estimated_tokens <= self.config.max_chunk_tokens {
                // Chunk is within limits, keep it
                final_chunks.push(chunk);
            } else {
                // Chunk is too large, split it using fixed strategy
                debug!(
                    "Chunk too large ({} tokens), splitting with fixed strategy",
                    chunk.metadata.estimated_tokens
                );

                let fixed_strategy = FixedChunkingStrategy::new(self.config.clone(), language);
                let sub_chunks = fixed_strategy.chunk(&chunk.content, file_path);
                final_chunks.extend(sub_chunks);
            }
        }

        Ok(final_chunks)
    }

    /// Split content into logical blocks (functions, classes, etc.)
    fn split_into_blocks(&self, content: &str, language: Language) -> Vec<Block> {
        let lines: Vec<&str> = content.lines().collect();
        let mut blocks = Vec::new();
        let mut current_block: Option<Block> = None;
        let mut brace_depth: i32 = 0;
        let mut paren_depth: i32 = 0;

        for (line_num, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            // Start of a new block
            if self.is_block_start(trimmed, language) {
                // Save previous block if exists
                if let Some(block) = current_block.take() {
                    blocks.push(block);
                }

                // Start new block
                current_block = Some(Block {
                    start_line: line_num + 1,
                    end_line: line_num + 1,
                    content: line.to_string(),
                });

                brace_depth = 0;
                paren_depth = 0;
            } else if let Some(ref mut block) = current_block {
                // Continue current block
                block.content.push('\n');
                block.content.push_str(line);
                block.end_line = line_num + 1;
            }

            // Track braces/parens for block boundaries
            for ch in line.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => {
                        brace_depth = brace_depth.saturating_sub(1);
                        // Block may be complete
                        if brace_depth == 0 && current_block.is_some() {
                            if let Some(block) = current_block.take() {
                                blocks.push(block);
                            }
                        }
                    }
                    '(' => paren_depth += 1,
                    ')' => paren_depth = paren_depth.saturating_sub(1),
                    _ => {}
                }
            }

            // Empty line may signal block end
            if trimmed.is_empty() && brace_depth == 0 && paren_depth == 0 {
                if let Some(block) = current_block.take() {
                    blocks.push(block);
                }
            }
        }

        // Don't forget the last block
        if let Some(block) = current_block {
            blocks.push(block);
        }

        // If no blocks found, treat entire content as one block
        if blocks.is_empty() {
            blocks.push(Block {
                start_line: 1,
                end_line: lines.len(),
                content: content.to_string(),
            });
        }

        blocks
    }

    /// Check if a line starts a new block
    fn is_block_start(&self, line: &str, language: Language) -> bool {
        match language {
            Language::Rust => {
                line.starts_with("fn ")
                    || line.starts_with("pub fn ")
                    || line.starts_with("async fn ")
                    || line.starts_with("pub async fn ")
                    || line.starts_with("struct ")
                    || line.starts_with("pub struct ")
                    || line.starts_with("enum ")
                    || line.starts_with("pub enum ")
                    || line.starts_with("impl ")
                    || line.starts_with("trait ")
                    || line.starts_with("pub trait ")
            }
            Language::Python => {
                line.starts_with("def ")
                    || line.starts_with("async def ")
                    || line.starts_with("class ")
            }
            Language::JavaScript | Language::TypeScript => {
                line.starts_with("function ")
                    || line.starts_with("async function ")
                    || line.starts_with("class ")
                    || line.starts_with("export function ")
                    || line.starts_with("export class ")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct Block {
    start_line: usize,
    end_line: usize,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_chunker_creation() {
        let config = ChunkerConfig::default();
        let chunker = Chunker::new(config);
        assert!(true); // If we get here, creation succeeded
    }

    #[test]
    fn test_chunk_rust_code() {
        let chunker = Chunker::default();

        let code = r#"
use std::collections::HashMap;

fn process_data(input: &str) -> Result<String> {
    let data = parse(input)?;
    Ok(data.to_string())
}

pub struct DataProcessor {
    cache: HashMap<String, String>,
}

impl DataProcessor {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn process(&self, input: &str) -> String {
        input.to_uppercase()
    }
}
"#;

        let chunks = chunker.chunk_str(code, Some("test.rs")).unwrap();
        assert!(!chunks.is_empty());

        // Should have multiple chunks for different items
        assert!(chunks.len() >= 2);

        // Check that functions are detected
        let has_function = chunks.iter().any(|c| {
            matches!(c.metadata.chunk_type, Some(ChunkType::Function | ChunkType::Method))
        });
        assert!(has_function);
    }

    #[test]
    fn test_adaptive_chunking_splits_large_blocks() {
        let mut config = ChunkerConfig::default();
        config.max_chunk_tokens = 100;
        config.strategy = ChunkingStrategy::Adaptive;

        let chunker = Chunker::new(config);

        // Create a very large function
        let large_function = format!(
            "fn large_function() {{\n{}\n}}",
            (0..100).map(|i| format!("    let var_{} = {};", i, i)).collect::<Vec<_>>().join("\n")
        );

        let chunks = chunker.chunk_str(&large_function, Some("test.rs")).unwrap();

        // Should be split into multiple chunks
        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_context_inclusion() {
        let mut config = ChunkerConfig::default();
        config.include_context = true;
        config.strategy = ChunkingStrategy::Semantic;

        let chunker = Chunker::new(config);

        let code = r#"
use serde::Serialize;
use std::fmt;

fn process() {
    println!("test");
}
"#;

        let chunks = chunker.chunk_str(code, Some("test.rs")).unwrap();

        // Find function chunk
        let func_chunk = chunks.iter().find(|c| {
            matches!(c.metadata.chunk_type, Some(ChunkType::Function))
        });

        if let Some(chunk) = func_chunk {
            // Should include context
            assert!(!chunk.metadata.context_imports.is_empty());
        }
    }
}
