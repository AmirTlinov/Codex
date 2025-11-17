use assert_cmd::Command;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

pub(crate) struct ParsedOutput {
    lines: Vec<String>,
    json: Value,
}

impl ParsedOutput {
    fn report(&self) -> anyhow::Result<&serde_json::Map<String, Value>> {
        self.json
            .get("report")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("report field missing"))
    }
}

pub(crate) fn parse_stdout_bytes(bytes: &[u8]) -> anyhow::Result<ParsedOutput> {
    let stdout = String::from_utf8(bytes.to_vec())?;
    let mut lines: Vec<String> = stdout.lines().map(ToString::to_string).collect();
    let json_line = lines
        .pop()
        .ok_or_else(|| anyhow::anyhow!("apply_patch output missing JSON line"))?;
    let json: Value = serde_json::from_str(&json_line)?;
    anyhow::ensure!(
        json.get("schema").and_then(Value::as_str) == Some("apply_patch/v2"),
        "schema mismatch"
    );
    Ok(ParsedOutput { lines, json })
}

pub(crate) fn run_apply_patch_success(dir: &Path, patch: &str) -> anyhow::Result<ParsedOutput> {
    run_apply_patch_with_patch(dir, &["apply"], patch)
}

pub(crate) fn run_apply_patch_failure(
    dir: &Path,
    patch: &str,
) -> anyhow::Result<(ParsedOutput, String)> {
    let mut cmd = Command::cargo_bin("apply_patch")?;
    cmd.current_dir(dir).arg("apply");
    let assert = cmd.write_stdin(patch.to_string()).assert().failure();
    let stdout = assert.get_output().stdout.clone();
    let parsed = parse_stdout_bytes(&stdout)?;
    let stderr = String::from_utf8(assert.get_output().stderr.clone())?;
    Ok((parsed, stderr))
}

fn run_apply_patch_with_patch(
    dir: &Path,
    args: &[&str],
    patch: &str,
) -> anyhow::Result<ParsedOutput> {
    let mut cmd = Command::cargo_bin("apply_patch")?;
    cmd.current_dir(dir);
    for arg in args {
        cmd.arg(arg);
    }
    let assert = cmd.write_stdin(patch.to_string()).assert().success();
    let stdout = assert.get_output().stdout.clone();
    parse_stdout_bytes(&stdout)
}

fn run_apply_patch_raw(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = Command::cargo_bin("apply_patch")?;
    cmd.current_dir(dir);
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.assert().success().get_output().stdout.clone();
    Ok(String::from_utf8(output)?)
}

fn write_catalog_entries(root: &Path, entries: &[(&str, &str, &str)]) -> anyhow::Result<()> {
    let mut scripts = Vec::new();
    for (path, version, name) in entries {
        let hash = compute_script_hash(&root.join(path))?;
        scripts.push(json!({
            "path": path,
            "version": version,
            "name": name,
            "hash": hash,
        }));
    }
    let catalog = json!({ "version": 1, "scripts": scripts });
    let catalog_path = root.join("refactors/catalog.json");
    if let Some(parent) = catalog_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(catalog_path, serde_json::to_string_pretty(&catalog)?)?;
    Ok(())
}

fn compute_script_hash(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[test]
fn test_apply_patch_cli_add_and_update() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = "cli_test.txt";
    let absolute_path = tmp.path().join(file);

    let add_patch = format!("*** Begin Patch\n*** Add File: {file}\n+hello\n*** End Patch\n");
    let parsed = run_apply_patch_success(tmp.path(), &add_patch)?;
    assert_eq!(parsed.lines[0], "Applied operations:");
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line == &format!("- add: {file} (+1)"))
    );
    assert_eq!(
        parsed.lines.last(),
        Some(&"✔ Patch applied successfully.".to_string())
    );
    let report = parsed.report()?;
    assert_eq!(
        report.get("status").and_then(Value::as_str),
        Some("success")
    );
    let operations = report
        .get("operations")
        .and_then(Value::as_array)
        .expect("operations array present");
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0].get("action").and_then(Value::as_str),
        Some("add")
    );
    assert_eq!(fs::read_to_string(&absolute_path)?, "hello\n");

    let update_patch =
        format!("*** Begin Patch\n*** Update File: {file}\n@@\n-hello\n+world\n*** End Patch\n");
    let parsed = run_apply_patch_success(tmp.path(), &update_patch)?;
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line == &format!("- update: {file} (+1, -1)"))
    );
    assert_eq!(
        parsed.lines.last(),
        Some(&"✔ Patch applied successfully.".to_string())
    );
    let operations = parsed
        .report()?
        .get("operations")
        .and_then(Value::as_array)
        .expect("operations array present");
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0].get("action").and_then(Value::as_str),
        Some("update")
    );
    assert_eq!(fs::read_to_string(&absolute_path)?, "world\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_delete_file() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = "cli_delete.txt";
    let absolute_path = tmp.path().join(file);
    fs::write(&absolute_path, "obsolete\n")?;

    let delete_patch = format!("*** Begin Patch\n*** Delete File: {file}\n*** End Patch\n");
    let parsed = run_apply_patch_success(tmp.path(), &delete_patch)?;
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line == &format!("- delete: {file} (-1)"))
    );
    assert_eq!(
        parsed.lines.last(),
        Some(&"✔ Patch applied successfully.".to_string())
    );
    assert!(
        !absolute_path.exists(),
        "{file} should be removed after apply_patch"
    );

    Ok(())
}

#[test]
fn test_apply_patch_cli_move_file() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let src = "cli_move_src.txt";
    let dest = "cli_move_dest.txt";
    let src_path = tmp.path().join(src);
    fs::write(&src_path, "first line\n")?;

    let move_patch = format!(
        "*** Begin Patch\n*** Update File: {src}\n*** Move to: {dest}\n@@\n-first line\n+second line\n*** End Patch\n"
    );
    let parsed = run_apply_patch_success(tmp.path(), &move_patch)?;
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line == &format!("- move: {src} -> {dest} (+1, -1)"))
    );
    assert_eq!(
        parsed.lines.last(),
        Some(&"✔ Patch applied successfully.".to_string())
    );
    assert!(
        !src_path.exists(),
        "source file should be removed after move"
    );
    let dest_path = tmp.path().join(dest);
    assert_eq!(fs::read_to_string(&dest_path)?, "second line\n");

    Ok(())
}

#[test]
fn test_apply_patch_cli_emits_machine_json() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = "cli_machine.txt";
    let add_patch = format!("*** Begin Patch\n*** Add File: {file}\n+machine\n*** End Patch\n");

    let parsed = run_apply_patch_success(tmp.path(), &add_patch)?;
    assert_eq!(parsed.lines[0], "Applied operations:");
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line == &format!("- add: {file} (+1)"))
    );
    let report = parsed.report()?;
    assert_eq!(
        report.get("status").and_then(Value::as_str),
        Some("success")
    );
    assert_eq!(report.get("mode").and_then(Value::as_str), Some("apply"));

    Ok(())
}

#[test]
fn test_apply_patch_cli_scripts_list_shows_catalog() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let script_rel = "refactors/add_logging.toml";
    fs::create_dir_all(tmp.path().join("refactors"))?;
    fs::write(
        tmp.path().join(script_rel),
        r#"name = "AddLog"
version = "0.1.0"

[[steps]]
path = "src/lib.rs"
op = "template"
mode = "file-end"
payload = ["// log"]
"#,
    )?;
    write_catalog_entries(tmp.path(), &[(script_rel, "0.1.0", "AddLog")])?;

    let stdout = run_apply_patch_raw(tmp.path(), &["scripts", "list"])?;
    assert!(stdout.contains("refactors/add_logging.toml"));
    assert!(stdout.contains("AddLog"));
    Ok(())
}

#[test]
fn test_apply_patch_cli_scripts_list_json() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let script_rel = "refactors/json_list.toml";
    fs::create_dir_all(tmp.path().join("refactors"))?;
    fs::write(
        tmp.path().join(script_rel),
        r#"name = "JsonList"
version = "0.1.0"

[[steps]]
path = "src/lib.rs"
op = "rename"
symbol = "foo"
new_name = "bar"
"#,
    )?;
    write_catalog_entries(tmp.path(), &[(script_rel, "0.1.0", "JsonList")])?;

    let stdout = run_apply_patch_raw(tmp.path(), &["scripts", "list", "--json"])?;
    let parsed: Value = serde_json::from_str(&stdout)?;
    let arr = parsed.as_array().expect("json array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("path").and_then(Value::as_str), Some(script_rel));
    assert_eq!(arr[0].get("name").and_then(Value::as_str), Some("JsonList"));
    Ok(())
}

#[test]
fn test_apply_patch_cli_preview_outputs_diff() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = tmp.path().join("lib.rs");
    fs::write(&file, "fn greet() {}\n")?;
    let patch = "*** Begin Patch\n*** Ast Operation: lib.rs op=rename symbol=greet new_name=salute\n*** End Patch\n";
    let parsed = run_apply_patch_with_patch(tmp.path(), &["preview"], patch)?;
    let preview_line = parsed
        .lines
        .iter()
        .find(|line| line.starts_with("Preview"))
        .cloned()
        .unwrap_or_default();
    assert!(preview_line.contains("Preview"));
    assert!(parsed.lines.iter().any(|line| line.contains("-fn greet")));
    let report = parsed.report()?;
    assert_eq!(report.get("mode").and_then(Value::as_str), Some("dry-run"));
    let operations = report
        .get("operations")
        .and_then(Value::as_array)
        .expect("operations array");
    assert!(operations[0].get("preview").is_some());
    Ok(())
}

#[test]
fn test_apply_patch_cli_does_not_write_logs() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = "cli_log.txt";
    let patch = format!("*** Begin Patch\n*** Add File: {file}\n+log test\n*** End Patch\n");

    run_apply_patch_success(tmp.path(), &patch)?;

    let log_dir = tmp.path().join("reports/logs");
    assert!(
        !log_dir.exists(),
        "apply_patch should not write diagnostics under reports/logs"
    );

    let patch = format!("*** Begin Patch\n*** Add File: {file}\n+second run\n*** End Patch\n");

    run_apply_patch_success(tmp.path(), &patch)?;
    assert!(
        !log_dir.exists(),
        "logs directory should remain absent after repeated runs"
    );
    Ok(())
}

#[test]
fn test_apply_patch_default_is_dry_run() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = "cli_dry_run.txt";
    let absolute_path = tmp.path().join(file);
    fs::write(&absolute_path, "original\n")?;

    let patch = format!(
        "*** Begin Patch\n*** Update File: {file}\n@@\n-original\n+changed\n*** End Patch\n"
    );

    let mut cmd = Command::cargo_bin("apply_patch")?;
    cmd.current_dir(tmp.path());
    let assert = cmd.write_stdin(patch).assert().success();
    let parsed = parse_stdout_bytes(&assert.get_output().stdout.clone())?;
    assert_eq!(parsed.lines[0], "Planned operations:");
    let report = parsed.report()?;
    assert_eq!(report.get("mode").and_then(Value::as_str), Some("dry-run"));
    assert_eq!(fs::read_to_string(&absolute_path)?, "original\n");
    Ok(())
}

#[test]
fn test_apply_patch_cli_writes_conflict_hint_on_failure() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = tmp.path().join("conflict.txt");
    fs::write(&file, "current\n")?;

    let patch =
        "*** Begin Patch\n*** Update File: conflict.txt\n@@\n-original\n+updated\n*** End Patch\n";
    let (parsed, stderr) = run_apply_patch_failure(tmp.path(), patch)?;
    assert!(
        parsed
            .lines
            .iter()
            .any(|line| line.contains("Attempted operations")),
        "stdout should include attempt summary"
    );
    assert!(
        stderr.contains("Failed to find expected lines"),
        "stderr should include conflict summary"
    );
    let log_dir = tmp.path().join("reports/logs");
    assert!(
        !log_dir.exists(),
        "logs directory should not be created on failure"
    );
    Ok(())
}
