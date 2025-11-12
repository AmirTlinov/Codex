use shlex::split as shlex_split;
use std::path::Path;

const MAX_LABEL_CHARS: usize = 120;

pub fn friendly_command_label_from_args(args: &[String]) -> String {
    let core_args = strip_shell_wrapper(args);
    let mut tokens: Vec<String> = expand_single_script_token(core_args);
    remove_set_prolog(&mut tokens);
    normalize_semicolons(&mut tokens);
    if tokens.is_empty() {
        return String::new();
    }
    let joined = join_tokens(&tokens);
    finalize_label(unquote_shell_operators(&joined))
}

pub fn friendly_command_label_from_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match shlex_split(trimmed) {
        Some(tokens) if !tokens.is_empty() => Some(friendly_command_label_from_args(&tokens)),
        _ => Some(finalize_label(trimmed.to_string())),
    }
}

fn strip_shell_wrapper(args: &[String]) -> &[String] {
    let mut slice = args;
    if slice
        .first()
        .map(|token| is_env_launcher(token))
        .unwrap_or(false)
        && slice.len() > 1
    {
        slice = &slice[1..];
    }
    if slice.len() >= 3
        && slice
            .first()
            .map(|token| is_shell_launcher(token))
            .unwrap_or(false)
        && matches!(slice[1].as_str(), "-c" | "-lc")
    {
        &slice[2..]
    } else {
        slice
    }
}

fn expand_single_script_token(tokens: &[String]) -> Vec<String> {
    if tokens.len() == 1
        && tokens
            .first()
            .map(|token| token.chars().any(char::is_whitespace))
            .unwrap_or(false)
        && let Some(expanded) = shlex_split(&tokens[0])
        && !expanded.is_empty()
    {
        return expanded;
    }
    tokens.to_vec()
}

fn remove_set_prolog(tokens: &mut Vec<String>) {
    loop {
        let Some(first) = tokens.first() else {
            return;
        };
        if !first.eq_ignore_ascii_case("set") {
            return;
        }
        tokens.remove(0);
        while let Some(token) = tokens.first().cloned() {
            if let Some(pos) = token.find(';') {
                let remainder = token[pos + 1..].trim().to_string();
                tokens.remove(0);
                if !remainder.is_empty() {
                    tokens.insert(0, remainder);
                }
                break;
            } else {
                tokens.remove(0);
                if tokens.is_empty() {
                    return;
                }
            }
        }
    }
}

fn normalize_semicolons(tokens: &mut Vec<String>) {
    if tokens.is_empty() {
        return;
    }
    let mut normalized: Vec<String> = Vec::with_capacity(tokens.len());
    for token in tokens.drain(..) {
        if token == ";" {
            normalized.push(token);
            continue;
        }
        let mut trimmed = token.as_str();
        let mut semicolons = 0;
        while trimmed.ends_with(';') {
            semicolons += 1;
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        let trimmed = trimmed.trim();
        if !trimmed.is_empty() {
            normalized.push(trimmed.to_string());
        }
        for _ in 0..semicolons {
            normalized.push(";".to_string());
        }
    }
    tokens.extend(normalized);
}

fn join_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .filter(|token| !token.trim().is_empty())
        .map(|token| {
            if is_shell_operator(token) {
                token.clone()
            } else if should_quote_token(token) {
                quote_token(token)
            } else {
                token.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn should_quote_token(token: &str) -> bool {
    token.chars().any(char::is_whitespace) && !looks_like_shell_clause(token)
}

fn looks_like_shell_clause(token: &str) -> bool {
    const CLAUSE_PREFIXES: [&str; 9] = [
        "for ",
        "if ",
        "elif ",
        "else",
        "while ",
        "until ",
        "case ",
        "select ",
        "function ",
    ];
    let trimmed = token.trim_start();
    CLAUSE_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
        || token.contains("$(")
        || token.contains("${")
        || token.contains(';')
        || token.contains("&&")
        || token.contains("||")
        || token.contains('|')
        || token.contains('\n')
}

fn quote_token(token: &str) -> String {
    let mut escaped = String::with_capacity(token.len() + 2);
    escaped.push('"');
    for ch in token.chars() {
        match ch {
            '"' => {
                escaped.push('\\');
                escaped.push('"');
            }
            '\\' => {
                escaped.push('\\');
                escaped.push('\\');
            }
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn is_shell_operator(token: &str) -> bool {
    matches!(token, "&&" | "||" | "|" | ";" | "&")
}

fn is_env_launcher(token: &str) -> bool {
    let name = Path::new(token)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(token);
    matches!(name, "env")
}

fn is_shell_launcher(token: &str) -> bool {
    let name = Path::new(token)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(token);
    matches!(name, "bash" | "zsh" | "sh")
}

fn finalize_label(raw: String) -> String {
    let mut text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    while let Some(stripped) = strip_wrapping_quotes(text) {
        text = stripped.trim();
    }
    text = strip_common_prologs(text);
    let collapsed = collapse_spaces(text);
    truncate_with_ellipsis(&collapsed, MAX_LABEL_CHARS)
}

fn strip_wrapping_quotes(text: &str) -> Option<&str> {
    let mut chars = text.chars();
    let start = chars.next()?;
    let end = text.chars().last()?;
    if (start == '"' || start == '\'' || start == '`') && start == end {
        let inner = &text[1..text.len() - 1];
        if !inner.contains(start) {
            return Some(inner);
        }
    }
    None
}

fn strip_common_prologs(text: &str) -> &str {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("set ")
        && let Some(idx) = trimmed.find(';')
    {
        return trimmed[idx + 1..].trim_start();
    }
    trimmed
}

fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut result = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            result.push_str(" …");
            return result;
        }
        result.push(ch);
    }
    result
}

fn unquote_shell_operators(label: &str) -> String {
    const OPS: [&str; 4] = ["&&", "||", "|", ";"];
    let mut out = label.to_string();
    for op in OPS {
        let needle = format!("'{op}'");
        out = out.replace(&needle, op);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_shell_wrapper_and_quotes() {
        let args = vec!["bash".into(), "-lc".into(), "npm start".into()];
        let label = friendly_command_label_from_args(&args);
        assert_eq!(label, "npm start");
    }

    #[test]
    fn parses_raw_string_with_and_without_wrapper() {
        let raw = "bash -lc npm run dev && tail -f";
        let label = friendly_command_label_from_str(raw).expect("label");
        assert_eq!(label, "npm run dev && tail -f", "label: {label}");
    }

    #[test]
    fn truncates_long_labels() {
        let raw = "set -euo pipefail; printf 'hello world'; sleep 10";
        let label = friendly_command_label_from_str(raw).expect("label");
        assert!(
            label.starts_with("printf \"hello world\""),
            "label: {label}"
        );
    }

    #[test]
    fn renders_single_argument_scripts_readably() {
        let args = vec![
            "bash".into(),
            "-lc".into(),
            "for i in $(seq 5 -1 0); do printf \"remaining %02d\\r\" \"$i\"; sleep 1; done".into(),
        ];
        let label = friendly_command_label_from_args(&args);
        assert_eq!(
            label,
            "for i in $(seq 5 -1 0) ; do printf \"remaining %02d\\\\r\" $i ; sleep 1 ; done"
        );
    }

    #[test]
    fn normalizes_kill_descriptions_with_quotes() {
        let raw = "'for i in $(seq 5 -1 0)' ; \"printf done\"";
        let label = friendly_command_label_from_str(raw).expect("label");
        assert_eq!(label, "for i in $(seq 5 -1 0) ; \"printf done\"");
    }
}
