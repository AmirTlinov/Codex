use crate::error::Result;
use log::debug;
use regex_lite::Regex;

/// Extracted search intent from user query
#[derive(Debug, Clone)]
pub struct SearchIntent {
    /// Main search query
    pub query: String,

    /// Detected programming concepts/keywords
    pub concepts: Vec<String>,

    /// Mentioned file paths or patterns
    pub files: Vec<String>,

    /// Confidence score (0.0-1.0)
    pub confidence: f32,

    /// Should search be triggered?
    pub should_search: bool,
}

/// Analyzer for extracting search intent from user messages
pub struct QueryAnalyzer {
    // Pre-compiled regexes
    file_pattern: Regex,
    code_pattern: Regex,
}

impl Default for QueryAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryAnalyzer {
    /// Create new query analyzer
    pub fn new() -> Self {
        Self {
            // Matches file paths: src/main.rs, path/to/file.py
            file_pattern: Regex::new(r"[\w/]+\.\w{1,4}").expect("Valid regex"),
            // Matches code-related terms
            code_pattern: Regex::new(
                r"\b(function|class|method|struct|impl|trait|interface|async|error|handle|test)\b",
            )
            .expect("Valid regex"),
        }
    }

    /// Analyze user message and extract search intent
    pub fn analyze(&self, message: &str) -> Result<SearchIntent> {
        debug!("Analyzing query: '{}'", message);

        let message_lower = message.to_lowercase();

        // Extract mentioned files
        let files: Vec<String> = self
            .file_pattern
            .find_iter(message)
            .map(|m| m.as_str().to_string())
            .collect();

        // Extract programming concepts
        let concepts: Vec<String> = self
            .code_pattern
            .find_iter(&message_lower)
            .map(|m| m.as_str().to_string())
            .collect();

        // Determine if search should be triggered
        let should_search = self.should_trigger_search(message, &files, &concepts);

        // Calculate confidence
        let confidence = self.calculate_confidence(&message_lower, &files, &concepts);

        // Generate search query
        let query = self.generate_query(message, &files, &concepts);

        Ok(SearchIntent {
            query,
            concepts,
            files,
            confidence,
            should_search,
        })
    }

    /// Determine if codebase search should be triggered
    fn should_trigger_search(&self, message: &str, files: &[String], concepts: &[String]) -> bool {
        // Trigger if:
        // 1. Files are mentioned
        if !files.is_empty() {
            return true;
        }

        // 2. Code concepts are mentioned with "how", "what", "where"
        if !concepts.is_empty() {
            let has_question_word = message.to_lowercase().contains("how")
                || message.to_lowercase().contains("what")
                || message.to_lowercase().contains("where")
                || message.to_lowercase().contains("which");

            if has_question_word {
                return true;
            }
        }

        // 3. Explicit search keywords
        let search_keywords = ["find", "search", "look for", "show me", "locate"];
        if search_keywords
            .iter()
            .any(|kw| message.to_lowercase().contains(kw))
        {
            return true;
        }

        false
    }

    /// Calculate confidence score for search
    fn calculate_confidence(&self, message: &str, files: &[String], concepts: &[String]) -> f32 {
        let mut confidence = 0.3; // Base confidence

        // Boost for files
        if !files.is_empty() {
            confidence += 0.3;
        }

        // Boost for concepts
        confidence += (concepts.len() as f32 * 0.1).min(0.3);

        // Boost for question structure
        if message.contains('?') {
            confidence += 0.1;
        }

        confidence.min(1.0)
    }

    /// Generate optimized search query
    fn generate_query(&self, message: &str, files: &[String], concepts: &[String]) -> String {
        // If files mentioned, focus on those
        if !files.is_empty() {
            return files.join(" ");
        }

        // Otherwise, combine concepts and key terms
        let mut query_parts = Vec::new();

        // Add concepts
        query_parts.extend(concepts.iter().cloned());

        // Add key nouns from message (simplified extraction)
        let words: Vec<&str> = message.split_whitespace().collect();
        for word in words {
            let word_lower = word.to_lowercase();
            // Skip common words
            if word.len() > 4
                && !["what", "where", "which", "about", "should", "could", "would"]
                    .contains(&word_lower.as_str())
            {
                query_parts.push(word.to_string());
            }
        }

        query_parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_file_detection() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer.analyze("Look at src/main.rs and lib.rs").unwrap();

        assert!(intent.should_search);
        assert_eq!(intent.files.len(), 2);
        assert!(intent.files.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_concept_detection() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer
            .analyze("How do I implement async error handling?")
            .unwrap();

        assert!(intent.should_search);
        assert!(!intent.concepts.is_empty());
        assert!(intent.concepts.contains(&"async".to_string()));
        assert!(intent.concepts.contains(&"error".to_string()));
    }

    #[test]
    fn test_no_search_trigger() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer.analyze("Thanks for the help!").unwrap();

        assert!(!intent.should_search);
        assert!(intent.confidence < 0.5);
    }

    #[test]
    fn test_explicit_search() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer.analyze("Find all test functions").unwrap();

        assert!(intent.should_search);
        assert!(intent.concepts.contains(&"test".to_string()));
    }
}
