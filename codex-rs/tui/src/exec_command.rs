use std::path::Path;
use std::path::PathBuf;

use dirs::home_dir;
use shlex::try_join;

pub(crate) fn escape_command(command: &[String]) -> String {
    try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "))
}

pub(crate) fn strip_bash_lc_and_escape(command: &[String]) -> String {
    match command {
        [first, second, third] if is_shell_wrapper(first, second) => third.clone(),
        [first, second, third, fourth]
            if first == "/usr/bin/env" && is_shell_wrapper(second, third) =>
        {
            fourth.clone()
        }
        _ => escape_command(command),
    }
}

fn is_shell_wrapper(first: &str, second: &str) -> bool {
    matches!(
        (first, second),
        ("bash", "-lc") | ("/bin/bash", "-lc") | ("sh", "-c") | ("/bin/sh", "-c")
    )
}

/// If `path` is absolute and inside $HOME, return the part *after* the home
/// directory; otherwise, return the path as-is. Note if `path` is the homedir,
/// this will return and empty path.
pub(crate) fn relativize_to_home<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if !path.is_absolute() {
        // If the path is not absolute, we can’t do anything with it.
        return None;
    }

    let home_dir = home_dir()?;
    let rel = path.strip_prefix(&home_dir).ok()?;
    Some(rel.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_command() {
        let args = vec!["foo".into(), "bar baz".into(), "weird&stuff".into()];
        let cmdline = escape_command(&args);
        assert_eq!(cmdline, "foo 'bar baz' 'weird&stuff'");
    }

    #[test]
    fn test_strip_bash_lc_and_escape() {
        let args = vec!["bash".into(), "-lc".into(), "echo hello".into()];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "echo hello");
    }

    #[test]
    fn test_strip_various_shell_wrappers() {
        let cases = [
            vec!["/bin/bash".into(), "-lc".into(), "ls".into()],
            vec!["sh".into(), "-c".into(), "rg pattern".into()],
            vec!["/bin/sh".into(), "-c".into(), "printf hi".into()],
            vec![
                "/usr/bin/env".into(),
                "bash".into(),
                "-lc".into(),
                "cargo test".into(),
            ],
        ];

        let expected = ["ls", "rg pattern", "printf hi", "cargo test"];

        for (args, expect) in cases.into_iter().zip(expected) {
            assert_eq!(strip_bash_lc_and_escape(&args), expect);
        }
    }
}
