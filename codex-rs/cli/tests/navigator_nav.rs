use std::path::Path;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use codex_navigator::client::ClientOptions;
use codex_navigator::client::NavigatorClient;
use codex_navigator::freeform::NavigatorPayload;
use codex_navigator::freeform::parse_payload as parse_navigator_payload;
use codex_navigator::plan_search_request;
use codex_navigator::proto::FileCategory;
use codex_navigator::proto::IndexState;
use codex_navigator::proto::SearchDiagnostics;
use codex_navigator::proto::SearchResponse;
use serde_json::Value;
use tempfile::TempDir;
use tokio::runtime::Runtime;

struct NavCommandOutput {
    response: SearchResponse,
    stderr: String,
}

fn codex_command(codex_home: &Path, project_root: &Path) -> Result<Command> {
    let mut cmd = Command::cargo_bin("codex")?;
    cmd.env("CODEX_HOME", codex_home);
    cmd.current_dir(project_root);
    Ok(cmd)
}

fn wait_for_metadata(path: &Path) -> Result<()> {
    let timeout = Duration::from_secs(10);
    let start = Instant::now();
    while !path.exists() {
        if start.elapsed() > timeout {
            anyhow::bail!("navigator metadata was not created in time");
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn daemon_metadata_path(codex_home: &Path, _project_root: &Path) -> Result<PathBuf> {
    let canonical_home = std::fs::canonicalize(codex_home)?;
    Ok(canonical_home.join("navigator").join("daemon.json"))
}

fn spawn_daemon_process(codex_home: &Path, project_root: &Path) -> Result<std::process::Child> {
    let binary = cargo_bin("codex");
    StdCommand::new(&binary)
        .env("CODEX_HOME", codex_home)
        .arg("navigator-daemon")
        .arg("--project-root")
        .arg(project_root)
        .arg("--codex-home")
        .arg(codex_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn navigator-daemon")
}

fn run_nav_command(
    codex_home: &Path,
    project_root: &Path,
    extra_args: &[&str],
) -> Result<NavCommandOutput> {
    let (stdout, stderr) = run_nav_raw(codex_home, project_root, extra_args)?;
    let response: SearchResponse = serde_json::from_str(stdout.trim())?;
    Ok(NavCommandOutput { response, stderr })
}

fn run_facet_command(
    codex_home: &Path,
    project_root: &Path,
    extra_args: &[&str],
) -> Result<SearchResponse> {
    let mut cmd = codex_command(codex_home, project_root)?;
    let mut args = vec![
        "navigator".to_string(),
        "facet".to_string(),
        "--project-root".to_string(),
        project_root
            .to_str()
            .ok_or_else(|| anyhow!("project_root must be valid UTF-8"))?
            .to_string(),
    ];
    for arg in extra_args {
        args.push(arg.to_string());
    }
    cmd.args(args);
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "navigator facet command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let response: SearchResponse = serde_json::from_str(stdout.trim())?;
    Ok(response)
}

fn run_nav_raw(
    codex_home: &Path,
    project_root: &Path,
    extra_args: &[&str],
) -> Result<(String, String)> {
    let mut cmd = codex_command(codex_home, project_root)?;
    let mut args = vec![
        "nav".to_string(),
        "--project-root".to_string(),
        project_root
            .to_str()
            .ok_or_else(|| anyhow!("project_root must be valid UTF-8"))?
            .to_string(),
        "--limit".to_string(),
        "8".to_string(),
    ];
    for arg in extra_args {
        args.push(arg.to_string());
    }
    cmd.args(args);
    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        anyhow::bail!("nav diagnostics-only failed: {stderr}");
    }
    Ok((stdout, stderr))
}

#[test]
fn navigator_nav_round_trip_via_daemon() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn navigator_history_lines_for_test() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;

    wait_for_metadata(&metadata_path)?;

    let output = run_nav_command(
        codex_home.path(),
        project_root,
        &["navigator_history_lines_for_test"],
    )?;
    let response = output.response;

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        response
            .hits
            .iter()
            .any(|hit| hit.path.ends_with("src/lib.rs"))
    );

    Ok(())
}

#[test]
fn navigator_accepts_json_payload_via_daemon_client() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/json_target.rs"),
        "pub fn json_symbol_for_test() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let request_json =
        r#"{"search": {"query": "json_symbol_for_test", "profiles": ["refernces"]}}"#;
    let payload = parse_navigator_payload(request_json)?;
    let NavigatorPayload::Search(args) = payload else {
        anyhow::bail!("expected search payload");
    };
    let request =
        plan_search_request(*args).map_err(|err| anyhow::anyhow!(err.message().to_string()))?;

    let rt = Runtime::new()?;
    let client = rt.block_on(NavigatorClient::new(ClientOptions {
        project_root: Some(project_root.to_path_buf()),
        codex_home: Some(codex_home.path().to_path_buf()),
        spawn: None,
    }))?;
    let response = rt.block_on(client.search(request))?;

    assert!(
        response
            .hits
            .iter()
            .any(|hit| hit.path.ends_with("src/json_target.rs"))
    );
    assert!(
        response
            .hints
            .iter()
            .any(|hint| hint.contains("auto-corrected"))
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}

#[test]
fn navigator_nav_supports_refine_flow() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/recent.rs"),
        "pub fn recent_symbol_for_test() {}",
    )?;
    std::fs::write(
        project_root.join("src/target.rs"),
        "pub fn index_coordinator_new() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    // initial search scoped to recent file only
    let initial = run_nav_command(
        codex_home.path(),
        project_root,
        &["--recent", "recent_symbol_for_test"],
    )?;
    let query_id = initial
        .response
        .query_id
        .context("initial nav missing query_id")?;

    // refine to target symbol; expect fallback and hints
    let refine_arg = query_id.to_string();
    let refined = run_nav_command(
        codex_home.path(),
        project_root,
        &["--from", &refine_arg, "index_coordinator_new"],
    )?;
    let response = refined.response;
    assert!(
        response
            .hits
            .iter()
            .any(|hit| hit.path.ends_with("src/target.rs"))
    );
    assert!(
        response
            .hints
            .iter()
            .any(|hint| hint.contains("refine returned no hits"))
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}

#[test]
fn navigator_nav_history_stack_toggles_filters() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn history_stack_symbol() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let initial = run_nav_command(codex_home.path(), project_root, &["history_stack_symbol"])?;
    let query_id = initial
        .response
        .query_id
        .context("initial navigator response missing query_id")?;

    let query_arg = query_id.to_string();
    let seeded = run_facet_command(
        codex_home.path(),
        project_root,
        &["--from", &query_arg, "--tests"],
    )?;
    let seeded_filters = seeded
        .active_filters
        .as_ref()
        .expect("seeding facet should record filters");
    assert!(seeded_filters.categories.contains(&FileCategory::Tests));

    let applied = run_facet_command(codex_home.path(), project_root, &["--history-stack", "0"])?;
    let applied_filters = applied
        .active_filters
        .as_ref()
        .expect("history stack should install filters");
    assert!(applied_filters.categories.contains(&FileCategory::Tests));
    assert!(applied.hints.iter().any(|hint| hint.contains("history[0]")));

    let cleared = run_facet_command(
        codex_home.path(),
        project_root,
        &["--remove-history-stack", "0"],
    )?;
    assert!(
        cleared
            .active_filters
            .as_ref()
            .map(|filters| filters.categories.is_empty())
            .unwrap_or(true)
    );
    assert!(
        cleared
            .hints
            .iter()
            .any(|hint| hint.contains("removed history[0] filters"))
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}

#[test]
fn navigator_nav_text_format_outputs_summary() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/text_format.rs"),
        "pub fn text_format_symbol() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let (stdout, _stderr) = run_nav_raw(
        codex_home.path(),
        project_root,
        &["--format", "text", "text_format_symbol"],
    )?;

    let trimmed = stdout.trim();
    assert!(trimmed.contains("query_id:"));
    assert!(trimmed.contains("hits (showing"));
    assert!(trimmed.contains("text_format.rs"));
    assert!(trimmed.starts_with("diagnostics:"));
    assert!(!trimmed.starts_with("{"), "text mode should not emit JSON");

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}

#[test]
fn navigator_nav_streams_diagnostics_and_hits() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn streaming_probe() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let output = run_nav_command(codex_home.path(), project_root, &["streaming_probe"])?;

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        output.stderr.contains("[navigator] diagnostics"),
        "stderr missing diagnostics: {}",
        output.stderr
    );
    assert!(
        output.stderr.contains("[navigator] top hits"),
        "stderr missing top hits: {}",
        output.stderr
    );

    Ok(())
}

#[test]
fn navigator_nav_literal_fallback_hits() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("config"))?;
    let padding = "A".repeat(400);
    std::fs::write(
        project_root.join("config/env.md"),
        format!("{padding}\nCODEX_SANDBOX=1"),
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let output = run_nav_command(codex_home.path(), project_root, &["CODEX_SANDBOX"])?;
    let response = output.response;

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        !response.hits.is_empty(),
        "expected literal fallback hit: {response:#?}"
    );
    assert!(
        response
            .hits
            .iter()
            .any(|hit| hit.id.starts_with("literal::"))
    );

    Ok(())
}

#[test]
fn navigator_nav_diagnostics_only_suppresses_hits() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(project_root.join("src/lib.rs"), "pub fn health_check() {}")?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let (stdout, stderr) = run_nav_raw(
        codex_home.path(),
        project_root,
        &["--diagnostics-only", "health_check"],
    )?;

    let _ = daemon.kill();
    let _ = daemon.wait();

    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "diagnostics-only should emit a JSON payload on stdout"
    );
    let diag: SearchDiagnostics = serde_json::from_str(trimmed)?;
    assert_eq!(diag.index_state, IndexState::Ready);
    assert!(diag.coverage.pending.is_empty());
    assert!(stderr.contains("[navigator] diagnostics"));
    assert!(!stderr.contains("[navigator] top hits"));

    Ok(())
}

#[test]
fn navigator_nav_streams_ndjson_events() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(project_root.join("src/lib.rs"), "pub fn ndjson_probe() {}")?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let (stdout, _) = run_nav_raw(
        codex_home.path(),
        project_root,
        &["--format", "ndjson", "ndjson_probe"],
    )?;

    let _ = daemon.kill();
    let _ = daemon.wait();

    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(lines.len() >= 2, "expected diagnostics + final events");
    let diag: Value = serde_json::from_str(lines[0])?;
    assert_eq!(diag["event"], "diagnostics");
    let final_event: Value = serde_json::from_str(lines.last().unwrap())?;
    assert_eq!(final_event["event"], "final");

    Ok(())
}

#[test]
fn navigator_cli_atlas_prints_workspace_tree() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    let cargo_manifest = "[workspace]\nmembers = [\"core\"]\n";
    std::fs::write(project_root.join("Cargo.toml"), cargo_manifest)?;
    std::fs::create_dir_all(project_root.join("core/src"))?;
    std::fs::write(
        project_root.join("core/src/lib.rs"),
        "pub fn atlas_sample() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;
    wait_for_metadata(&metadata_path)?;

    let mut cmd = codex_command(codex_home.path(), project_root)?;
    cmd.arg("navigator")
        .arg("atlas")
        .arg("--project-root")
        .arg(project_root);
    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("core"),
        "atlas output missing crate: {stdout}"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}
