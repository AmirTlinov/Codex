use clap::Parser;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::io::{self};
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
use crate::report_to_machine_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Apply,
    DryRun,
    Amend,
    Explain,
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

#[derive(clap::Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// Validate the patch and show the summary without writing changes.
    DryRun,
    /// Plan the patch without touching the filesystem; prints the same report as `dry-run`.
    Explain,
    /// Apply only the amended portion of a patch after a previous failure.
    Amend,
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
    let patch = load_patch().map_err(|err| err.to_string())?;
    let mut config = build_config();
    let operation_blocks = extract_operation_blocks(&patch);

    let mode = match cli.command {
        Some(Command::DryRun) => Mode::DryRun,
        Some(Command::Explain) => Mode::Explain,
        Some(Command::Amend) => Mode::Amend,
        None => Mode::Apply,
    };

    if matches!(mode, Mode::DryRun | Mode::Explain) {
        config.mode = PatchReportMode::DryRun;
    }

    let mut stdout = io::stdout();
    let stdout_is_terminal = stdout.is_terminal();
    let emit_options = EmitOutputsOptions {
        show_summary: true,
        rich_summary: stdout_is_terminal,
    };

    match apply_patch_with_config(&patch, &config) {
        Ok(mut report) => {
            report.amendment_template = None;
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
        } else if let (TaskStatus::Failed, Some(stderr)) =
            (item.status, item.stderr.as_ref().filter(|s| !s.is_empty()))
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
        if let (OperationStatus::Failed, Some(block)) = (op.status, blocks.get(idx)) {
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
