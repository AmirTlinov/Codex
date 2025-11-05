use codex_core::config::AgentsContextImport;
use codex_core::config::AgentsContextImportResult;
use codex_core::config::AgentsSource;
use codex_core::config::import_agents_context;
use pretty_assertions::assert_eq;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_IMPORT_FILES: usize = 512;

fn write_file(path: &PathBuf, contents: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        panic!("failed to create {parent:?}: {err}");
    }
    let mut file = fs::File::create(path).unwrap_or_else(|err| {
        panic!("failed to create {}: {err}", path.display());
    });
    if let Err(err) = file.write_all(contents.as_bytes()) {
        panic!("failed to write {}: {err}", path.display());
    }
}

#[test]
fn import_file_into_global_context() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");

    let source_path = workspace.path().join("docs").join("guide.md");
    write_file(&source_path, "Run the full test suite before committing.");

    let import = AgentsContextImport {
        source: PathBuf::from("docs/guide.md"),
        target: AgentsSource::Global,
        destination_dir: None,
    };

    let AgentsContextImportResult { added_entries } =
        import_agents_context(&global_agents_home, None, cwd, import).expect("import file");

    assert_eq!(added_entries.len(), 1);
    let entry = &added_entries[0];
    assert_eq!(entry.source, AgentsSource::Global);
    assert_eq!(entry.relative_path, "guide.md");
    assert!(entry.absolute_path.exists());
    assert_eq!(entry.content, "Run the full test suite before committing.");

    let copied = global_agents_home.join("context").join("guide.md");
    assert!(copied.exists(), "file copied into context");
}

#[test]
fn import_directory_under_destination_dir() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");

    let source_dir = workspace.path().join("playbooks");
    fs::create_dir_all(&source_dir).expect("create source dir");
    write_file(&source_dir.join("deploy.md"), "Deployment checklist");
    fs::create_dir_all(source_dir.join("nested")).expect("create nested dir");
    write_file(&source_dir.join("nested").join("ops.md"), "Ops runbook");

    let import = AgentsContextImport {
        source: pathbuf("playbooks"),
        target: AgentsSource::Global,
        destination_dir: Some("runbooks".to_string()),
    };

    let AgentsContextImportResult { mut added_entries } =
        import_agents_context(&global_agents_home, None, cwd, import).expect("import directory");

    added_entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    let relative_paths: Vec<_> = added_entries
        .iter()
        .map(|entry| entry.relative_path.as_str())
        .collect();

    assert_eq!(
        relative_paths,
        vec![
            "runbooks/playbooks/deploy.md",
            "runbooks/playbooks/nested/ops.md",
        ]
    );

    for entry in &added_entries {
        assert_eq!(entry.source, AgentsSource::Global);
        assert!(entry.absolute_path.exists());
        assert!(entry.content.contains("runbook") || entry.content.contains("Deployment"));
    }
}

#[test]
fn importing_into_missing_project_context_errors() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");
    let source_path = workspace.path().join("guide.md");
    write_file(&source_path, "Remember to add tests.");

    let import = AgentsContextImport {
        source: PathBuf::from("guide.md"),
        target: AgentsSource::Project,
        destination_dir: None,
    };

    let err = import_agents_context(&global_agents_home, None, cwd, import)
        .expect_err("project context missing");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn import_conflicting_file_is_rejected() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");

    let source_path = workspace.path().join("guide.md");
    write_file(&source_path, "Always update docs.");

    let first = AgentsContextImport {
        source: PathBuf::from("guide.md"),
        target: AgentsSource::Global,
        destination_dir: None,
    };
    import_agents_context(&global_agents_home, None, cwd, first).expect("first import");

    let second = AgentsContextImport {
        source: PathBuf::from("guide.md"),
        target: AgentsSource::Global,
        destination_dir: None,
    };
    let err = import_agents_context(&global_agents_home, None, cwd, second)
        .expect_err("should fail on duplicate");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn import_rejects_files_over_size_limit() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");

    let source_path = workspace.path().join("large.md");
    if let Some(parent) = source_path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let mut file = fs::File::create(&source_path).expect("create large file");
    file.write_all(&vec![b'a'; MAX_FILE_BYTES + 1])
        .expect("write large content");

    let import = AgentsContextImport {
        source: PathBuf::from("large.md"),
        target: AgentsSource::Global,
        destination_dir: None,
    };

    let err = import_agents_context(&global_agents_home, None, cwd, import)
        .expect_err("reject oversized file");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(format!("{err}").contains("files must be <="));
}

#[test]
fn import_rejects_directory_exceeding_file_cap() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();
    let global_agents_home = workspace.path().join(".agents");

    let source_dir = workspace.path().join("notes");
    fs::create_dir_all(&source_dir).expect("create source");
    for idx in 0..=MAX_IMPORT_FILES {
        let path = source_dir.join(format!("file_{idx}.md"));
        write_file(&path, "memo");
    }

    let import = AgentsContextImport {
        source: PathBuf::from("notes"),
        target: AgentsSource::Global,
        destination_dir: None,
    };

    let err = import_agents_context(&global_agents_home, None, cwd, import)
        .expect_err("reject directory exceeding cap");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(format!("{err}").contains("more than"));
}

fn pathbuf(path: &str) -> PathBuf {
    PathBuf::from(path)
}
