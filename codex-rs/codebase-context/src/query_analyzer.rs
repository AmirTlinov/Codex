use crate::context_provider::ContextSearchMetadata;
use crate::error::Result;
use log::debug;
use regex_lite::Regex;

/// Telemetry about what triggered the context lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IntentSignals {
    pub file_mentions: usize,
    pub code_tokens: usize,
    pub stack_traces: usize,
    pub error_terms: usize,
    pub explicit_search_keywords: usize,
    pub question_keywords: usize,
    pub non_english_keywords: usize,
    pub code_blocks: usize,
    pub patch_markers: usize,
    pub metadata_file_hints: usize,
}

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

    /// Richer signal breakdown for observability/tests
    pub signals: IntentSignals,
}

/// Analyzer for extracting search intent from user messages
pub struct QueryAnalyzer {
    // Pre-compiled regexes
    file_pattern: Regex,
    code_pattern: Regex,
    stack_pattern: Regex,
    error_pattern: Regex,
    classifier: IntentClassifier,
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
            file_pattern: Regex::new(r"[\w./\\-]+\.\w{1,6}").expect("Valid regex"),
            // Matches code-related terms (english keywords for now)
            code_pattern: Regex::new(
                r"\b(function|class|method|struct|impl|trait|interface|async|await|error|handle|test|panic|module)\b",
            )
            .expect("Valid regex"),
            // Detect stack-trace like entries (paths with line numbers or `at …`)
            stack_pattern: Regex::new(
                r"(?m)(?:^|\s)(?:at\s+[\w./\\:<>-]+|[\w./\\-]+\.(?:rs|py|ts|tsx|js|jsx|go|java|kt|cs|rb|cpp|c|rb|php):\d{1,5})",
            )
            .expect("Valid regex"),
            // Match error/panic terms in multiple languages
            error_pattern: Regex::new(
                r"\b(error|panic|fail|exception|traceback|ошибка|паника|исключение)\b",
            )
            .expect("Valid regex"),
            classifier: IntentClassifier::default(),
        }
    }

    /// Analyze user message and extract search intent
    pub fn analyze(
        &self,
        message: &str,
        metadata: Option<&ContextSearchMetadata>,
    ) -> Result<SearchIntent> {
        debug!("Analyzing query: '{}'", message);

        let message_lower = message.to_lowercase();

        let mut files: Vec<String> = self
            .file_pattern
            .find_iter(message)
            .map(|m| m.as_str().to_string())
            .collect();
        let message_file_mentions = files.len();
        if let Some(meta) = metadata {
            for path in &meta.recent_file_paths {
                if !files.iter().any(|existing| existing == path) {
                    files.push(path.clone());
                }
            }
        }

        // Extract programming concepts
        let concepts: Vec<String> = self
            .code_pattern
            .find_iter(&message_lower)
            .map(|m| m.as_str().to_string())
            .collect();

        let stack_paths = self.extract_stack_traces(message);
        let error_terms = self.extract_error_terms(&message_lower);
        let non_english_hits = Self::non_english_keywords()
            .iter()
            .filter(|kw| message_lower.contains(*kw))
            .count();
        let question_keywords = Self::question_keywords()
            .iter()
            .filter(|kw| message_lower.contains(*kw))
            .count();
        let explicit_search_keywords = Self::search_keywords()
            .iter()
            .filter(|kw| message_lower.contains(*kw))
            .count();
        let code_block_markers = message.matches("```").count();
        let patch_markers = ["*** begin patch", "diff --git", "apply_patch"]
            .iter()
            .filter(|kw| message_lower.contains(*kw))
            .count();
        let metadata_file_hints = files.len().saturating_sub(message_file_mentions);

        let stack_count = stack_paths.len();
        let error_count = error_terms.len();
        let concept_mentions = concepts.len();

        let signals = IntentSignals {
            file_mentions: message_file_mentions,
            code_tokens: concept_mentions,
            stack_traces: stack_count,
            error_terms: error_count,
            explicit_search_keywords,
            question_keywords,
            non_english_keywords: non_english_hits,
            code_blocks: code_block_markers,
            patch_markers,
            metadata_file_hints,
        };

        let features = IntentFeatures::from_message(&signals, message);
        let confidence = self.classifier.score(&features);

        // Determine if search should be triggered (model + safety fallbacks)
        let should_search = confidence >= 0.45
            || signals.file_mentions > 0
            || signals.stack_traces > 0
            || signals.error_terms > 0
            || signals.explicit_search_keywords > 0
            || signals.patch_markers > 0
            || signals.metadata_file_hints > 0
            || signals.code_blocks > 0;

        // Generate search query
        let query = self.generate_query(
            message,
            &files,
            &concepts,
            &stack_paths,
            &error_terms,
            metadata,
        );

        Ok(SearchIntent {
            query,
            concepts,
            files,
            confidence,
            should_search,
            signals,
        })
    }

    /// Generate optimized search query
    fn generate_query(
        &self,
        message: &str,
        files: &[String],
        concepts: &[String],
        stack_paths: &[String],
        error_terms: &[String],
        metadata: Option<&ContextSearchMetadata>,
    ) -> String {
        if !files.is_empty() {
            return files.join(" ");
        }

        let mut query_parts = Vec::new();
        if !stack_paths.is_empty() {
            query_parts.extend(stack_paths.iter().cloned());
        }

        if !concepts.is_empty() {
            query_parts.extend(concepts.iter().cloned());
        }

        if !error_terms.is_empty() {
            query_parts.extend(error_terms.iter().cloned());
        }

        if query_parts.is_empty() {
            if let Some(meta) = metadata {
                if !meta.recent_file_paths.is_empty() {
                    query_parts.extend(meta.recent_file_paths.iter().take(4).cloned());
                }
                if query_parts.is_empty() && !meta.recent_terms.is_empty() {
                    query_parts.extend(meta.recent_terms.iter().take(4).cloned());
                }
            }
        }

        let technical_stop_words = [
            "what",
            "where",
            "which",
            "about",
            "should",
            "could",
            "would",
            "please",
            "можно",
            "нужно",
            "как",
            "где",
        ];

        let mut buffer = String::new();
        for token in message.split_whitespace() {
            let cleaned =
                token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            if cleaned.len() < 3 {
                continue;
            }
            let lower = cleaned.to_lowercase();
            if technical_stop_words.contains(&lower.as_str()) {
                continue;
            }
            if lower.chars().all(|c| c.is_numeric()) {
                continue;
            }
            buffer.push_str(cleaned);
            buffer.push(' ');
        }

        if !buffer.is_empty() {
            query_parts.push(buffer.trim().to_string());
        }

        if !query_parts.is_empty() {
            query_parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::from(message.trim())
        }
    }

    fn extract_stack_traces(&self, message: &str) -> Vec<String> {
        self.stack_pattern
            .find_iter(message)
            .filter_map(|m| {
                let raw = m.as_str().trim();
                if raw.starts_with("at ") {
                    let trimmed = raw.trim_start_matches("at ").trim();
                    // Keep last segment to avoid leaking full stack trace
                    trimmed
                        .split_whitespace()
                        .last()
                        .map(|segment| segment.trim_matches(|c| c == '(' || c == ')').to_string())
                } else {
                    Some(raw.trim().to_string())
                }
            })
            .map(|entry| entry.replace("\\", "/"))
            .collect()
    }

    fn extract_error_terms(&self, message: &str) -> Vec<String> {
        self.error_pattern
            .find_iter(message)
            .map(|m| m.as_str().to_string())
            .collect()
    }

    fn search_keywords() -> &'static [&'static str] {
        static KEYWORDS: &[&str] = &[
            "find",
            "search",
            "look for",
            "show me",
            "locate",
            "найди",
            "покажи",
            "ищи",
        ];
        KEYWORDS
    }

    fn question_keywords() -> &'static [&'static str] {
        static KEYWORDS: &[&str] = &[
            "how",
            "what",
            "where",
            "which",
            "when",
            "почему",
            "как",
            "где",
            "который",
        ];
        KEYWORDS
    }

    fn non_english_keywords() -> &'static [&'static str] {
        static KEYWORDS: &[&str] = &[
            "файл",
            "функ",
            "метод",
            "класс",
            "тест",
            "ошибка",
            "строка",
            "как",
            "где",
        ];
        KEYWORDS
    }
}

#[derive(Debug, Clone, Copy)]
struct IntentClassifier {
    bias: f32,
    w_file: f32,
    w_stack: f32,
    w_error: f32,
    w_explicit: f32,
    w_question: f32,
    w_code: f32,
    w_non_english: f32,
    w_code_block: f32,
    w_patch: f32,
    w_length: f32,
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self {
            bias: -0.9,
            w_file: 1.25,
            w_stack: 1.1,
            w_error: 0.85,
            w_explicit: 0.75,
            w_question: 0.45,
            w_code: 0.25,
            w_non_english: 0.35,
            w_code_block: 0.35,
            w_patch: 0.55,
            w_length: 0.2,
        }
    }
}

impl IntentClassifier {
    fn score(&self, features: &IntentFeatures) -> f32 {
        let mut z = self.bias;
        z += self.w_file * features.file_mentions;
        z += self.w_stack * features.stack_traces;
        z += self.w_error * features.error_terms;
        z += self.w_explicit * features.explicit_terms;
        z += self.w_question * features.question_terms;
        z += self.w_code * features.code_tokens;
        z += self.w_non_english * features.non_english_terms;
        z += self.w_code_block * features.code_blocks;
        z += self.w_patch * features.patch_markers;
        z += self.w_length * features.length_bucket;
        sigmoid(z)
    }
}

#[derive(Debug, Clone, Copy)]
struct IntentFeatures {
    file_mentions: f32,
    stack_traces: f32,
    error_terms: f32,
    explicit_terms: f32,
    question_terms: f32,
    code_tokens: f32,
    non_english_terms: f32,
    code_blocks: f32,
    patch_markers: f32,
    length_bucket: f32,
}

impl IntentFeatures {
    fn from_message(signals: &IntentSignals, message: &str) -> Self {
        let len_bucket = (message.chars().count() as f32 / 400.0).min(1.5);
        let file_signal = (signals.file_mentions as f32).min(3.0)
            + ((signals.metadata_file_hints as f32).min(3.0) * 0.5);
        Self {
            file_mentions: file_signal,
            stack_traces: (signals.stack_traces as f32).min(2.0),
            error_terms: (signals.error_terms as f32).min(2.0),
            explicit_terms: (signals.explicit_search_keywords as f32).min(2.0),
            question_terms: (signals.question_keywords as f32).min(2.0),
            code_tokens: (signals.code_tokens as f32 / 4.0).min(1.5),
            non_english_terms: (signals.non_english_keywords as f32).min(2.0),
            code_blocks: (signals.code_blocks as f32).min(2.0),
            patch_markers: (signals.patch_markers as f32).min(2.0),
            length_bucket: len_bucket,
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_file_detection() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer
            .analyze("Look at src/main.rs and lib.rs", None)
            .unwrap();

        assert!(intent.should_search);
        assert_eq!(intent.files.len(), 2);
        assert!(intent.files.contains(&"src/main.rs".to_string()));
        assert_eq!(intent.signals.file_mentions, 2);
    }

    #[test]
    fn test_concept_detection() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer
            .analyze("How do I implement async error handling?", None)
            .unwrap();

        assert!(intent.should_search);
        assert!(!intent.concepts.is_empty());
        assert!(intent.concepts.contains(&"async".to_string()));
        assert!(intent.concepts.contains(&"error".to_string()));
        assert!(intent.confidence > 0.5);
        assert!(intent.signals.question_keywords > 0);
    }

    #[test]
    fn test_no_search_trigger() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer.analyze("Thanks for the help!", None).unwrap();

        assert!(!intent.should_search);
        assert!(intent.confidence < 0.5);
        assert_eq!(intent.signals.file_mentions, 0);
    }

    #[test]
    fn test_explicit_search() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer.analyze("Find all test functions", None).unwrap();

        assert!(intent.should_search);
        assert!(intent.concepts.contains(&"test".to_string()));
        assert!(intent.signals.explicit_search_keywords > 0);
    }

    #[test]
    fn test_stack_trace_triggers_search() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer
            .analyze(
                "thread 'main' panicked at 'boom', src/lib.rs:42:10\n  at crate::demo::run (src/lib.rs:42)",
                None,
            )
            .unwrap();

        assert!(intent.should_search, "stack traces should trigger search");
        assert!(intent.query.contains("src/lib.rs"));
        assert!(intent.signals.stack_traces >= 1);
        assert!(intent.confidence >= 0.6);
    }

    #[test]
    fn test_russian_keywords_supported() {
        let analyzer = QueryAnalyzer::new();
        let intent = analyzer
            .analyze("как исправить ошибка в файле src/lib.rs при тесте?", None)
            .unwrap();

        assert!(intent.should_search);
        assert!(intent.query.contains("src/lib.rs"));
        assert!(
            intent.signals.non_english_keywords > 0,
            "signals={:?}",
            intent.signals
        );
        assert!(intent.confidence >= 0.5);
    }

    #[test]
    fn test_metadata_file_hint_triggers_search() {
        let analyzer = QueryAnalyzer::new();
        let metadata = ContextSearchMetadata {
            cwd: None,
            recent_file_paths: vec!["src/lib.rs".to_string()],
            recent_terms: vec!["apply_patch".to_string()],
        };
        let intent = analyzer.analyze("continue", Some(&metadata)).unwrap();

        assert!(intent.should_search);
        assert!(intent.query.contains("src/lib.rs"));
        assert!(intent.signals.metadata_file_hints > 0);
    }
}
