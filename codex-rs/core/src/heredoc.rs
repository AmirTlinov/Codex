use std::borrow::Cow;

pub use codex_protocol::heredoc::HeredocSummary;
pub use codex_protocol::heredoc::HeredocSummaryLabel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocTarget {
    pub path: String,
    pub append: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeredocAction {
    Write { append: bool },
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeredocMetadata {
    pub command: String,
    pub command_line: String,
    pub action: HeredocAction,
    pub targets: Vec<HeredocTarget>,
    pub line_count: usize,
}

pub static SUPPORTED_HEREDOC_COMMANDS: &[&str] = &[
    "cat",
    "tee",
    "ash",
    "bash",
    "dash",
    "elvish",
    "fish",
    "ksh",
    "pwsh",
    "powershell",
    "sh",
    "xonsh",
    "zsh",
    "bun",
    "deno",
    "node",
    "ts-node",
    "ipython",
    "pypy",
    "pypy3",
    "python",
    "python2",
    "python3",
    "bb",
    "clojure",
    "elixir",
    "erl",
    "escript",
    "go",
    "groovy",
    "julia",
    "lua",
    "luajit",
    "nim",
    "nodejs",
    "perl",
    "php",
    "php8",
    "ruby",
    "scala",
    "swift",
    "duckdb",
    "mongo",
    "mongosh",
    "mysql",
    "mysqlsh",
    "psql",
    "redis-cli",
    "sqlcmd",
    "sqlite3",
    "apply_patch",
    "envsubst",
    "jq",
    "kubectl",
    "yq",
];

pub fn analyze(script: &str) -> Option<HeredocMetadata> {
    let mut lines = script.lines();
    let first_line = lines.next()?.trim();
    let idx = first_line.find("<<")?;
    let command_token = first_line[..idx].split_whitespace().next()?;
    let command = normalize_command_name(command_token);
    if !is_supported_command(&command) {
        return None;
    }

    let terminator = parse_terminator_marker(&first_line[idx + 2..])?;
    if terminator.is_empty() {
        return None;
    }

    let mut line_count = 0usize;
    let mut found_end = false;
    for line in lines.by_ref() {
        if line.trim() == terminator {
            found_end = true;
            break;
        }
        line_count += 1;
    }
    if !found_end {
        return None;
    }

    let targets = parse_destinations(first_line).unwrap_or_default();
    let action = if targets.is_empty() {
        HeredocAction::Run
    } else {
        let append = targets.iter().all(|t| t.append);
        HeredocAction::Write { append }
    };

    Some(HeredocMetadata {
        command,
        command_line: first_line.to_string(),
        action,
        targets,
        line_count,
    })
}

fn is_supported_command(command: &str) -> bool {
    SUPPORTED_HEREDOC_COMMANDS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(command))
}

fn normalize_command_name(raw: &str) -> String {
    raw.rsplit('/')
        .next()
        .map(Cow::from)
        .unwrap_or_else(|| Cow::from(raw))
        .to_lowercase()
}

fn parse_terminator_marker(segment: &str) -> Option<String> {
    let mut rest = segment.trim_start();
    if let Some(stripped) = rest.strip_prefix('-') {
        rest = stripped.trim_start();
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    if first == '\'' || first == '"' {
        let quote = first;
        let mut terminator = String::new();
        for ch in chars {
            if ch == quote {
                break;
            }
            terminator.push(ch);
        }
        Some(terminator)
    } else {
        let mut terminator = first.to_string();
        for ch in chars {
            if ch.is_whitespace() || ch == '>' {
                break;
            }
            terminator.push(ch);
        }
        Some(terminator)
    }
}

fn parse_destinations(line: &str) -> Option<Vec<HeredocTarget>> {
    parse_redirection_destinations(line).or_else(|| parse_tee_destinations(line))
}

fn parse_redirection_destinations(line: &str) -> Option<Vec<HeredocTarget>> {
    let mut in_single = false;
    let mut in_double = false;
    let mut iter = line.char_indices().peekable();
    loop {
        let Some((idx, ch)) = iter.next() else {
            break;
        };
        match ch {
            '\'' => {
                if !in_double {
                    in_single = !in_single;
                }
            }
            '"' => {
                if !in_single {
                    in_double = !in_double;
                }
            }
            '>' if !in_single && !in_double => {
                let is_descriptor = line[..idx]
                    .chars()
                    .rev()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| c.is_ascii_digit());
                if is_descriptor {
                    continue;
                }

                let mut append = false;
                let mut consumed = 1usize;
                while let Some(&(_, next_ch)) = iter.peek() {
                    if next_ch == '>' {
                        append = true;
                        consumed += 1;
                        iter.next();
                    } else {
                        break;
                    }
                }

                let rest = &line[idx + consumed..];
                let Some(parsed) = parse_token(rest) else {
                    continue;
                };
                if parsed.token.starts_with('&') || parsed.token.is_empty() {
                    continue;
                }
                return Some(vec![HeredocTarget {
                    path: parsed.token,
                    append,
                }]);
            }
            _ => {}
        }
    }
    None
}

fn parse_tee_destinations(line: &str) -> Option<Vec<HeredocTarget>> {
    let mut in_single = false;
    let mut in_double = false;
    let mut iter = line.char_indices().peekable();
    loop {
        let Some((idx, ch)) = iter.next() else {
            break;
        };
        match ch {
            '\'' => {
                if !in_double {
                    in_single = !in_single;
                }
            }
            '"' => {
                if !in_single {
                    in_double = !in_double;
                }
            }
            '|' if !in_single && !in_double => {
                let rest = &line[idx + 1..];
                if let Some(targets) = parse_tee_segment(rest) {
                    return Some(targets);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_tee_segment(segment: &str) -> Option<Vec<HeredocTarget>> {
    let mut rest = segment.trim_start();
    if let Some(stripped) = rest.strip_prefix("sudo ") {
        rest = stripped.trim_start();
    }
    if !rest.starts_with("tee") {
        return None;
    }
    rest = &rest["tee".len()..];
    let mut append = false;
    let mut targets: Vec<HeredocTarget> = Vec::new();
    let mut remainder = rest;
    loop {
        let trimmed = remainder.trim_start();
        if trimmed.is_empty()
            || matches!(
                trimmed.chars().next(),
                Some('|') | Some('>') | Some(';') | Some('&')
            )
        {
            break;
        }
        let parsed = parse_token(remainder)?;
        if parsed.token.is_empty() {
            break;
        }
        remainder = parsed.remainder;
        if parsed.token.starts_with('-') {
            if parsed.token == "-a" || parsed.token == "--append" {
                append = true;
            }
            continue;
        }
        if parsed.token == "-" {
            continue;
        }
        targets.push(HeredocTarget {
            path: parsed.token,
            append,
        });
    }
    if targets.is_empty() {
        None
    } else {
        Some(targets)
    }
}

struct ParsedToken<'a> {
    token: String,
    remainder: &'a str,
}

fn parse_token(input: &str) -> Option<ParsedToken<'_>> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let offset = input.len() - trimmed.len();
    let mut token = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                let remainder_idx = idx + ch.len_utf8();
                let remainder = &input[offset + remainder_idx..];
                return Some(ParsedToken { token, remainder });
            }
            _ => {
                token.push(ch);
            }
        }
    }
    if in_single || in_double {
        return None;
    }
    Some(ParsedToken {
        token,
        remainder: "",
    })
}

pub fn summarize(meta: &HeredocMetadata) -> HeredocSummary {
    match meta.action {
        HeredocAction::Run => HeredocSummary {
            label: HeredocSummaryLabel::Run,
            program: Some(meta.command.clone()),
            targets: Vec::new(),
            line_count: Some(meta.line_count),
        },
        HeredocAction::Write { append } => HeredocSummary {
            label: if append {
                HeredocSummaryLabel::Append
            } else {
                HeredocSummaryLabel::Write
            },
            program: None,
            targets: meta.targets.iter().map(|t| t.path.clone()).collect(),
            line_count: Some(meta.line_count),
        },
    }
}

pub fn summarize_script(script: &str) -> Option<HeredocSummary> {
    analyze(script).map(|meta| summarize(&meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn summarize_run_action_sets_program_and_line_count() {
        let meta = HeredocMetadata {
            command: "python".into(),
            command_line: "python <<'PY'".into(),
            action: HeredocAction::Run,
            targets: Vec::new(),
            line_count: 4,
        };

        let summary = summarize(&meta);

        assert_eq!(summary.label, HeredocSummaryLabel::Run);
        assert_eq!(summary.program.as_deref(), Some("python"));
        assert!(summary.targets.is_empty());
        assert_eq!(summary.line_count, Some(4));
    }

    #[test]
    fn summarize_write_action_lists_targets_and_append_flag() {
        let meta = HeredocMetadata {
            command: "cat".into(),
            command_line: "cat <<'EOF' >> /tmp/log".into(),
            action: HeredocAction::Write { append: true },
            targets: vec![
                HeredocTarget {
                    path: "/tmp/log".into(),
                    append: true,
                },
                HeredocTarget {
                    path: "./notes.txt".into(),
                    append: true,
                },
            ],
            line_count: 1,
        };

        let summary = summarize(&meta);

        assert_eq!(summary.label, HeredocSummaryLabel::Append);
        assert!(summary.program.is_none());
        assert_eq!(
            summary.targets,
            vec!["/tmp/log".to_string(), "./notes.txt".to_string()]
        );
        assert_eq!(summary.line_count, Some(1));
    }

    #[test]
    fn summarize_script_extracts_targets_and_line_count() {
        let script = "cat <<'EOF' > ./src/lib.rs\nfn hello() {}\nEOF";

        let summary = summarize_script(script).expect("expected heredoc summary");

        assert_eq!(summary.label, HeredocSummaryLabel::Write);
        assert!(summary.program.is_none());
        assert_eq!(summary.targets, vec!["./src/lib.rs".to_string()]);
        assert_eq!(summary.line_count, Some(1));
    }
}
