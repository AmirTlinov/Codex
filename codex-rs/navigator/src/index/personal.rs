use super::model::SymbolRecord;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const MAX_PLAN_TOKENS: usize = 24;
const MIN_TOKEN_LEN: usize = 4;
const PLAN_TOKEN_BONUS: f32 = 24.0;
const BRANCH_TOKEN_BONUS: f32 = 12.0;
const BRANCH_PATH_BONUS: f32 = 16.0;

const PLAN_FILE_CANDIDATES: &[&str] = &[
    ".agents/current_plan.md",
    ".agents/current_plan.txt",
    ".agents/plan/current_plan.md",
    ".agents/plan/current_plan.txt",
    "plan/current_plan.md",
    "plan/current_plan.txt",
    "PLAN.md",
    "Plan.md",
    "plan.md",
];

const PLAN_STOPWORDS: &[&str] = &[
    "plan",
    "todo",
    "task",
    "tasks",
    "flagship",
    "navigator",
    "codex",
    "agent",
    "agents",
    "context",
    "continue",
    "branch",
    "focus",
];

#[derive(Clone, Debug, Default)]
pub struct PersonalSignals {
    branch_tokens: Vec<String>,
    plan_tokens: Vec<String>,
    focus_paths: HashSet<String>,
}

impl PersonalSignals {
    pub fn load(project_root: &Path) -> Self {
        let branch_tokens = detect_branch_tokens(project_root);
        let plan_tokens = detect_plan_tokens(project_root);
        let focus_paths = detect_branch_focus_paths(project_root);
        Self {
            branch_tokens,
            plan_tokens,
            focus_paths,
        }
    }

    pub fn symbol_bonus(&self, symbol: &SymbolRecord) -> f32 {
        if self.branch_tokens.is_empty()
            && self.plan_tokens.is_empty()
            && self.focus_paths.is_empty()
        {
            return 0.0;
        }
        let path_lower = symbol.path.to_ascii_lowercase();
        let identifier_lower = symbol.identifier.to_ascii_lowercase();
        let preview_lower = symbol.preview.to_ascii_lowercase();
        let mut bonus = 0.0;
        if !self.plan_tokens.is_empty()
            && (contains_any(&path_lower, &self.plan_tokens)
                || contains_any(&identifier_lower, &self.plan_tokens)
                || contains_any(&preview_lower, &self.plan_tokens))
        {
            bonus += PLAN_TOKEN_BONUS;
        }
        if !self.branch_tokens.is_empty() && contains_any(&path_lower, &self.branch_tokens) {
            bonus += BRANCH_TOKEN_BONUS;
        }
        if self
            .focus_paths
            .iter()
            .any(|focus| path_lower == *focus || path_lower.starts_with(&(focus.clone() + "/")))
        {
            bonus += BRANCH_PATH_BONUS;
        }
        bonus
    }

    pub fn literal_bonus(&self, path: &str) -> f32 {
        if self.branch_tokens.is_empty()
            && self.plan_tokens.is_empty()
            && self.focus_paths.is_empty()
        {
            return 0.0;
        }
        let path_lower = path.to_ascii_lowercase();
        let mut bonus = 0.0;
        if !self.plan_tokens.is_empty() && contains_any(&path_lower, &self.plan_tokens) {
            bonus += PLAN_TOKEN_BONUS / 2.0;
        }
        if !self.branch_tokens.is_empty() && contains_any(&path_lower, &self.branch_tokens) {
            bonus += BRANCH_TOKEN_BONUS;
        }
        if self.focus_paths.contains(&path_lower) {
            bonus += BRANCH_PATH_BONUS;
        }
        bonus
    }
}

fn contains_any(haystack: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn detect_branch_tokens(project_root: &Path) -> Vec<String> {
    let branch = run_git(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "head");
    match branch {
        Some(name) => tokenize_branch(&name),
        None => Vec::new(),
    }
}

fn tokenize_branch(branch: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for part in branch.split(['/', '-', '_']) {
        let token = part.trim();
        if token.len() < MIN_TOKEN_LEN {
            continue;
        }
        let normalized = token.to_ascii_lowercase();
        if !tokens.contains(&normalized) {
            tokens.push(normalized);
        }
    }
    tokens
}

fn detect_plan_tokens(project_root: &Path) -> Vec<String> {
    if let Some(env_path) = env::var_os("NAVIGATOR_PLAN_PATH")
        && let Some(text) = read_plan_text(project_root, PathBuf::from(env_path))
    {
        return tokenize_plan(&text);
    }
    for candidate in PLAN_FILE_CANDIDATES {
        let candidate_path = project_root.join(candidate);
        if let Some(text) = read_plan_text(project_root, candidate_path) {
            return tokenize_plan(&text);
        }
    }
    Vec::new()
}

fn read_plan_text(project_root: &Path, path: PathBuf) -> Option<String> {
    let resolved = if path.is_absolute() {
        path
    } else {
        normalize_relative(project_root, path)
    };
    fs::read_to_string(resolved).ok()
}

fn normalize_relative(root: &Path, relative: PathBuf) -> PathBuf {
    if relative.components().any(|c| {
        matches!(
            c,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        relative
    } else {
        root.join(relative)
    }
}

fn tokenize_plan(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let stopwords: HashSet<&str> = PLAN_STOPWORDS.iter().copied().collect();
    for raw in text.split(|ch: char| !ch.is_alphanumeric()) {
        let token = raw.trim();
        if token.len() < MIN_TOKEN_LEN {
            continue;
        }
        let normalized = token.to_ascii_lowercase();
        if stopwords.contains(normalized.as_str()) {
            continue;
        }
        if !tokens.contains(&normalized) {
            tokens.push(normalized);
        }
        if tokens.len() >= MAX_PLAN_TOKENS {
            break;
        }
    }
    tokens
}

fn detect_branch_focus_paths(project_root: &Path) -> HashSet<String> {
    let Some(base) = find_merge_base(project_root) else {
        return HashSet::new();
    };
    let diff_arg = format!("{base}..HEAD");
    let Some(diff_output) = run_git(project_root, &["diff", "--name-only", diff_arg.as_str()])
    else {
        return HashSet::new();
    };
    diff_output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('\\', "/").to_ascii_lowercase())
        .collect()
}

fn find_merge_base(project_root: &Path) -> Option<String> {
    let candidates = [
        vec!["merge-base", "HEAD", "origin/main"],
        vec!["merge-base", "HEAD", "origin/master"],
        vec!["merge-base", "HEAD", "main"],
        vec!["merge-base", "HEAD", "master"],
        vec!["rev-parse", "HEAD^"],
    ];
    for args in candidates {
        if let Some(output) = run_git(project_root, &args) {
            let trimmed = output.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_plan_skips_stopwords_and_short_tokens() {
        let text = "Flagship plan: improve atlas and planner reliability asap";
        let tokens = tokenize_plan(text);
        assert!(tokens.contains(&"improve".to_string()));
        assert!(tokens.contains(&"atlas".to_string()));
        assert!(!tokens.contains(&"plan".to_string()));
        assert!(!tokens.contains(&"and".to_string()));
    }

    #[test]
    fn branch_tokenizes_path_like_names() {
        let tokens = tokenize_branch("feature/navigator-plan_boost");
        assert!(tokens.contains(&"feature".to_string()));
        assert!(tokens.contains(&"navigator".to_string()));
        assert!(tokens.contains(&"plan".to_string()));
    }
}
