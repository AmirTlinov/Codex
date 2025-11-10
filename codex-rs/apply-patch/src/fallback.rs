use crate::SymbolFallbackMode;
use crate::SymbolFallbackStrategy;
use crate::ast::LineColumnRange;
use crate::ast::SymbolPath;
use crate::ast::byte_range_to_line_col;

#[derive(Debug, Clone)]
pub(crate) struct FallbackMatch {
    pub match_index: usize,
    pub location: LineColumnRange,
    pub excerpt: Vec<String>,
    pub strategy: SymbolFallbackStrategy,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FallbackFailure {
    pub reason: String,
    pub excerpt: Vec<String>,
    pub location: Option<LineColumnRange>,
}

pub(crate) fn locate_symbol(
    source: &str,
    symbol_path: &SymbolPath,
    mode: SymbolFallbackMode,
) -> Result<FallbackMatch, FallbackFailure> {
    let needle = symbol_path
        .last()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .ok_or_else(|| FallbackFailure {
            reason: "symbol path is empty".to_string(),
            excerpt: Vec::new(),
            location: None,
        })?;

    let mut scanner = TokenScanner::new(source);
    let mut best: Option<Candidate> = None;
    while let Some(token) = scanner.next_identifier() {
        if token.text != needle {
            continue;
        }
        let candidate = evaluate_candidate(token, scanner.lines(), source, symbol_path);
        best = Some(select_better(best, candidate));
    }

    if let Some(candidate) = best {
        let threshold = match mode {
            SymbolFallbackMode::Ast => 42,
            SymbolFallbackMode::Fuzzy => 14,
            SymbolFallbackMode::Disabled => unreachable!(),
        };
        if candidate.score >= threshold {
            return Ok(candidate.into_match(source, needle));
        }
        if matches!(mode, SymbolFallbackMode::Fuzzy) && candidate.score >= 6 {
            return Ok(candidate.into_match(source, needle));
        }
        let detail = format!(
            "best candidate for '{symbol_path}' scored {} below {mode} threshold",
            candidate.score
        );
        return Err(candidate.into_failure(source, needle, detail));
    }

    if matches!(mode, SymbolFallbackMode::Fuzzy)
        && let Some(idx) = source.find(needle)
    {
        let location = byte_range_to_line_col(idx..idx + needle.len(), source);
        let lines = LineTable::new(source);
        return Ok(FallbackMatch {
            match_index: idx,
            location,
            excerpt: line_excerpt(source, &lines, location.start_line.saturating_sub(1)),
            strategy: SymbolFallbackStrategy::Identifier,
            reason: format!(
                "matched literal '{needle}' at line {} via plain substring search",
                location.start_line
            ),
        });
    }

    Err(FallbackFailure {
        reason: format!("symbol '{symbol_path}' not found"),
        excerpt: Vec::new(),
        location: None,
    })
}

#[derive(Debug, Clone)]
struct Candidate {
    index: usize,
    score: i32,
    reasons: Vec<String>,
    line_index: usize,
    strategy: SymbolFallbackStrategy,
}

impl Candidate {
    fn into_match(self, source: &str, needle: &str) -> FallbackMatch {
        let lines = LineTable::new(source);
        let range = byte_range_to_line_col(self.index..self.index + needle.len(), source);
        FallbackMatch {
            match_index: self.index,
            location: range,
            excerpt: line_excerpt(source, &lines, range.start_line.saturating_sub(1)),
            strategy: self.strategy,
            reason: format!("{} (score {})", self.reasons.join("; "), self.score),
        }
    }

    fn into_failure(self, source: &str, needle: &str, reason: String) -> FallbackFailure {
        let lines = LineTable::new(source);
        FallbackFailure {
            reason,
            excerpt: line_excerpt(source, &lines, self.line_index),
            location: Some(byte_range_to_line_col(
                self.index..self.index + needle.len(),
                source,
            )),
        }
    }
}

fn select_better(existing: Option<Candidate>, candidate: Candidate) -> Candidate {
    match existing {
        None => candidate,
        Some(current) => {
            if candidate.score > current.score {
                candidate
            } else {
                current
            }
        }
    }
}

fn evaluate_candidate(
    token: IdentifierToken,
    lines: &LineTable,
    source: &str,
    symbol_path: &SymbolPath,
) -> Candidate {
    let mut candidate = Candidate {
        index: token.start,
        score: 10,
        reasons: vec![format!("identifier at line {}", token.line + 1)],
        line_index: token.line,
        strategy: SymbolFallbackStrategy::Identifier,
    };

    if let Some(sig) = detect_signature(lines.line_text(token.line, source), &token.text) {
        candidate.score += sig.score;
        candidate.reasons.push(sig.reason);
        candidate.strategy = SymbolFallbackStrategy::Scoped;
    }

    let parents = match_parent_scopes(lines, source, token.line, symbol_path);
    if parents.score > 0 {
        candidate.score += parents.score;
        candidate.reasons.push(parents.reason);
        candidate.strategy = SymbolFallbackStrategy::Scoped;
    }

    candidate
}

struct SignatureHit {
    score: i32,
    reason: String,
}

fn detect_signature(line: &str, needle: &str) -> Option<SignatureHit> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    if contains_keyword_decl(&lower, "fn", &needle_lower)
        || contains_keyword_decl(&lower, "def", &needle_lower)
        || contains_keyword_decl(&lower, "function", &needle_lower)
        || contains_keyword_decl(&lower, "class", &needle_lower)
        || contains_keyword_decl(&lower, "struct", &needle_lower)
        || contains_keyword_decl(&lower, "enum", &needle_lower)
    {
        return Some(SignatureHit {
            score: 32,
            reason: format!("matched declaration '{trimmed}'"),
        });
    }
    if trimmed
        .to_ascii_lowercase()
        .starts_with(&format!("{needle_lower}("))
    {
        return Some(SignatureHit {
            score: 18,
            reason: format!("method '{needle}'"),
        });
    }
    if trimmed.contains(&format!("{needle} =")) {
        return Some(SignatureHit {
            score: 12,
            reason: format!("assignment for '{needle}'"),
        });
    }
    None
}

fn contains_keyword_decl(line: &str, keyword: &str, needle: &str) -> bool {
    let mut search = 0;
    while let Some(pos) = line[search..].find(keyword) {
        let absolute = search + pos;
        if absolute > 0
            && let Some(prev) = line[..absolute].chars().rev().find(|c| !c.is_whitespace())
            && (prev.is_alphanumeric() || prev == '_')
        {
            search = absolute + keyword.len();
            continue;
        }
        let after = line[absolute + keyword.len()..].trim_start();
        if matches_identifier(after, needle) {
            return true;
        }
        search = absolute + keyword.len();
    }
    false
}

fn matches_identifier(fragment: &str, needle: &str) -> bool {
    if fragment.starts_with(needle) && has_identifier_boundary(fragment, needle.len()) {
        return true;
    }
    for separator in ["::", "."] {
        if let Some(idx) = fragment.rfind(separator) {
            let tail = fragment[idx + separator.len()..].trim_start();
            if tail.starts_with(needle) && has_identifier_boundary(tail, needle.len()) {
                return true;
            }
        }
    }
    false
}

fn has_identifier_boundary(fragment: &str, len: usize) -> bool {
    fragment[len..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_')
}

fn qualifier_matches_expected(line: &str, needle: &str, expected: &str) -> bool {
    let line_lower = line.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    if let Some(pos) = line_lower.find(&needle_lower) {
        let prefix = &line_lower[..pos];
        if let Some(actual) = extract_qualifier(prefix) {
            return actual == expected.to_ascii_lowercase();
        }
    }
    true
}

fn extract_qualifier(prefix: &str) -> Option<String> {
    for separator in ["::", "."] {
        if let Some(idx) = prefix.rfind(separator) {
            let before = &prefix[..idx];
            let token = before
                .rsplit(|c: char| c.is_whitespace() || c == ':' || c == '.')
                .next()
                .unwrap_or(before)
                .trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

struct ParentMatch {
    score: i32,
    reason: String,
}

fn match_parent_scopes(
    lines: &LineTable,
    source: &str,
    line_index: usize,
    symbol_path: &SymbolPath,
) -> ParentMatch {
    let mut total = 0;
    let mut parts = Vec::new();
    let mut current = line_index;
    let parents: Vec<&String> = symbol_path.parent_segments().iter().rev().collect();
    for (idx, segment) in parents.iter().enumerate() {
        let expected = parents
            .get(idx + 1)
            .copied()
            .map(std::string::String::as_str);
        if let Some(found) = find_parent_line(lines, source, current, segment, expected) {
            let distance = current.saturating_sub(found);
            let bonus = 8 + (6 - distance.min(6) as i32);
            total += bonus;
            parts.push(format!("parent '{segment}' at line {}", found + 1));
            if found == 0 {
                break;
            }
            current = found;
        } else {
            break;
        }
    }

    if total == 0 {
        return ParentMatch {
            score: 0,
            reason: String::new(),
        };
    }

    ParentMatch {
        score: total,
        reason: parts.join("; "),
    }
}

fn find_parent_line(
    lines: &LineTable,
    source: &str,
    start_line: usize,
    needle: &str,
    expected_qualifier: Option<&str>,
) -> Option<usize> {
    let lower = needle.to_ascii_lowercase();
    let mut line = start_line.saturating_sub(1);
    let mut scanned = 0usize;
    while scanned < 400 {
        let text = lines.line_text(line, source);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let normalized = trimmed.to_ascii_lowercase();
            if contains_keyword_decl(&normalized, "class", &lower)
                || contains_keyword_decl(&normalized, "struct", &lower)
                || contains_keyword_decl(&normalized, "enum", &lower)
                || contains_keyword_decl(&normalized, "impl", &lower)
                || contains_keyword_decl(&normalized, "trait", &lower)
                || contains_keyword_decl(&normalized, "interface", &lower)
                || contains_keyword_decl(&normalized, "module", &lower)
                || contains_keyword_decl(&normalized, "mod", &lower)
            {
                if let Some(expected) = expected_qualifier
                    && !qualifier_matches_expected(trimmed, needle, expected)
                {
                    scanned += 1;
                    if line == 0 {
                        break;
                    }
                    line -= 1;
                    continue;
                }
                return Some(line);
            }
        }
        scanned += 1;
        if line == 0 {
            break;
        }
        line -= 1;
    }
    None
}

#[derive(Clone)]
struct IdentifierToken {
    text: String,
    start: usize,
    line: usize,
}

struct TokenScanner<'a> {
    source: &'a str,
    chars: std::str::CharIndices<'a>,
    lookahead: Option<(usize, char)>,
    lines: LineTable,
}

impl<'a> TokenScanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.char_indices(),
            lookahead: None,
            lines: LineTable::new(source),
        }
    }

    fn lines(&self) -> &LineTable {
        &self.lines
    }

    fn next_identifier(&mut self) -> Option<IdentifierToken> {
        while let Some((idx, ch)) = self.next_char() {
            if is_ident_start(ch) {
                let start = idx;
                let mut end = idx + ch.len_utf8();
                while let Some((next_idx, next_ch)) = self.peek_char() {
                    if is_ident_continue(next_ch) {
                        self.advance();
                        end = next_idx + next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let line = self.lines.line_for_byte(start);
                return Some(IdentifierToken {
                    text: self.source[start..end].to_string(),
                    start,
                    line,
                });
            }
        }
        None
    }

    fn next_char(&mut self) -> Option<(usize, char)> {
        if let Some(peek) = self.lookahead.take() {
            return Some(peek);
        }
        self.chars.next()
    }

    fn peek_char(&mut self) -> Option<(usize, char)> {
        if self.lookahead.is_none() {
            self.lookahead = self.chars.next();
        }
        self.lookahead
    }

    fn advance(&mut self) {
        self.lookahead = None;
    }
}

#[derive(Clone)]
struct LineTable {
    ranges: Vec<LineRange>,
}

#[derive(Clone)]
struct LineRange {
    start: usize,
    end: usize,
}

impl LineTable {
    fn new(source: &str) -> Self {
        let mut ranges = Vec::new();
        let mut start = 0;
        for (idx, ch) in source.char_indices() {
            if ch == '\n' {
                ranges.push(LineRange { start, end: idx });
                start = idx + 1;
            }
        }
        ranges.push(LineRange {
            start,
            end: source.len(),
        });
        Self { ranges }
    }

    fn line_for_byte(&self, idx: usize) -> usize {
        let mut low = 0;
        let mut high = self.ranges.len();
        while low < high {
            let mid = (low + high) / 2;
            let range = &self.ranges[mid];
            if idx < range.start {
                high = mid;
            } else if idx > range.end {
                low = mid + 1;
            } else {
                return mid;
            }
        }
        self.ranges.len().saturating_sub(1)
    }

    fn line_text<'a>(&self, idx: usize, source: &'a str) -> &'a str {
        if self.ranges.is_empty() {
            return "";
        }
        let line = idx.min(self.ranges.len() - 1);
        let range = &self.ranges[line];
        source[range.start..range.end].trim_end_matches('\r')
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn line_excerpt(source: &str, lines: &LineTable, center: usize) -> Vec<String> {
    if lines.ranges.is_empty() {
        return Vec::new();
    }
    let start = center.saturating_sub(2);
    let end = (center + 3).min(lines.ranges.len());
    (start..end)
        .map(|idx| lines.line_text(idx, source).to_string())
        .collect()
}
