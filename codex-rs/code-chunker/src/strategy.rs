use crate::config::{ChunkerConfig, ChunkingStrategy};
use crate::language::Language;
use crate::{CodeChunk, ChunkMetadata, ChunkType};

/// Token estimator for different languages
pub struct TokenEstimator {
    language: Language,
}

impl TokenEstimator {
    pub fn new(language: Language) -> Self {
        Self { language }
    }

    /// Estimate token count for a string
    pub fn estimate(&self, text: &str) -> usize {
        let line_count = text.lines().count();
        let avg_tokens = self.language.avg_tokens_per_line();
        (line_count as f32 * avg_tokens).ceil() as usize
    }

    /// Estimate token count for a line range
    pub fn estimate_lines(&self, line_count: usize) -> usize {
        (line_count as f32 * self.language.avg_tokens_per_line()).ceil() as usize
    }
}

/// Fixed-size chunking strategy
pub struct FixedChunkingStrategy {
    config: ChunkerConfig,
    language: Language,
}

impl FixedChunkingStrategy {
    pub fn new(config: ChunkerConfig, language: Language) -> Self {
        Self { config, language }
    }

    /// Chunk code into fixed-size chunks
    pub fn chunk(&self, content: &str, file_path: &str) -> Vec<CodeChunk> {
        let lines: Vec<&str> = content.lines().collect();
        let estimator = TokenEstimator::new(self.language);

        let mut chunks = Vec::new();
        let mut current_start = 0;

        while current_start < lines.len() {
            let mut current_end = current_start;
            let mut current_tokens = 0;

            // Accumulate lines until we reach target token count
            while current_end < lines.len() {
                let line_tokens = estimator.estimate(lines[current_end]);

                if current_tokens + line_tokens > self.config.max_chunk_tokens
                    && current_tokens >= self.config.min_chunk_tokens
                {
                    break;
                }

                current_tokens += line_tokens;
                current_end += 1;

                if current_tokens >= self.config.target_chunk_tokens {
                    break;
                }
            }

            // Ensure we make progress
            if current_end == current_start {
                current_end = current_start + 1;
            }

            let chunk_content = lines[current_start..current_end].join("\n");
            let metadata = ChunkMetadata {
                language: Some(self.language.name().to_string()),
                chunk_type: Some(ChunkType::Other),
                estimated_tokens: current_tokens,
                ..Default::default()
            };

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                start_line: current_start + 1,
                end_line: current_end,
                content: chunk_content,
                metadata,
            });

            current_start = current_end;
        }

        chunks
    }
}

/// Sliding window chunking strategy with overlap
pub struct SlidingWindowStrategy {
    config: ChunkerConfig,
    language: Language,
}

impl SlidingWindowStrategy {
    pub fn new(config: ChunkerConfig, language: Language) -> Self {
        Self { config, language }
    }

    /// Chunk code using sliding window with overlap
    pub fn chunk(&self, content: &str, file_path: &str) -> Vec<CodeChunk> {
        let lines: Vec<&str> = content.lines().collect();
        let estimator = TokenEstimator::new(self.language);

        // Calculate overlap in lines
        let overlap_lines = match self.config.overlap {
            crate::config::OverlapStrategy::None => 0,
            crate::config::OverlapStrategy::FixedLines { overlap_lines } => overlap_lines,
            crate::config::OverlapStrategy::FixedTokens { overlap_tokens } => {
                (overlap_tokens as f32 / self.language.avg_tokens_per_line()).ceil() as usize
            }
            crate::config::OverlapStrategy::Semantic { overlap_tokens } => {
                (overlap_tokens as f32 / self.language.avg_tokens_per_line()).ceil() as usize
            }
        };

        let mut chunks = Vec::new();
        let mut current_start = 0;

        while current_start < lines.len() {
            let mut current_end = current_start;
            let mut current_tokens = 0;

            // Accumulate lines for this window
            while current_end < lines.len() && current_tokens < self.config.target_chunk_tokens {
                current_tokens += estimator.estimate(lines[current_end]);
                current_end += 1;
            }

            let chunk_content = lines[current_start..current_end].join("\n");
            let metadata = ChunkMetadata {
                language: Some(self.language.name().to_string()),
                chunk_type: Some(ChunkType::Other),
                estimated_tokens: current_tokens,
                ..Default::default()
            };

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                start_line: current_start + 1,
                end_line: current_end,
                content: chunk_content,
                metadata,
            });

            // Slide window with overlap
            let step = if overlap_lines > 0 {
                (current_end - current_start).saturating_sub(overlap_lines)
            } else {
                current_end - current_start
            };

            current_start += step.max(1);
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_token_estimator() {
        let estimator = TokenEstimator::new(Language::Rust);
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let tokens = estimator.estimate(code);
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    #[test]
    fn test_fixed_chunking() {
        let mut config = ChunkerConfig::default();
        config.target_chunk_tokens = 50;
        config.max_chunk_tokens = 80;

        let strategy = FixedChunkingStrategy::new(config, Language::Rust);

        let code = (0..20).map(|i| format!("fn func_{}() {{}}", i)).collect::<Vec<_>>().join("\n");

        let chunks = strategy.chunk(&code, "test.rs");
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.metadata.estimated_tokens <= 80));
    }

    #[test]
    fn test_sliding_window_with_overlap() {
        let mut config = ChunkerConfig::default();
        config.target_chunk_tokens = 30;
        config.overlap = crate::config::OverlapStrategy::FixedLines { overlap_lines: 2 };

        let strategy = SlidingWindowStrategy::new(config, Language::Python);

        let code = (0..10).map(|i| format!("def func_{}():\n    pass", i)).collect::<Vec<_>>().join("\n");

        let chunks = strategy.chunk(&code, "test.py");
        assert!(chunks.len() > 1);

        // Check that there's overlap between consecutive chunks
        if chunks.len() > 1 {
            let overlap_exists = chunks[0].end_line >= chunks[1].start_line;
            assert!(overlap_exists || chunks[0].end_line == chunks[1].start_line - 1);
        }
    }
}
