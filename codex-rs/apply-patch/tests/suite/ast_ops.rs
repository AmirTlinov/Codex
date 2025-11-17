use super::cli::run_apply_patch_success;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write_catalog_entries(root: &Path, entries: &[(&str, &str, &str)]) -> anyhow::Result<()> {
    let mut scripts = Vec::with_capacity(entries.len());
    for (rel_path, version, name) in entries {
        let hash = compute_script_hash(&root.join(rel_path))?;
        scripts.push(json!({
            "path": rel_path,
            "version": version,
            "name": name,
            "hash": hash,
        }));
    }
    let catalog = json!({
        "version": 1,
        "scripts": scripts,
    });
    fs::write(
        root.join("refactors/catalog.json"),
        serde_json::to_string_pretty(&catalog)?,
    )?;
    Ok(())
}

fn compute_script_hash(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[test]
fn test_ast_rename_symbol_with_propagation() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = tmp.path().join("lib.rs");
    fs::write(
        &file,
        "fn greet() { println!(\"hi\"); }\nfn main() { greet(); }\n",
    )?;

    let patch = "*** Begin Patch\n*** Ast Operation: lib.rs op=rename-symbol symbol=greet new_name=salute propagate=file\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;

    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("fn salute()"));
    assert!(contents.contains("salute();"));
    Ok(())
}

#[test]
fn test_ast_update_imports_adds_and_removes() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = tmp.path().join("main.rs");
    fs::write(
        &file,
        "use crate::alpha::One;\n\nfn run() { let _ = One; }\n",
    )?;

    let patch = "*** Begin Patch\n*** Ast Operation: main.rs lang=rust op=update-imports\n+add use crate::beta::Two;\n+remove use crate::alpha::One;\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;

    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("use crate::beta::Two;"));
    assert!(!contents.contains("use crate::alpha::One;"));
    Ok(())
}

#[test]
fn test_ast_template_inserts_into_body() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    let file = tmp.path().join("worker.rs");
    fs::write(&file, "fn crunch() {\n    let value = 1;\n}\n")?;

    let patch = "*** Begin Patch\n*** Ast Operation: worker.rs op=template mode=body-start symbol=crunch\n+println!(\"start\");\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;

    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("println!(\"start\");"));
    Ok(())
}

#[test]
fn test_ast_script_executes_multiple_steps() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    fs::create_dir_all(tmp.path().join("refactors"))?;
    let file = tmp.path().join("src/lib.rs");
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, "fn alpha() {\n    println!(\"alpha\");\n}\n")?;
    let script_path = tmp.path().join("refactors/rename.toml");
    fs::write(
        &script_path,
        r#"name = "Rename"
version = "0.1.0"

[[steps]]
path = "src/lib.rs"
op = "rename"
symbol = "alpha"
new_name = "beta"

[[steps]]
path = "src/lib.rs"
op = "template"
symbol = "beta"
mode = "body-end"
payload = ["println!(\"beta done\");"]
"#,
    )?;
    write_catalog_entries(tmp.path(), &[("refactors/rename.toml", "0.1.0", "Rename")])?;
    let patch = "*** Begin Patch\n*** Ast Script: refactors/rename.toml\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;
    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("fn beta()"));
    assert!(contents.contains("println!(\"beta done\");"));
    Ok(())
}

#[test]
fn test_ast_script_query_inserts_attributes() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    fs::create_dir_all(tmp.path().join("refactors"))?;
    let file = tmp.path().join("src/lib.rs");
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(
        &file,
        "fn handle_one() {}\nfn other() {}\nfn handle_two() {}\n",
    )?;
    let script_path = tmp.path().join("refactors/instrument.toml");
    fs::write(
        &script_path,
        r##"name = "Instrument"
version = "0.1.0"

[[steps]]
path = "src/lib.rs"
lang = "rust"
query = "(function_item name: (identifier) @name (#match? @name \"^handle_\"))"
capture = "name"
op = "insert-attributes"
placement = "before"
payload = ["#[instrument]"]
"##,
    )?;
    write_catalog_entries(
        tmp.path(),
        &[("refactors/instrument.toml", "0.1.0", "Instrument")],
    )?;
    let patch = "*** Begin Patch\n*** Ast Script: refactors/instrument.toml\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;
    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("#[instrument]\nfn handle_one()"));
    assert!(contents.contains("#[instrument]\nfn handle_two()"));
    assert!(!contents.contains("#[instrument]\nfn other()"));
    Ok(())
}

#[test]
fn test_ast_script_json_format() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    fs::create_dir_all(tmp.path().join("refactors"))?;
    let file = tmp.path().join("src/lib.rs");
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, "fn greet() {}\n")?;
    let script_rel = "refactors/json_script.json";
    fs::write(
        tmp.path().join(script_rel),
        r#"{
  "name": "JsonScript",
  "version": "0.1.0",
  "steps": [
    {
      "path": "src/lib.rs",
      "op": "rename",
      "symbol": "greet",
      "new_name": "greet_json"
    }
  ]
}
"#,
    )?;
    write_catalog_entries(tmp.path(), &[(script_rel, "0.1.0", "JsonScript")])?;
    let patch = "*** Begin Patch\n*** Ast Script: refactors/json_script.json\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;
    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("fn greet_json()"));
    Ok(())
}

#[test]
fn test_ast_script_starlark_format() -> anyhow::Result<()> {
    let tmp = tempdir()?;
    fs::create_dir_all(tmp.path().join("refactors"))?;
    let file = tmp.path().join("src/lib.rs");
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(
        &file,
        "fn handle_one() {}\nfn other() {}\nfn handle_two() {}\n",
    )?;
    let script_rel = "refactors/query.star";
    fs::write(
        tmp.path().join(script_rel),
        r##"{
    "name": "StarScript",
    "version": "0.1.0",
    "steps": [
        {
            "path": "src/lib.rs",
            "lang": "rust",
            "query": "(function_item name: (identifier) @name (#match? @name \"^handle_\"))",
            "capture": "name",
            "op": "insert-attributes",
            "placement": "before",
            "payload": ["#[cold]"]
        }
    ]
}
"##,
    )?;
    write_catalog_entries(tmp.path(), &[(script_rel, "0.1.0", "StarScript")])?;
    let patch = "*** Begin Patch\n*** Ast Script: refactors/query.star\n*** End Patch\n";
    run_apply_patch_success(tmp.path(), patch)?;
    let contents = fs::read_to_string(&file)?;
    assert!(contents.contains("#[cold]\nfn handle_one()"));
    assert!(contents.contains("#[cold]\nfn handle_two()"));
    assert!(!contents.contains("#[cold]\nfn other()"));
    Ok(())
}
