use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metadata associated with a code chunk
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ChunkMetadata {
    /// Programming language of the code
    pub language: Option<String>,

    /// Git commit hash when the chunk was indexed
    pub commit_hash: Option<String>,

    /// Timestamp when the chunk was indexed (Unix timestamp)
    pub indexed_at: Option<i64>,

    /// Custom metadata fields
    #[serde(flatten)]
    pub custom: HashMap<String, serde_json::Value>,
}

/// A chunk of code with its location and content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeChunk {
    /// Path to the file containing this chunk
    pub path: String,

    /// Starting line number (1-indexed)
    pub start_line: usize,

    /// Ending line number (1-indexed, inclusive)
    pub end_line: usize,

    /// The actual code content
    pub content: String,

    /// Additional metadata
    #[serde(default)]
    pub metadata: ChunkMetadata,
}

impl CodeChunk {
    /// Create a new code chunk
    pub fn new(
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            start_line,
            end_line,
            content: content.into(),
            metadata: ChunkMetadata::default(),
        }
    }

    /// Create a new code chunk with metadata
    pub fn with_metadata(
        path: impl Into<String>,
        start_line: usize,
        end_line: usize,
        content: impl Into<String>,
        metadata: ChunkMetadata,
    ) -> Self {
        Self {
            path: path.into(),
            start_line,
            end_line,
            content: content.into(),
            metadata,
        }
    }

    /// Get the number of lines in this chunk
    pub fn line_count(&self) -> usize {
        if self.end_line >= self.start_line {
            self.end_line - self.start_line + 1
        } else {
            0
        }
    }

    /// Check if this chunk overlaps with another chunk
    pub fn overlaps_with(&self, other: &CodeChunk) -> bool {
        if self.path != other.path {
            return false;
        }

        // Check for line range overlap
        !(self.end_line < other.start_line || other.end_line < self.start_line)
    }

    /// Merge this chunk with another chunk if they are adjacent or overlapping
    pub fn try_merge(&self, other: &CodeChunk) -> Option<CodeChunk> {
        if self.path != other.path {
            return None;
        }

        // Check if chunks are adjacent or overlapping
        if self.end_line + 1 < other.start_line || other.end_line + 1 < self.start_line {
            return None;
        }

        let new_start = self.start_line.min(other.start_line);
        let new_end = self.end_line.max(other.end_line);

        // Merge content
        let merged_content = if self.start_line <= other.start_line {
            format!("{}\n{}", self.content.trim_end(), other.content.trim_start())
        } else {
            format!("{}\n{}", other.content.trim_end(), self.content.trim_start())
        };

        Some(CodeChunk::new(
            self.path.clone(),
            new_start,
            new_end,
            merged_content,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_chunk_creation() {
        let chunk = CodeChunk::new("test.rs", 1, 5, "fn main() {}");
        assert_eq!(chunk.path, "test.rs");
        assert_eq!(chunk.start_line, 1);
        assert_eq!(chunk.end_line, 5);
        assert_eq!(chunk.line_count(), 5);
    }

    #[test]
    fn test_chunk_line_count() {
        let chunk = CodeChunk::new("test.rs", 10, 20, "code");
        assert_eq!(chunk.line_count(), 11);
    }

    #[test]
    fn test_chunk_overlap_detection() {
        let chunk1 = CodeChunk::new("test.rs", 1, 10, "code1");
        let chunk2 = CodeChunk::new("test.rs", 5, 15, "code2");
        let chunk3 = CodeChunk::new("test.rs", 20, 30, "code3");
        let chunk4 = CodeChunk::new("other.rs", 1, 10, "code4");

        assert!(chunk1.overlaps_with(&chunk2));
        assert!(chunk2.overlaps_with(&chunk1));
        assert!(!chunk1.overlaps_with(&chunk3));
        assert!(!chunk1.overlaps_with(&chunk4));
    }

    #[test]
    fn test_chunk_merge() {
        let chunk1 = CodeChunk::new("test.rs", 1, 5, "line 1\nline 2");
        let chunk2 = CodeChunk::new("test.rs", 6, 10, "line 3\nline 4");

        let merged = chunk1.try_merge(&chunk2).unwrap();
        assert_eq!(merged.start_line, 1);
        assert_eq!(merged.end_line, 10);
        assert!(merged.content.contains("line 1"));
        assert!(merged.content.contains("line 4"));
    }

    #[test]
    fn test_chunk_merge_non_adjacent() {
        let chunk1 = CodeChunk::new("test.rs", 1, 5, "code1");
        let chunk2 = CodeChunk::new("test.rs", 10, 15, "code2");

        assert!(chunk1.try_merge(&chunk2).is_none());
    }

    #[test]
    fn test_chunk_metadata() {
        let mut metadata = ChunkMetadata::default();
        metadata.language = Some("rust".to_string());
        metadata.commit_hash = Some("abc123".to_string());

        let chunk = CodeChunk::with_metadata("test.rs", 1, 5, "code", metadata.clone());
        assert_eq!(chunk.metadata.language, Some("rust".to_string()));
        assert_eq!(chunk.metadata.commit_hash, Some("abc123".to_string()));
    }
}
