use crate::planner::NavigatorSearchArgs;
use crate::proto::SearchProfile;

pub fn summarize_args(args: &NavigatorSearchArgs) -> Option<String> {
    let mut summary = args
        .query
        .clone()
        .or_else(|| args.symbol_exact.clone())
        .or_else(|| args.help_symbol.clone())
        .or_else(|| args.path_globs.first().cloned())
        .or_else(|| args.file_substrings.first().cloned());

    if let (Some(text), Some(path)) = (
        summary.as_ref(),
        args.path_globs
            .first()
            .or_else(|| args.file_substrings.first()),
    ) && !text.contains(path)
    {
        summary = Some(format!("{text} in {path}"));
    }

    if let (Some(text), Some(lang)) = (summary.as_ref(), args.languages.first()) {
        summary = Some(format!("{text} ({lang})"));
    }

    summary
}

pub fn collect_flags(args: &NavigatorSearchArgs) -> Vec<String> {
    let mut flags = Vec::new();
    if args.recent_only.unwrap_or(false) {
        flags.push("recent".to_string());
    }
    if args.only_tests.unwrap_or(false) {
        flags.push("tests".to_string());
    }
    if args.only_docs.unwrap_or(false) {
        flags.push("docs".to_string());
    }
    if args.only_deps.unwrap_or(false) {
        flags.push("deps".to_string());
    }
    if args.with_refs.unwrap_or(false) {
        flags.push("with_refs".to_string());
    }
    if args.help_symbol.is_some() {
        flags.push("help".to_string());
    }
    if args
        .profiles
        .iter()
        .any(|profile| matches!(profile, SearchProfile::Text))
    {
        flags.push("text".to_string());
    }
    flags
}

pub fn profile_badges(profiles: &[SearchProfile]) -> Vec<String> {
    profiles.iter().map(|p| p.badge().to_string()).collect()
}
