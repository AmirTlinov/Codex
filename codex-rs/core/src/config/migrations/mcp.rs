use std::fs;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use tempfile::NamedTempFile;
use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;
use toml_edit::Table as TomlTable;

use super::super::CONFIG_TOML_FILE;

/// Number of historical config backups retained during migration.
pub const BACKUP_RETENTION: usize = 3;

/// Options controlling MCP schema migration behaviour.
#[derive(Debug, Clone, Copy)]
pub struct MigrationOptions {
    pub dry_run: bool,
    pub force: bool,
}

fn normalize_timeout_fields(
    server_name: &str,
    table: &mut TomlTable,
) -> std::io::Result<Vec<String>> {
    let mut notes = Vec::new();

    if table.contains_key("startup_timeout_ms") {
        let item = table.remove("startup_timeout_ms").unwrap_or(TomlItem::None);
        if table.get("startup_timeout_sec").is_none() {
            let secs = convert_ms_item(&item, server_name, "startup_timeout_ms")?;
            table["startup_timeout_sec"] = toml_edit::value(secs);
        }
        notes.push(format!("normalized startup_timeout_ms for '{server_name}'"));
    }

    if table.contains_key("tool_timeout_ms") {
        let item = table.remove("tool_timeout_ms").unwrap_or(TomlItem::None);
        if table.get("tool_timeout_sec").is_none() {
            let secs = convert_ms_item(&item, server_name, "tool_timeout_ms")?;
            table["tool_timeout_sec"] = toml_edit::value(secs);
        }
        notes.push(format!("normalized tool_timeout_ms for '{server_name}'"));
    }

    Ok(notes)
}

fn convert_ms_item(item: &TomlItem, server_name: &str, field_name: &str) -> std::io::Result<f64> {
    let Some(value) = item.as_value() else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{field_name} for server '{server_name}' must be a numeric literal"),
        ));
    };

    if let Some(ms) = value.as_integer() {
        if ms < 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{field_name} for server '{server_name}' must be non-negative"),
            ));
        }
        return Ok((ms as f64) / 1000.0);
    }

    if let Some(ms) = value.as_float() {
        if ms < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{field_name} for server '{server_name}' must be non-negative"),
            ));
        }
        return Ok(ms / 1000.0);
    }

    if let Some(ms_str) = value.as_str() {
        let ms: f64 = ms_str.parse().map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!("{field_name} for server '{server_name}' must be numeric, got '{ms_str}'"),
            )
        })?;
        if ms < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{field_name} for server '{server_name}' must be non-negative"),
            ));
        }
        return Ok(ms / 1000.0);
    }

    Err(Error::new(
        ErrorKind::InvalidData,
        format!(
            "{field_name} for server '{server_name}' must be an integer, float, or numeric string"
        ),
    ))
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            dry_run: true,
            force: false,
        }
    }
}

/// Outcome of a migration attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationReport {
    pub backed_up: bool,
    pub changes_detected: bool,
    pub from_version: u32,
    pub to_version: u32,
    pub notes: Vec<String>,
}

impl MigrationReport {
    fn unchanged(version: u32, note: impl Into<String>) -> Self {
        Self {
            backed_up: false,
            changes_detected: false,
            from_version: version,
            to_version: version,
            notes: vec![note.into()],
        }
    }
}

/// Result of creating/rotating configuration backups.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BackupOutcome {
    pub created: bool,
    pub rotated: bool,
    pub backup_path: Option<PathBuf>,
}

/// Performs a best-effort migration of MCP-related configuration to schema version 2.
///
/// Behaviour:
/// * Inspects current `mcp_schema_version` (default = 1 when absent).
/// * If already at or above the target version and `force` is false, returns early.
/// * For dry-run, reports whether a change would occur without touching disk.
/// * For apply (`dry_run = false`), rotates backups and writes an updated config
///   with `mcp_schema_version = 2` (placeholder transformation for now).
pub fn migrate_to_v2(
    codex_home: &Path,
    options: &MigrationOptions,
) -> std::io::Result<MigrationReport> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    if !config_path.exists() {
        return Ok(MigrationReport::unchanged(
            1,
            "config.toml not found; nothing to migrate",
        ));
    }

    let mut doc = load_config_as_document(&config_path)?;
    let current_version = doc
        .get("mcp_schema_version")
        .and_then(|item| item.as_value())
        .and_then(toml_edit::Value::as_integer)
        .map(|v| v.max(0) as u32)
        .unwrap_or(1);

    let mut notes = Vec::new();
    let mut servers_changed = false;

    if let Some(servers_table) = doc
        .get_mut("mcp_servers")
        .and_then(|item| item.as_table_mut())
    {
        let server_keys: Vec<String> = servers_table.iter().map(|(k, _)| k.to_string()).collect();
        for server_name in server_keys {
            let Some(item) = servers_table.get_mut(&server_name) else {
                continue;
            };
            let Some(table) = item.as_table_mut() else {
                continue;
            };
            let mut server_notes = normalize_timeout_fields(&server_name, table)?;
            if !server_notes.is_empty() {
                servers_changed = true;
                if options.dry_run {
                    for note in &mut server_notes {
                        *note = format!("would {note}");
                    }
                }
                notes.extend(server_notes);
            }
        }
    }

    let needs_version_bump = current_version < 2 || options.force;
    if needs_version_bump {
        let msg = if options.dry_run {
            if current_version < 2 {
                format!("would set mcp_schema_version from {current_version} to 2")
            } else {
                "would reassert mcp_schema_version = 2".to_string()
            }
        } else if current_version < 2 {
            format!("mcp_schema_version updated from {current_version} to 2")
        } else {
            "mcp_schema_version reasserted to 2".to_string()
        };
        notes.push(msg);
    }

    let changes_detected = servers_changed || needs_version_bump;

    if !changes_detected {
        return Ok(MigrationReport::unchanged(
            current_version,
            "schema already at v2 and no timeout normalization required",
        ));
    }

    if options.dry_run {
        return Ok(MigrationReport {
            backed_up: false,
            changes_detected,
            from_version: current_version,
            to_version: 2,
            notes,
        });
    }

    let backup = create_backup_with_rotation(codex_home)?;

    if needs_version_bump {
        doc["mcp_schema_version"] = toml_edit::value(2);
    }

    write_document_atomic(codex_home, &config_path, doc)?;

    Ok(MigrationReport {
        backed_up: backup.created,
        changes_detected,
        from_version: current_version,
        to_version: 2,
        notes,
    })
}

/// Creates `config.toml.bak{N}` backups, rotating existing snapshots up to [`BACKUP_RETENTION`].
pub fn create_backup_with_rotation(codex_home: &Path) -> std::io::Result<BackupOutcome> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    if !config_path.exists() {
        return Ok(BackupOutcome::default());
    }

    fs::create_dir_all(codex_home)?;

    let mut rotated = false;
    for idx in (1..=BACKUP_RETENTION).rev() {
        let src = backup_path(codex_home, idx);
        if !src.exists() {
            continue;
        }
        if idx == BACKUP_RETENTION {
            fs::remove_file(&src)?;
        } else {
            let dst = backup_path(codex_home, idx + 1);
            if dst.exists() {
                fs::remove_file(&dst)?;
            }
            fs::rename(&src, &dst)?;
        }
        rotated = true;
    }

    let bak1 = backup_path(codex_home, 1);
    fs::copy(&config_path, &bak1)?;

    Ok(BackupOutcome {
        created: true,
        rotated,
        backup_path: Some(bak1),
    })
}

fn backup_path(codex_home: &Path, index: usize) -> PathBuf {
    codex_home.join(format!("config.toml.bak{index}"))
}

fn load_config_as_document(path: &Path) -> std::io::Result<DocumentMut> {
    let contents = fs::read_to_string(path)?;
    contents
        .parse::<DocumentMut>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn write_document_atomic(
    codex_home: &Path,
    config_path: &Path,
    doc: DocumentMut,
) -> std::io::Result<()> {
    fs::create_dir_all(codex_home)?;
    let tmp = NamedTempFile::new_in(codex_home)?;
    tmp.as_file().write_all(doc.to_string().as_bytes())?;
    tmp.persist(config_path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn backup_rotation_creates_and_rotates() -> std::io::Result<()> {
        let tmp = TempDir::new()?;
        let codex_home = tmp.path();
        let config_path = codex_home.join(CONFIG_TOML_FILE);
        fs::create_dir_all(codex_home)?;
        fs::write(&config_path, "model = \"gpt-5\"\n")?;

        // First backup
        let outcome1 = create_backup_with_rotation(codex_home)?;
        assert!(outcome1.created);
        assert!(outcome1.backup_path.unwrap().exists());

        // Modify config and create additional backups to trigger rotation.
        fs::write(&config_path, "model = \"o3\"\n")?;
        let _ = create_backup_with_rotation(codex_home)?;
        fs::write(&config_path, "model = \"gpt-4\"\n")?;
        let _ = create_backup_with_rotation(codex_home)?;
        fs::write(&config_path, "model = \"gpt-4.1\"\n")?;
        let outcome4 = create_backup_with_rotation(codex_home)?;
        assert!(outcome4.created);

        let bak1 = backup_path(codex_home, 1);
        let bak2 = backup_path(codex_home, 2);
        let bak3 = backup_path(codex_home, 3);
        assert!(bak1.exists());
        assert!(bak2.exists());
        assert!(bak3.exists());

        Ok(())
    }

    #[test]
    fn dry_run_reports_without_changes() -> std::io::Result<()> {
        let tmp = TempDir::new()?;
        let codex_home = tmp.path();
        let config_path = codex_home.join(CONFIG_TOML_FILE);
        fs::create_dir_all(codex_home)?;
        fs::write(&config_path, "model = \"gpt-5\"\n")?;

        let report = migrate_to_v2(codex_home, &MigrationOptions::default())?;
        assert!(report.changes_detected);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("would set mcp_schema_version"))
        );
        assert_eq!(report.from_version, 1);
        assert_eq!(report.to_version, 2);

        // Dry run should not create backup or modify file.
        assert!(!backup_path(codex_home, 1).exists());
        let contents = fs::read_to_string(&config_path)?;
        assert!(!contents.contains("mcp_schema_version"));

        Ok(())
    }

    #[test]
    fn migrate_applies_version_and_creates_backup() -> std::io::Result<()> {
        let tmp = TempDir::new()?;
        let codex_home = tmp.path();
        let config_path = codex_home.join(CONFIG_TOML_FILE);
        fs::create_dir_all(codex_home)?;
        fs::write(&config_path, "model = \"gpt-5\"\n")?;

        let options = MigrationOptions {
            dry_run: false,
            force: false,
        };
        let report = migrate_to_v2(codex_home, &options)?;
        assert!(report.changes_detected);
        assert!(report.backed_up);
        assert_eq!(report.to_version, 2);
        assert!(backup_path(codex_home, 1).exists());

        let contents = fs::read_to_string(&config_path)?;
        assert!(contents.contains("mcp_schema_version = 2"));

        Ok(())
    }

    #[test]
    fn migrate_normalizes_timeout_ms_and_is_idempotent() -> std::io::Result<()> {
        let tmp = TempDir::new()?;
        let codex_home = tmp.path();
        let config_path = codex_home.join(CONFIG_TOML_FILE);
        fs::create_dir_all(codex_home)?;
        fs::write(
            &config_path,
            r#"
[mcp_servers.docs]
command = "echo"
startup_timeout_ms = 2500
tool_timeout_ms = 6000
"#,
        )?;

        let options = MigrationOptions {
            dry_run: false,
            force: false,
        };
        let report = migrate_to_v2(codex_home, &options)?;
        assert!(report.changes_detected);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("startup_timeout_ms"))
        );
        assert!(report.notes.iter().any(|n| n.contains("tool_timeout_ms")));

        let contents = fs::read_to_string(&config_path)?;
        assert!(contents.contains("startup_timeout_sec = 2.5"));
        assert!(contents.contains("tool_timeout_sec = 6"));
        assert!(!contents.contains("startup_timeout_ms"));
        assert!(!contents.contains("tool_timeout_ms"));
        assert!(contents.contains("mcp_schema_version = 2"));

        let second = migrate_to_v2(codex_home, &options)?;
        assert!(!second.changes_detected);
        assert_eq!(
            second.notes,
            vec!["schema already at v2 and no timeout normalization required".to_string()]
        );

        Ok(())
    }
}
