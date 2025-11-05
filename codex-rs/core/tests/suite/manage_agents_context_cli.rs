use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Ensure `codex --manage-agents-context` prints the summary with token usage
/// and compact include/exclude listings when run non-interactively.
#[test]
fn manage_agents_context_summary_mentions_tokens() {
    let home = TempDir::new().expect("tempdir");
    let context_dir = home.path().join(".agents").join("context");
    fs::create_dir_all(&context_dir).expect("create context dir");
    fs::write(
        context_dir.join("guide.md"),
        "Remember to run the full test suite before committing.",
    )
    .expect("write guide");

    let mut cmd = Command::cargo_bin("codex").expect("codex binary");
    cmd.arg("--manage-agents-context")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Loaded 1 agents context file. (~"))
        .stdout(predicate::str::contains("include (0)"))
        .stdout(predicate::str::contains("exclude (0)"));

    // Clean up to avoid dangling directories on Windows.
    fs::remove_dir_all(home.path().join(".agents").join("context")).ok();
}

#[test]
fn manage_agents_context_reports_configuration_failure() {
    let home = TempDir::new().expect("tempdir");
    fs::write(home.path().join(".agents"), b"not a directory").expect("write placeholder");

    let mut cmd = Command::cargo_bin("codex").expect("codex binary");
    cmd.arg("--manage-agents-context")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Error loading configuration"));

    // Clean up the placeholder file.
    fs::remove_file(home.path().join(".agents")).ok();
}

#[test]
fn manage_agents_context_cli_clears_filters() {
    let home = TempDir::new().expect("tempdir");
    let agents_home = home.path().join(".agents");
    let notes_dir = agents_home.join("context").join("notes");
    fs::create_dir_all(&notes_dir).expect("create context dir");
    fs::write(
        notes_dir.join("guide.md"),
        "Always run tests before committing.",
    )
    .expect("write guide");

    let mut seed_cmd = Command::cargo_bin("codex").expect("codex binary");
    seed_cmd
        .arg("--manage-agents-context")
        .arg("--agents-context-include")
        .arg("notes")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");
    seed_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("include (1): notes"));

    let mut clear_cmd = Command::cargo_bin("codex").expect("codex binary");
    clear_cmd
        .arg("--manage-agents-context")
        .arg("--clear-agents-context-include")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");
    clear_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("include (0)"))
        .stdout(predicate::str::contains("exclude (0)"));
}

#[test]
fn manage_agents_context_cli_normalizes_include_filters() {
    let home = TempDir::new().expect("tempdir");
    let notes_dir = home.path().join(".agents").join("context").join("notes");
    fs::create_dir_all(&notes_dir).expect("create notes dir");
    fs::write(notes_dir.join("guide.md"), "Guide").expect("write guide");
    fs::write(notes_dir.join("tips.md"), "Tips").expect("write tips");

    let mut cmd = Command::cargo_bin("codex").expect("codex binary");
    cmd.arg("--manage-agents-context")
        .arg("--agents-context-include")
        .arg("notes/")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Loaded 2 agents context files"))
        .stdout(predicate::str::contains("include (1): notes"))
        .stdout(predicate::str::contains("exclude (0)"));
}

#[test]
fn manage_agents_context_cli_clear_and_replace() {
    let home = TempDir::new().expect("tempdir");
    let context_dir = home.path().join(".agents").join("context");
    let runbooks_dir = context_dir.join("runbooks");
    fs::create_dir_all(runbooks_dir).expect("create runbooks dir");
    fs::write(
        context_dir.join("notes.md"),
        "Always run tests before committing.",
    )
    .expect("write guide");
    fs::write(
        context_dir.join("runbooks").join("deploy.md"),
        "Deployment checklist",
    )
    .expect("write runbook");

    let mut cmd = Command::cargo_bin("codex").expect("codex binary");
    cmd.arg("--manage-agents-context")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("include (0)"));

    let mut seed_cmd = Command::cargo_bin("codex").expect("codex binary");
    seed_cmd
        .arg("--manage-agents-context")
        .arg("--agents-context-include")
        .arg("notes.md")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");
    seed_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("include (1): notes.md"));

    let mut replace_cmd = Command::cargo_bin("codex").expect("codex binary");
    replace_cmd
        .arg("--manage-agents-context")
        .arg("--agents-context-include")
        .arg("runbooks")
        .env("CODEX_HOME", home.path())
        .env("OPENAI_API_KEY", "dummy-key")
        .env("TERM", "xterm-256color");
    replace_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("include (1): runbooks"))
        .stdout(predicate::str::contains("exclude (0)"));
}
