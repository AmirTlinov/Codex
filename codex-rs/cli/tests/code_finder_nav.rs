use std::fmt::Write as _;
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
use blake3::Hasher;
use codex_code_finder::client::ClientOptions;
use codex_code_finder::client::CodeFinderClient;
use codex_code_finder::freeform::CodeFinderPayload;
use codex_code_finder::freeform::parse_payload as parse_code_finder_payload;
use codex_code_finder::plan_search_request;
use codex_code_finder::proto::SearchResponse;
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn hash_project_root(root: &Path) -> Result<String> {
    let canonical = std::fs::canonicalize(root)?;
    let mut hasher = Hasher::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut short = String::with_capacity(16);
    for byte in digest.as_bytes().iter().take(8) {
        let _ = write!(&mut short, "{byte:02x}");
    }
    Ok(short)
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
            anyhow::bail!("code-finder metadata was not created in time");
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn daemon_metadata_path(codex_home: &Path, project_root: &Path) -> Result<PathBuf> {
    let canonical_home = std::fs::canonicalize(codex_home)?;
    let hash = hash_project_root(project_root)?;
    Ok(canonical_home
        .join("code-finder")
        .join(hash)
        .join("daemon.json"))
}

fn spawn_daemon_process(codex_home: &Path, project_root: &Path) -> Result<std::process::Child> {
    let binary = cargo_bin("codex");
    StdCommand::new(&binary)
        .env("CODEX_HOME", codex_home)
        .arg("code-finder-daemon")
        .arg("--project-root")
        .arg(project_root)
        .arg("--codex-home")
        .arg(codex_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn code-finder-daemon")
}

fn run_nav_command_json(
    codex_home: &Path,
    project_root: &Path,
    extra_args: &[&str],
) -> Result<SearchResponse> {
    let mut cmd = codex_command(codex_home, project_root)?;
    let mut args = vec![
        "code-finder".to_string(),
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
    if !output.status.success() {
        anyhow::bail!(
            "nav command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let response: SearchResponse = serde_json::from_slice(&output.stdout)?;
    Ok(response)
}

#[test]
fn code_finder_nav_round_trip_via_daemon() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project_dir = TempDir::new()?;
    let project_root = project_dir.path();
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn code_finder_history_lines_for_test() {}",
    )?;

    let metadata_path = daemon_metadata_path(codex_home.path(), project_root)?;
    let mut daemon = spawn_daemon_process(codex_home.path(), project_root)?;

    wait_for_metadata(&metadata_path)?;

    let response = run_nav_command_json(
        codex_home.path(),
        project_root,
        &["code_finder_history_lines_for_test"],
    )?;

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
fn code_finder_accepts_json_payload_via_daemon_client() -> Result<()> {
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
    let payload = parse_code_finder_payload(request_json)?;
    let CodeFinderPayload::Search(args) = payload else {
        anyhow::bail!("expected search payload");
    };
    let request =
        plan_search_request(*args).map_err(|err| anyhow::anyhow!(err.message().to_string()))?;

    let rt = Runtime::new()?;
    let client = rt.block_on(CodeFinderClient::new(ClientOptions {
        project_root: Some(project_root.to_path_buf()),
        codex_home: Some(codex_home.path().to_path_buf()),
        spawn: None,
    }))?;
    let response = rt.block_on(client.search(&request))?;

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
fn code_finder_nav_supports_refine_flow() -> Result<()> {
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
    let initial = run_nav_command_json(
        codex_home.path(),
        project_root,
        &["--recent", "recent_symbol_for_test"],
    )?;
    let query_id = initial.query_id.context("initial nav missing query_id")?;

    // refine to target symbol; expect fallback and hints
    let refine_arg = query_id.to_string();
    let refined = run_nav_command_json(
        codex_home.path(),
        project_root,
        &["--from", &refine_arg, "index_coordinator_new"],
    )?;
    assert!(
        refined
            .hits
            .iter()
            .any(|hit| hit.path.ends_with("src/target.rs"))
    );
    assert!(
        refined
            .hints
            .iter()
            .any(|hint| hint.contains("refine returned no hits"))
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    Ok(())
}
