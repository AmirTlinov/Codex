use std::path::Path;

use anyhow::Result;
use codex_core::config::load_global_mcp_servers;
use codex_core::config::write_global_mcp_servers;
use codex_core::config_types::McpServerTransportConfig;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::cargo_bin("codex")?;
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

#[test]
fn list_shows_empty_state() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd.args(["mcp", "list"]).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("No MCP servers configured yet."));

    Ok(())
}

#[tokio::test]
async fn list_and_get_render_expected_output() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut add = codex_command(codex_home.path())?;
    add.args([
        "mcp",
        "add",
        "docs",
        "--env",
        "TOKEN=secret",
        "--",
        "docs-server",
        "--port",
        "4000",
    ])
    .assert()
    .success();

    let mut servers = load_global_mcp_servers(codex_home.path()).await?;
    let docs_entry = servers
        .get_mut("docs")
        .expect("docs server should exist after add");
    match &mut docs_entry.transport {
        McpServerTransportConfig::Stdio { env_vars, .. } => {
            *env_vars = vec!["APP_TOKEN".to_string(), "WORKSPACE_ID".to_string()];
        }
        other => panic!("unexpected transport: {other:?}"),
    }
    write_global_mcp_servers(codex_home.path(), &servers)?;

    let mut list_cmd = codex_command(codex_home.path())?;
    let list_output = list_cmd.args(["mcp", "list"]).output()?;
    assert!(list_output.status.success());
    let stdout = String::from_utf8(list_output.stdout)?;
    assert!(stdout.contains("Name"));
    assert!(stdout.contains("Template"));
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("docs-server"));
    assert!(stdout.contains("TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Auth"));
    assert!(stdout.contains("enabled"));
    assert!(stdout.contains("Unsupported"));

    let mut list_json_cmd = codex_command(codex_home.path())?;
    let json_output = list_json_cmd.args(["mcp", "list", "--json"]).output()?;
    assert!(json_output.status.success());
    let stdout = String::from_utf8(json_output.stdout)?;
    let parsed: JsonValue = serde_json::from_str(&stdout)?;
    assert_eq!(
        parsed,
        json!([
            {
                "name": "docs",
                "display_name": null,
                "category": null,
                "template_id": null,
                "template": null,
                "description": null,
                "tags": [],
                "metadata": null,
                "created_at": null,
                "last_verified_at": null,
                "auth": null,
                "healthcheck": null,
                "enabled": true,
                "transport": {
                    "type": "stdio",
                    "command": "docs-server",
                    "args": [
                        "--port",
                        "4000"
                    ],
                    "env": {
                        "TOKEN": "secret"
                    },
                    "env_vars": [
                        "APP_TOKEN",
                        "WORKSPACE_ID"
                    ],
                    "cwd": null
                },
                "startup_timeout_sec": null,
                "tool_timeout_sec": null,
                "enabled_tools": null,
                "disabled_tools": null,
                "auth_status": "unsupported"
            }
        ])
    );

    let mut get_cmd = codex_command(codex_home.path())?;
    let get_output = get_cmd.args(["mcp", "get", "docs"]).output()?;
    assert!(get_output.status.success());
    let stdout = String::from_utf8(get_output.stdout)?;
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("transport: stdio"));
    assert!(stdout.contains("command: docs-server"));
    assert!(stdout.contains("args: --port 4000"));
    assert!(stdout.contains("env: TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("enabled: true"));
    assert!(stdout.contains("remove: codex mcp remove docs"));

    let mut get_json_cmd = codex_command(codex_home.path())?;
    get_json_cmd
        .args(["mcp", "get", "docs", "--json"])
        .assert()
        .success()
        .stdout(contains("\"name\": \"docs\"").and(contains("\"enabled\": true")));

    Ok(())
}

#[tokio::test]
async fn get_disabled_server_shows_single_line() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut add = codex_command(codex_home.path())?;
    add.args(["mcp", "add", "docs", "--", "docs-server"])
        .assert()
        .success();

    let mut servers = load_global_mcp_servers(codex_home.path()).await?;
    let docs = servers
        .get_mut("docs")
        .expect("docs server should exist after add");
    docs.enabled = false;
    write_global_mcp_servers(codex_home.path(), &servers)?;

    let mut get_cmd = codex_command(codex_home.path())?;
    let get_output = get_cmd.args(["mcp", "get", "docs"]).output()?;
    assert!(get_output.status.success());
    let stdout = String::from_utf8(get_output.stdout)?;
    assert_eq!(stdout.trim_end(), "docs (disabled)");

    Ok(())
}

#[test]
fn list_includes_agents_home_servers() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    let agents_mcp_dir = project.path().join(".agents").join("mcp");
    std::fs::create_dir_all(&agents_mcp_dir)?;
    std::fs::write(
        agents_mcp_dir.join("mcp.json"),
        r#"
{
  "docs": {
    "command": "docs-server",
    "args": ["--port", "8080"],
    "env": {"TOKEN": "secret"}
  }
}
"#,
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .current_dir(project.path())
        .args(["mcp", "list", "--json"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let parsed: JsonValue = serde_json::from_str(&stdout)?;
    let servers = parsed.as_array().expect("list response must be an array");
    assert_eq!(servers.len(), 1);
    let server = servers[0]
        .as_object()
        .expect("server entry should be an object");

    assert_eq!(server.get("name"), Some(&json!("docs")));
    assert_eq!(server.get("display_name"), Some(&JsonValue::Null));
    assert_eq!(server.get("category"), Some(&JsonValue::Null));
    assert_eq!(server.get("template_id"), Some(&JsonValue::Null));
    assert_eq!(server.get("description"), Some(&JsonValue::Null));
    assert_eq!(server.get("tags"), Some(&json!([])));
    assert_eq!(server.get("metadata"), Some(&JsonValue::Null));
    assert_eq!(server.get("created_at"), Some(&JsonValue::Null));
    assert_eq!(server.get("last_verified_at"), Some(&JsonValue::Null));
    assert_eq!(server.get("auth"), Some(&JsonValue::Null));
    assert_eq!(server.get("healthcheck"), Some(&JsonValue::Null));
    assert_eq!(server.get("enabled"), Some(&json!(true)));
    assert_eq!(server.get("startup_timeout_sec"), Some(&JsonValue::Null));
    assert_eq!(server.get("tool_timeout_sec"), Some(&JsonValue::Null));
    assert_eq!(server.get("enabled_tools"), Some(&JsonValue::Null));
    assert_eq!(server.get("disabled_tools"), Some(&JsonValue::Null));
    assert_eq!(server.get("auth_status"), Some(&json!("unsupported")));

    let transport = server
        .get("transport")
        .and_then(JsonValue::as_object)
        .expect("transport must be an object");
    assert_eq!(transport.get("type"), Some(&json!("stdio")));
    assert_eq!(transport.get("command"), Some(&json!("docs-server")));
    assert_eq!(transport.get("args"), Some(&json!(["--port", "8080"])));
    assert_eq!(transport.get("env"), Some(&json!({ "TOKEN": "secret" })));
    assert_eq!(transport.get("env_vars"), Some(&json!([])));
    assert_eq!(transport.get("cwd"), Some(&JsonValue::Null));

    Ok(())
}

#[tokio::test]
async fn list_and_get_show_template_metadata() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
[mcp_servers.docs]
command = "docs-server"
args = ["--port", "4000"]
template_id = "docs/local@1"
display_name = "Docs"

[mcp_templates."docs/local@1"]
summary = "Docs Template"
version = "1.0"
metadata = { owner = "docs-team" }
"#,
    )?;

    let mut list_cmd = codex_command(codex_home.path())?;
    let list_output = list_cmd.args(["mcp", "list"]).output()?;
    assert!(list_output.status.success());
    let stdout = String::from_utf8(list_output.stdout)?;
    assert!(stdout.contains("docs/local@1 (Docs Template)"));

    let mut json_get_cmd = codex_command(codex_home.path())?;
    let json_output = json_get_cmd
        .args(["mcp", "get", "docs", "--json"])
        .output()?;
    assert!(json_output.status.success());
    let json_stdout = String::from_utf8(json_output.stdout)?;
    let parsed: JsonValue = serde_json::from_str(&json_stdout)?;
    assert_eq!(
        parsed["template"],
        json!({
            "id": "docs/local@1",
            "version": "1.0",
            "summary": "Docs Template",
            "category": JsonValue::Null,
            "metadata": {
                "owner": "docs-team"
            }
        })
    );

    let mut get_cmd = codex_command(codex_home.path())?;
    let get_output = get_cmd.args(["mcp", "get", "docs"]).output()?;
    assert!(get_output.status.success());
    let get_stdout = String::from_utf8(get_output.stdout)?;
    assert!(get_stdout.contains("template.summary: Docs Template"));

    Ok(())
}
