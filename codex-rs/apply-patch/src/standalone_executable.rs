use clap::Parser;
use clap::Subcommand;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::io::{self};
use std::path::Path;
use std::path::PathBuf;

use crate::ApplyPatchConfig;
use crate::ApplyPatchError;
use crate::OperationStatus;
use crate::PatchReport;
use crate::PatchReportMode;
use crate::PatchReportStatus;
use crate::TaskStatus;
use crate::apply_patch_with_config;
use crate::emit_report;
use crate::formatting::FormattingOutcome;
use crate::post_checks::PostCheckOutcome;
use crate::refactor_catalog::ScriptCatalog;
use crate::refactor_script::RefactorScript;
use crate::report_to_machine_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Apply,
    DryRun,
    Amend,
    Explain,
    Preview,
}

#[derive(Parser, Debug)]
#[command(
    name = "apply_patch",
    about = "Apply Serena-style *** Begin Patch blocks to the filesystem.",
    disable_help_subcommand = true
)]
struct ApplyArgs {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Apply the patch to disk (explicit opt-in).
    Apply,
    /// Validate the patch and show the summary without writing changes.
    DryRun,
    /// Plan the patch without touching the filesystem; prints the same report as `dry-run`.
    Explain,
    /// Apply only the amended portion of a patch after a previous failure.
    Amend,
    /// Show per-operation previews in dry-run mode.
    Preview,
    /// Script catalog helpers.
    Scripts {
        #[command(subcommand)]
        action: ScriptsCommand,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
enum ScriptsCommand {
    /// List entries from refactors/catalog.json (use --json for machine output).
    List {
        #[arg(long)]
        json: bool,
    },
}

pub fn main() -> ! {
    let exit_code = run_main();
    std::process::exit(exit_code);
}

pub fn run_main() -> i32 {
    let raw_args: Vec<String> = std::env::args().collect();
    let cli = match ApplyArgs::try_parse_from(&raw_args) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("{err}");
            return 2;
        }
    };

    match run(cli) {
        Ok(()) => 0,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            1
        }
    }
}

fn run(cli: ApplyArgs) -> Result<(), String> {
    if let Some(Command::Scripts { action }) = &cli.command {
        return run_scripts_command(action);
    }

    let patch = load_patch().map_err(|err| err.to_string())?;
    let mut config = build_config();
    let operation_blocks = extract_operation_blocks(&patch);

    let mode = match cli.command {
        Some(Command::Apply) => Mode::Apply,
        Some(Command::DryRun) => Mode::DryRun,
        Some(Command::Explain) => Mode::Explain,
        Some(Command::Amend) => Mode::Amend,
        Some(Command::Preview) => Mode::Preview,
        Some(Command::Scripts { .. }) => unreachable!(),
        None => Mode::DryRun,
    };

    if matches!(mode, Mode::DryRun | Mode::Explain | Mode::Preview) {
        config.mode = PatchReportMode::DryRun;
    }

    let mut stdout = io::stdout();
    let stdout_is_terminal = stdout.is_terminal();
    let stdin_is_terminal = io::stdin().is_terminal();
    let emit_options = EmitOutputsOptions {
        show_summary: true,
        rich_summary: stdout_is_terminal,
    };

    match apply_patch_with_config(&patch, &config) {
        Ok(mut report) => {
            report.amendment_template = None;
            if matches!(mode, Mode::Preview) {
                emit_previews(
                    &report,
                    &mut stdout,
                    stdout_is_terminal && stdin_is_terminal,
                )?;
            }
            let json_line = emit_outputs(&report, &mut stdout, &emit_options)?;
            writeln!(stdout, "{json_line}").map_err(|err| err.to_string())?;
            Ok(())
        }
        Err(ApplyPatchError::Execution(mut exec_error)) => {
            let template = build_amendment_template(&operation_blocks, &exec_error.report);
            exec_error.report.amendment_template = template.clone();
            let json_line = emit_outputs(&exec_error.report, &mut stdout, &emit_options)?;
            if let Some(template) = template {
                writeln!(
                    stdout,
                    "Amendment template (edit and reapply with `apply_patch`):"
                )
                .map_err(|err| err.to_string())?;
                writeln!(stdout, "{template}").map_err(|err| err.to_string())?;
            }
            writeln!(stdout, "{json_line}").map_err(|err| err.to_string())?;
            Err(exec_error.message)
        }
        Err(other) => Err(other.to_string()),
    }
}

fn build_config() -> ApplyPatchConfig {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ApplyPatchConfig {
        root,
        ..ApplyPatchConfig::default()
    }
}

struct EmitOutputsOptions {
    show_summary: bool,
    rich_summary: bool,
}

fn emit_outputs(
    report: &PatchReport,
    stdout: &mut impl Write,
    options: &EmitOutputsOptions,
) -> Result<String, String> {
    if options.show_summary {
        emit_report(stdout, report).map_err(|err| err.to_string())?;
        if matches!(report.status, PatchReportStatus::Failed) {
            for error in &report.errors {
                writeln!(stdout, "Error: {error}").map_err(|err| err.to_string())?;
            }
        }
        if !report.formatting.is_empty() {
            write_formatting_section(stdout, &report.formatting).map_err(|err| err.to_string())?;
        }
        if !report.post_checks.is_empty() {
            write_post_checks_section(stdout, &report.post_checks)
                .map_err(|err| err.to_string())?;
        }
        if matches!(report.status, PatchReportStatus::Failed) && !report.diagnostics.is_empty() {
            writeln!(stdout, "Diagnostics:").map_err(|err| err.to_string())?;
            for diag in &report.diagnostics {
                writeln!(stdout, "- {}: {}", diag.code, diag.message)
                    .map_err(|err| err.to_string())?;
            }
        }
        if options.rich_summary {
            writeln!(stdout).map_err(|err| err.to_string())?;
        }
    }

    let machine_json = report_to_machine_json(report);
    let json_str = serde_json::to_string(&machine_json).map_err(|err| err.to_string())?;
    Ok(json_str)
}

fn write_formatting_section(
    stdout: &mut impl Write,
    items: &[FormattingOutcome],
) -> std::io::Result<()> {
    writeln!(stdout, "Formatting:")?;
    for item in items {
        let scope = item.scope.as_deref().unwrap_or("-");
        let duration = format_duration(item.duration_ms);
        let mut line = format!(
            "- {} ({}) {} {}",
            item.tool,
            scope,
            status_icon(item.status),
            duration
        );
        if let Some(note) = item.note.as_ref().filter(|s| !s.is_empty()) {
            line.push_str(" – ");
            line.push_str(note);
        }
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn run_scripts_command(action: &ScriptsCommand) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|err| err.to_string())?;
    let Some(catalog) = ScriptCatalog::load(&root).map_err(|err| err.to_string())? else {
        println!("No refactors/catalog.json found under {}", root.display());
        return Ok(());
    };

    match action {
        ScriptsCommand::List { json } => list_scripts(&root, &catalog, *json),
    }
}

fn list_scripts(root: &Path, catalog: &ScriptCatalog, emit_json: bool) -> Result<(), String> {
    let rows = build_script_rows(root, catalog);
    if emit_json {
        let payload: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "path": row.path,
                    "name": row.name,
                    "version": row.version,
                    "hash": row.hash,
                    "description": row.description,
                    "labels": row.labels,
                })
            })
            .collect();
        let serialized = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
        println!("{serialized}");
        return Ok(());
    }

    if rows.is_empty() {
        println!("No scripts registered in refactors/catalog.json.");
        return Ok(());
    }

    println!("Scripts in refactors/catalog.json:");
    println!("{:<32} {:<20} {:<10} Hash", "Path", "Name", "Version");
    for row in rows {
        let name = row.name.as_deref().unwrap_or("-");
        println!(
            "{:<32} {:<20} {:<10} {}",
            row.path, name, row.version, row.hash
        );
        if let Some(desc) = row.description.as_deref() {
            println!("    {desc}");
        }
        if !row.labels.is_empty() {
            println!("    labels: {}", row.labels.join(", "));
        }
    }
    Ok(())
}

fn build_script_rows(root: &Path, catalog: &ScriptCatalog) -> Vec<ScriptListRow> {
    let mut rows = Vec::new();
    for entry in catalog.entries() {
        let path = root.join(&entry.path);
        let vars = BTreeMap::new();
        let metadata = match RefactorScript::load_from_path(&path, &vars, None) {
            Ok(script) => Some(script.metadata),
            Err(err) => {
                eprintln!("Warning: failed to load {} ({err})", path.display());
                None
            }
        };
        let name = entry
            .name
            .clone()
            .or_else(|| metadata.as_ref().map(|meta| meta.name.clone()));
        let description = metadata.as_ref().and_then(|meta| meta.description.clone());
        let labels = metadata
            .as_ref()
            .map(|meta| meta.labels.clone())
            .unwrap_or_default();
        rows.push(ScriptListRow {
            path: entry.path.clone(),
            name,
            version: entry.version.clone(),
            hash: entry.hash.clone(),
            description,
            labels,
        });
    }
    rows
}

struct ScriptListRow {
    path: String,
    name: Option<String>,
    version: String,
    hash: String,
    description: Option<String>,
    labels: Vec<String>,
}

fn emit_previews(
    report: &PatchReport,
    stdout: &mut impl Write,
    interactive: bool,
) -> Result<(), String> {
    let previews: Vec<_> = report
        .operations
        .iter()
        .filter_map(|op| op.preview.as_ref().map(|text| (op, text)))
        .collect();
    if previews.is_empty() {
        writeln!(stdout, "No AST previews available.").map_err(|err| err.to_string())?;
        return Ok(());
    }
    writeln!(stdout, "AST operation previews:").map_err(|err| err.to_string())?;
    let mut input = String::new();
    for (index, (operation, preview)) in previews.iter().enumerate() {
        writeln!(
            stdout,
            "Preview {}/{}: {}",
            index + 1,
            previews.len(),
            operation.path.display()
        )
        .map_err(|err| err.to_string())?;
        if let Some(message) = &operation.message {
            writeln!(stdout, "  {message}").map_err(|err| err.to_string())?;
        }
        writeln!(stdout, "{preview}").map_err(|err| err.to_string())?;
        if interactive && index + 1 < previews.len() {
            writeln!(stdout, "Press Enter for next preview (q to quit)...")
                .map_err(|err| err.to_string())?;
            input.clear();
            io::stdin()
                .read_line(&mut input)
                .map_err(|err| err.to_string())?;
            if input.trim().eq_ignore_ascii_case("q") {
                break;
            }
        }
    }
    writeln!(stdout).map_err(|err| err.to_string())?;
    Ok(())
}

fn write_post_checks_section(
    stdout: &mut impl Write,
    items: &[PostCheckOutcome],
) -> std::io::Result<()> {
    writeln!(stdout, "Post-checks:")?;
    for item in items {
        let duration = format_duration(item.duration_ms);
        let mut line = format!("- {} {} {}", item.name, status_icon(item.status), duration);
        if let Some(note) = item.note.as_ref().filter(|s| !s.is_empty()) {
            line.push_str(" – ");
            line.push_str(note);
        } else if item.status == TaskStatus::Failed
            && let Some(stderr) = item.stderr.as_ref().filter(|s| !s.is_empty())
        {
            line.push_str(" – ");
            line.push_str(stderr);
        }
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn status_icon(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Applied => "✔",
        TaskStatus::Skipped => "⚠",
        TaskStatus::Failed => "✘",
    }
}

fn format_duration(duration_ms: u128) -> String {
    if duration_ms >= 1000 {
        let secs = (duration_ms as f64) / 1000.0;
        format!("{secs:.1} s")
    } else {
        format!("{duration_ms} ms")
    }
}

fn extract_operation_blocks(patch: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut collecting = false;

    for line in patch.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == "*** Begin Patch" {
            continue;
        }
        if trimmed == "*** End Patch" {
            if collecting && !current.is_empty() {
                blocks.push(current.clone());
            }
            break;
        }
        if is_operation_header(trimmed) {
            if collecting && !current.is_empty() {
                blocks.push(current.clone());
                current.clear();
            }
            collecting = true;
        }

        if collecting {
            current.push_str(line);
        }
    }

    if collecting && !current.is_empty() {
        blocks.push(current);
    }

    blocks
}

fn is_operation_header(line: &str) -> bool {
    matches!(
        line,
        line if line.starts_with("*** Add File:")
            || line.starts_with("*** Delete File:")
            || line.starts_with("*** Update File:")
            || line.starts_with("*** Insert Before Symbol:")
            || line.starts_with("*** Insert After Symbol:")
            || line.starts_with("*** Replace Symbol Body:")
    )
}

fn build_amendment_template(blocks: &[String], report: &PatchReport) -> Option<String> {
    let mut sections = Vec::new();
    for (idx, op) in report.operations.iter().enumerate() {
        if op.status == OperationStatus::Failed
            && let Some(block) = blocks.get(idx)
        {
            sections.push(block);
        }
    }

    let selected_blocks: Vec<&String> = if sections.is_empty() {
        if blocks.is_empty() {
            return None;
        }
        blocks.iter().collect()
    } else {
        sections
    };

    let mut template = String::from("*** Begin Patch\n");
    for block in selected_blocks {
        template.push_str(block);
        if !block.ends_with('\n') {
            template.push('\n');
        }
    }
    template.push_str("*** End Patch\n");
    Some(template)
}

fn load_patch() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "No patch content provided. Supply a *** Begin Patch block via STDIN.",
        ));
    }
    Ok(buf)
}
