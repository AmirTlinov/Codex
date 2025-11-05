mod artifacts;
mod ast;
mod fallback;
mod formatting;
mod parser;
mod post_checks;
mod seek_sequence;
mod standalone_executable;

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::io::Write as _;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::process::{self};
use std::str::Utf8Error;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use artifacts::ArtifactSummary;
use artifacts::UnappliedEntry;
use artifacts::UnappliedKind;
use artifacts::summarize_unapplied;
use formatting::FormattingOutcome;
use post_checks::PostCheckOutcome;

use anyhow::Context;
use anyhow::Result;
use ast::LineColumnRange;
use ast::SymbolPath;
use ast::SymbolResolution;
use ast::SymbolTarget;
use ast::byte_range_to_line_col;
use ast::resolve_locator;
use encoding_rs::Encoding;
use fallback::FallbackMatch;
use filetime::FileTime;
pub use parser::Hunk;
pub use parser::ParseError;
use parser::ParseError::*;
use parser::UpdateFileChunk;
pub use parser::parse_patch;
use serde_json::json;
use similar::ChangeTag;
use similar::TextDiff;
use thiserror::Error;
use tree_sitter::LanguageError;
use tree_sitter::Node;
use tree_sitter::Parser;
use tree_sitter_bash::LANGUAGE as BASH;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::Builder as TempFileBuilder;

pub use standalone_executable::main;

/// Detailed instructions for gpt-4.1 on how to use the `apply_patch` tool.
pub const APPLY_PATCH_TOOL_INSTRUCTIONS: &str = include_str!("../apply_patch_tool_instructions.md");

const APPLY_PATCH_COMMANDS: [&str; 2] = ["apply_patch", "applypatch"];
const BEGIN_PATCH_COMMANDS: [&str; 2] = ["begin_patch", "beginpatch"];
const APPLY_PATCH_MACHINE_SCHEMA: &str = "apply_patch/v2";

#[derive(Debug, Error, PartialEq)]
pub enum ApplyPatchError {
    #[error(transparent)]
    ParseError(#[from] ParseError),
    #[error(transparent)]
    IoError(#[from] IoError),
    /// Error that occurs while computing replacements when applying patch chunks
    #[error("{0}")]
    ComputeReplacements(Box<ConflictDiagnostic>),
    /// Patch application produced a report with failed operations.
    #[error("{0}")]
    Execution(Box<PatchExecutionError>),
    /// A raw patch body was provided without an explicit `apply_patch` invocation.
    #[error(
        "patch detected without explicit call to apply_patch. Rerun as [\"apply_patch\", \"<patch>\"]"
    )]
    ImplicitInvocation,
    #[error("unsupported encoding '{0}'")]
    UnsupportedEncoding(String),
    #[error("encoding error: {0}")]
    EncodingError(String),
}

fn update_symbol_change(
    changes: &mut HashMap<PathBuf, ApplyPatchFileChange>,
    path: PathBuf,
    symbol: &SymbolPath,
    new_lines: &[String],
    kind: SymbolEditKind,
) -> Result<(), ApplyPatchError> {
    let (mut original_disk, disk_missing) = match fs::read(&path) {
        Ok(bytes) => (decode_content(&bytes, "utf-8")?, false),
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                (String::new(), true)
            } else {
                return Err(ApplyPatchError::IoError(IoError {
                    context: format!("Failed to read {}", path.display()),
                    source: err,
                }));
            }
        }
    };

    let mut move_path = None;
    let current_content = if let Some(existing) = changes.get(&path) {
        match existing {
            ApplyPatchFileChange::Update {
                new_content,
                move_path: existing_move,
                ..
            } => {
                move_path = existing_move.clone();
                new_content.clone()
            }
            ApplyPatchFileChange::Add { .. } => {
                return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                    "Cannot apply symbol-directed edit to newly added file {}",
                    path.display()
                ))));
            }
            ApplyPatchFileChange::Delete { .. } => {
                return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                    "Cannot apply symbol-directed edit to deleted file {}",
                    path.display()
                ))));
            }
        }
    } else {
        if disk_missing {
            return Err(ApplyPatchError::IoError(IoError {
                context: format!("Cannot edit {} because it does not exist", path.display()),
                source: std::io::Error::other("missing file"),
            }));
        }
        original_disk.clone()
    };

    let (updated_content, _) = compute_symbol_edit(
        &current_content,
        &path,
        symbol,
        new_lines,
        kind,
        SymbolFallbackMode::Fuzzy,
    )?;

    if disk_missing {
        original_disk = current_content;
    }

    let diff = TextDiff::from_lines(&original_disk, &updated_content);
    let unified_diff = diff.unified_diff().context_radius(3).to_string();

    changes.insert(
        path,
        ApplyPatchFileChange::Update {
            unified_diff,
            move_path,
            new_content: updated_content,
        },
    );

    Ok(())
}

impl ApplyPatchError {
    pub fn conflict(&self) -> Option<&ConflictDiagnostic> {
        match self {
            ApplyPatchError::ComputeReplacements(details) => Some(details.as_ref()),
            ApplyPatchError::Execution(exec) => exec.conflict(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ApplyPatchError {
    fn from(err: std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: err,
        })
    }
}

impl From<&std::io::Error> for ApplyPatchError {
    fn from(err: &std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: std::io::Error::new(err.kind(), err.to_string()),
        })
    }
}

#[derive(Debug, Error)]
#[error("{context}: {source}")]
pub struct IoError {
    context: String,
    #[source]
    source: std::io::Error,
}

impl PartialEq for IoError {
    fn eq(&self, other: &Self) -> bool {
        self.context == other.context && self.source.to_string() == other.source.to_string()
    }
}

#[derive(Debug, PartialEq)]
pub enum MaybeApplyPatch {
    Body(ApplyPatchArgs),
    ShellParseError(ExtractHeredocError),
    PatchParseError(ParseError),
    NotApplyPatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymbolFallbackMode {
    Ast,
    #[default]
    Fuzzy,
    Disabled,
}

impl SymbolFallbackMode {
    fn as_str(&self) -> &'static str {
        match self {
            SymbolFallbackMode::Ast => "ast",
            SymbolFallbackMode::Fuzzy => "fuzzy",
            SymbolFallbackMode::Disabled => "disabled",
        }
    }
}

impl fmt::Display for SymbolFallbackMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolFallbackStrategy {
    Ast,
    Scoped,
    Identifier,
    Substring,
}

impl SymbolFallbackStrategy {
    fn as_str(&self) -> &'static str {
        match self {
            SymbolFallbackStrategy::Ast => "ast",
            SymbolFallbackStrategy::Scoped => "scoped",
            SymbolFallbackStrategy::Identifier => "identifier",
            SymbolFallbackStrategy::Substring => "substring",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolEditKind {
    InsertBefore,
    InsertAfter,
    ReplaceBody,
}

impl SymbolEditKind {
    fn as_str(&self) -> &'static str {
        match self {
            SymbolEditKind::InsertBefore => "insert-before",
            SymbolEditKind::InsertAfter => "insert-after",
            SymbolEditKind::ReplaceBody => "replace-body",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLocation {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl From<LineColumnRange> for SymbolLocation {
    fn from(range: LineColumnRange) -> Self {
        Self {
            start_line: range.start_line,
            start_col: range.start_col,
            end_line: range.end_line,
            end_col: range.end_col,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOperationSummary {
    pub kind: SymbolEditKind,
    pub symbol: SymbolPath,
    pub strategy: SymbolFallbackStrategy,
    pub location: SymbolLocation,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
struct SymbolEditPlanResult {
    original_contents: String,
    new_contents: String,
    summary: SymbolOperationSummary,
}

#[derive(Debug, Clone)]
struct SymbolMatchContext {
    body_range: Option<std::ops::Range<usize>>,
    insert_before: usize,
    insert_after: usize,
    strategy: SymbolFallbackStrategy,
    location: LineColumnRange,
    reason: Option<String>,
}

/// Both the raw PATCH argument to `apply_patch` as well as the PATCH argument
/// parsed into hunks.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub hunks: Vec<Hunk>,
    pub workdir: Option<String>,
}

pub fn maybe_parse_apply_patch(argv: &[String]) -> MaybeApplyPatch {
    match argv {
        // Direct invocation: apply_patch <patch>
        [cmd, body] if APPLY_PATCH_COMMANDS.contains(&cmd.as_str()) => match parse_patch(body) {
            Ok(source) => MaybeApplyPatch::Body(source),
            Err(e) => MaybeApplyPatch::PatchParseError(e),
        },
        // Bash heredoc form: (optional `cd <path> &&`) apply_patch <<'EOF' ...
        [bash, flag, script] if bash == "bash" && flag == "-lc" => {
            match extract_apply_patch_from_bash(script) {
                Ok((body, workdir)) => match parse_patch(&body) {
                    Ok(mut source) => {
                        source.workdir = workdir;
                        MaybeApplyPatch::Body(source)
                    }
                    Err(e) => MaybeApplyPatch::PatchParseError(e),
                },
                Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch) => {
                    MaybeApplyPatch::NotApplyPatch
                }
                Err(e) => MaybeApplyPatch::ShellParseError(e),
            }
        }
        _ => MaybeApplyPatch::NotApplyPatch,
    }
}

#[derive(Debug, PartialEq)]
pub enum ApplyPatchFileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
        /// new_content that will result after the unified_diff is applied.
        new_content: String,
    },
}

#[derive(Debug, PartialEq)]
pub enum MaybeApplyPatchVerified {
    /// `argv` corresponded to an `apply_patch` invocation, and these are the
    /// resulting proposed file changes.
    Body(ApplyPatchAction),
    /// `argv` could not be parsed to determine whether it corresponds to an
    /// `apply_patch` invocation.
    ShellParseError(ExtractHeredocError),
    /// `argv` corresponded to an `apply_patch` invocation, but it could not
    /// be fulfilled due to the specified error.
    CorrectnessError(ApplyPatchError),
    /// `argv` decidedly did not correspond to an `apply_patch` invocation.
    NotApplyPatch,
}

/// ApplyPatchAction is the result of parsing an `apply_patch` command. By
/// construction, all paths should be absolute paths.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchAction {
    changes: HashMap<PathBuf, ApplyPatchFileChange>,

    /// The raw patch argument that can be used with `apply_patch` as an exec
    /// call. i.e., if the original arg was parsed in "lenient" mode with a
    /// heredoc, this should be the value without the heredoc wrapper.
    pub patch: String,

    /// The working directory that was used to resolve relative paths in the patch.
    pub cwd: PathBuf,

    /// Optional explicit command to realize the patch. When `None`, the built-in
    /// `apply_patch` entry point is used.
    pub command: Option<Vec<String>>,
}

impl ApplyPatchAction {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns the changes that would be made by applying the patch.
    pub fn changes(&self) -> &HashMap<PathBuf, ApplyPatchFileChange> {
        &self.changes
    }

    /// Should be used exclusively for testing. (Not worth the overhead of
    /// creating a feature flag for this.)
    pub fn new_add_for_test(path: &Path, content: String) -> Self {
        if !path.is_absolute() {
            panic!("path must be absolute");
        }

        #[expect(clippy::expect_used)]
        let filename = path
            .file_name()
            .expect("path should not be empty")
            .to_string_lossy();
        let patch = format!(
            r#"*** Begin Patch
*** Update File: {filename}
@@
+ {content}
*** End Patch"#,
        );
        let changes = HashMap::from([(path.to_path_buf(), ApplyPatchFileChange::Add { content })]);
        #[expect(clippy::expect_used)]
        Self {
            changes,
            cwd: path
                .parent()
                .expect("path should have parent")
                .to_path_buf(),
            patch,
            command: None,
        }
    }
}

/// cwd must be an absolute path so that we can resolve relative paths in the
/// patch.
pub fn maybe_parse_apply_patch_verified(argv: &[String], cwd: &Path) -> MaybeApplyPatchVerified {
    // Detect a raw patch body passed directly as the command or as the body of a bash -lc
    // script. In these cases, report an explicit error rather than applying the patch.
    match argv {
        [body] => {
            if parse_patch(body).is_ok() {
                return MaybeApplyPatchVerified::CorrectnessError(
                    ApplyPatchError::ImplicitInvocation,
                );
            }
        }
        [bash, flag, script] if bash == "bash" && flag == "-lc" => {
            if parse_patch(script).is_ok() {
                return MaybeApplyPatchVerified::CorrectnessError(
                    ApplyPatchError::ImplicitInvocation,
                );
            }
        }
        _ => {}
    }

    match maybe_parse_apply_patch(argv) {
        MaybeApplyPatch::Body(args) => match build_apply_patch_action(args, cwd, None) {
            Ok(action) => MaybeApplyPatchVerified::Body(action),
            Err(err) => MaybeApplyPatchVerified::CorrectnessError(err),
        },
        MaybeApplyPatch::ShellParseError(e) => MaybeApplyPatchVerified::ShellParseError(e),
        MaybeApplyPatch::PatchParseError(e) => MaybeApplyPatchVerified::CorrectnessError(e.into()),
        MaybeApplyPatch::NotApplyPatch => maybe_parse_begin_patch_verified(argv, cwd),
    }
}

pub fn maybe_parse_begin_patch_verified(argv: &[String], cwd: &Path) -> MaybeApplyPatchVerified {
    match maybe_extract_begin_patch_args(argv, cwd) {
        Ok(Some((args, command))) => match build_apply_patch_action(args, cwd, Some(command)) {
            Ok(action) => MaybeApplyPatchVerified::Body(action),
            Err(err) => MaybeApplyPatchVerified::CorrectnessError(err),
        },
        Ok(None) => MaybeApplyPatchVerified::NotApplyPatch,
        Err(err) => MaybeApplyPatchVerified::CorrectnessError(err),
    }
}

/// Extract the heredoc body (and optional `cd` workdir) from a `bash -lc` script
/// that invokes the apply_patch tool using a heredoc.
///
/// Accepted scripts must contain a single heredoc redirected statement that invokes
/// `apply_patch`. Optional preparatory commands such as `set -e` or a standalone
/// `cd <path>` may precede the statement on separate lines. Within the redirected
/// statement we allow either a bare `apply_patch <<'EOF' ... EOF` or a guarded
/// `... &&/; cd <path> && apply_patch <<'EOF' ... EOF`, where the inline `cd` supplies
/// the working directory for the patch (the last such `cd` wins). We reject constructs
/// involving pipes or `||`, as well as trailing commands after `apply_patch`, because
/// those alter execution semantics we cannot reproduce safely.
///
/// Returns `(heredoc_body, Some(path))` when the `cd` variant matches, or
/// `(heredoc_body, None)` for the direct form. Errors are returned if the script
/// cannot be parsed or does not match the allowed patterns.
fn extract_apply_patch_from_bash(
    src: &str,
) -> std::result::Result<(String, Option<String>), ExtractHeredocError> {
    let lang = BASH.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(ExtractHeredocError::FailedToLoadBashGrammar)?;
    let tree = parser
        .parse(src, None)
        .ok_or(ExtractHeredocError::FailedToParsePatchIntoAst)?;

    let root = tree.root_node();
    let bytes = src.as_bytes();

    let mut cursor = root.walk();
    let children: Vec<Node> = root.named_children(&mut cursor).collect();

    let mut workdir_from_prefix: Option<String> = None;
    let mut result: Option<(String, Option<String>)> = None;

    for (idx, child) in children.iter().enumerate() {
        match child.kind() {
            "command" => {
                if result.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                match classify_prefix_command(*child, bytes)? {
                    PrefixCommand::Cd(path) => workdir_from_prefix = Some(path),
                    PrefixCommand::Allowed => {}
                };
            }
            "redirected_statement" => {
                if result.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                if let Some((patch, inline_workdir)) = parse_apply_patch_redirected(*child, bytes)?
                {
                    if idx + 1 != children.len() {
                        return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                    }
                    let workdir = inline_workdir.or(workdir_from_prefix.clone());
                    result = Some((patch, workdir));
                } else {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
            }
            "comment" => {
                if result.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
            }
            _ => {
                if result.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                // conservative fallback: unknown constructs mean we cannot extract apply_patch safely
                return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
            }
        }
    }

    result.ok_or(ExtractHeredocError::CommandDidNotStartWithApplyPatch)
}

enum PrefixCommand {
    Cd(String),
    Allowed,
}

fn classify_prefix_command(
    command: Node,
    bytes: &[u8],
) -> std::result::Result<PrefixCommand, ExtractHeredocError> {
    if let Some(path) = parse_cd_command(command, bytes)? {
        return Ok(PrefixCommand::Cd(path));
    }
    if parse_set_command(command, bytes)? {
        return Ok(PrefixCommand::Allowed);
    }
    Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch)
}

fn parse_set_command(
    command: Node,
    bytes: &[u8],
) -> std::result::Result<bool, ExtractHeredocError> {
    if command.kind() != "command" {
        return Ok(false);
    }

    let mut cursor = command.walk();
    let mut name: Option<String> = None;

    for child in command.named_children(&mut cursor) {
        match child.kind() {
            "command_name" => name = Some(node_text(child, bytes)?),
            "comment" => {}
            _ => {}
        }
    }

    Ok(matches!(name.as_deref(), Some("set")))
}

fn parse_apply_patch_redirected(
    node: Node,
    bytes: &[u8],
) -> std::result::Result<Option<(String, Option<String>)>, ExtractHeredocError> {
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(None);
    };

    let mut workdir: Option<String> = None;

    let matches_apply = match body.kind() {
        "command" => parse_apply_patch_command(body, bytes)?,
        "list" => {
            let (is_apply_patch, cd_path) = parse_apply_patch_list(body, bytes)?;
            workdir = cd_path;
            is_apply_patch
        }
        _ => false,
    };

    if !matches_apply {
        return Ok(None);
    }

    let patch = extract_heredoc_body(node, bytes)?;
    Ok(Some((patch, workdir)))
}

fn parse_apply_patch_command(
    command: Node,
    bytes: &[u8],
) -> std::result::Result<bool, ExtractHeredocError> {
    let mut cursor = command.walk();
    let mut name: Option<String> = None;
    let mut extra_args = 0usize;

    for child in command.named_children(&mut cursor) {
        match child.kind() {
            "command_name" => {
                name = Some(node_text(child, bytes)?);
            }
            "word" | "string" | "raw_string" | "command_substitution" | "process_substitution" => {
                extra_args += 1;
            }
            "variable_assignment" => {
                extra_args += 1;
            }
            "comment" => {}
            _ => {
                extra_args += 1;
            }
        }
    }

    let Some(name) = name else {
        return Ok(false);
    };
    if matches!(name.as_str(), "apply_patch" | "applypatch") && extra_args == 0 {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_apply_patch_list(
    list: Node,
    bytes: &[u8],
) -> std::result::Result<(bool, Option<String>), ExtractHeredocError> {
    let mut commands = Vec::new();
    let mut connectors = Vec::new();
    flatten_list(list, bytes, &mut commands, &mut connectors)?;

    if commands.is_empty() {
        return Ok((false, None));
    }

    for connector in &connectors {
        if !is_allowed_connector(connector) {
            return Ok((false, None));
        }
    }

    let mut workdir: Option<String> = None;
    if commands.len() > 1 {
        for command in &commands[0..commands.len() - 1] {
            if let Some(path) = parse_cd_command(*command, bytes)? {
                workdir = Some(path);
                continue;
            }
            if parse_set_command(*command, bytes)? {
                continue;
            }
            return Ok((false, None));
        }
    }

    let Some(&last) = commands.last() else {
        return Ok((false, None));
    };
    if !parse_apply_patch_command(last, bytes)? {
        return Ok((false, None));
    }

    Ok((true, workdir))
}

fn flatten_list<'tree>(
    list: Node<'tree>,
    bytes: &[u8],
    commands: &mut Vec<Node<'tree>>,
    connectors: &mut Vec<String>,
) -> std::result::Result<(), ExtractHeredocError> {
    for idx in 0..list.child_count() {
        let Some(child) = list.child(idx) else {
            return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
        };
        match child.kind() {
            "command" => commands.push(child),
            "list" => flatten_list(child, bytes, commands, connectors)?,
            "comment" => {}
            _ if child.is_named() => {
                return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
            }
            _ => {
                let text = node_text(child, bytes)?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    connectors.push(trimmed.to_string());
                }
            }
        }
    }
    Ok(())
}

fn is_allowed_connector(connector: &str) -> bool {
    matches!(connector, "&&" | ";")
}

fn parse_cd_command(
    command: Node,
    bytes: &[u8],
) -> std::result::Result<Option<String>, ExtractHeredocError> {
    if command.kind() != "command" {
        return Ok(None);
    }

    let mut cursor = command.walk();
    let mut named = command.named_children(&mut cursor);

    let Some(name_node) = named.next() else {
        return Ok(None);
    };
    if name_node.kind() != "command_name" {
        return Ok(None);
    }

    let name = node_text(name_node, bytes)?;
    if name != "cd" {
        return Ok(None);
    }

    let mut arg: Option<String> = None;
    for child in named {
        match child.kind() {
            "word" => {
                if arg.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                arg = Some(node_text(child, bytes)?);
            }
            "string" => {
                if arg.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                arg = Some(extract_double_quoted(child, bytes)?);
            }
            "raw_string" => {
                if arg.is_some() {
                    return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
                }
                arg = Some(extract_single_quoted(child, bytes)?);
            }
            "comment" => {}
            _ => {
                return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
            }
        }
    }

    let Some(path) = arg else {
        return Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch);
    };
    Ok(Some(path))
}

fn extract_heredoc_body(
    node: Node,
    bytes: &[u8],
) -> std::result::Result<String, ExtractHeredocError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "heredoc_redirect" {
            continue;
        }
        let mut redirect_cursor = child.walk();
        for redirect_child in child.named_children(&mut redirect_cursor) {
            if redirect_child.kind() == "heredoc_body" {
                let text = redirect_child
                    .utf8_text(bytes)
                    .map_err(ExtractHeredocError::HeredocNotUtf8)?
                    .trim_end_matches('\n')
                    .to_string();
                return Ok(text);
            }
        }
    }
    Err(ExtractHeredocError::FailedToFindHeredocBody)
}

fn node_text(node: Node, bytes: &[u8]) -> std::result::Result<String, ExtractHeredocError> {
    Ok(node
        .utf8_text(bytes)
        .map_err(ExtractHeredocError::HeredocNotUtf8)?
        .to_string())
}

fn extract_double_quoted(
    node: Node,
    bytes: &[u8],
) -> std::result::Result<String, ExtractHeredocError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "string_content" {
            return node_text(child, bytes);
        }
    }
    Ok(String::new())
}

fn extract_single_quoted(
    node: Node,
    bytes: &[u8],
) -> std::result::Result<String, ExtractHeredocError> {
    let text = node_text(node, bytes)?;
    let stripped = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(text.as_str());
    Ok(stripped.to_string())
}

fn maybe_extract_begin_patch_args(
    argv: &[String],
    cwd: &Path,
) -> Result<Option<(ApplyPatchArgs, Vec<String>)>, ApplyPatchError> {
    let Some(first) = argv.first() else {
        return Ok(None);
    };
    let Some(cmd_name) = Path::new(first)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
    else {
        return Ok(None);
    };
    if !BEGIN_PATCH_COMMANDS.contains(&cmd_name.as_str()) {
        return Ok(None);
    }

    let mut patch_file: Option<PathBuf> = None;
    let mut root_override: Option<PathBuf> = None;
    let mut apply_command = Vec::with_capacity(argv.len());
    apply_command.push(first.clone());

    let mut idx = 1;
    while idx < argv.len() {
        let arg = &argv[idx];
        match arg.as_str() {
            "-f" | "--patch-file" => {
                idx += 1;
                if idx >= argv.len() {
                    return Err(ApplyPatchError::IoError(IoError {
                        context: "begin_patch missing value for -f/--patch-file".to_string(),
                        source: std::io::Error::other("missing value"),
                    }));
                }
                let value = argv[idx].clone();
                patch_file = Some(PathBuf::from(&value));
                apply_command.push(arg.clone());
                apply_command.push(value);
            }
            "-C" | "--root" => {
                idx += 1;
                if idx >= argv.len() {
                    return Err(ApplyPatchError::IoError(IoError {
                        context: "begin_patch missing value for -C/--root".to_string(),
                        source: std::io::Error::other("missing value"),
                    }));
                }
                let value = argv[idx].clone();
                root_override = Some(PathBuf::from(&value));
                apply_command.push(arg.clone());
                apply_command.push(value);
            }
            "--" => {
                apply_command.extend_from_slice(&argv[idx..]);
                break;
            }
            other => {
                if let Some(path) = other.strip_prefix("--patch-file=") {
                    patch_file = Some(PathBuf::from(path));
                    apply_command.push(format!("--patch-file={path}"));
                } else if let Some(path) = other.strip_prefix("--root=") {
                    root_override = Some(PathBuf::from(path));
                    apply_command.push(format!("--root={path}"));
                } else if other.starts_with("--output-format=") || other == "--output-format" {
                    if other == "--output-format" {
                        idx += 1;
                    }
                } else if other.starts_with("--stdout-schema=") || other == "--stdout-schema" {
                    if other == "--stdout-schema" {
                        idx += 1;
                    }
                } else if matches!(
                    other,
                    "--dry-run" | "--plan" | "--no-summary" | "--no-logs" | "--machine"
                ) {
                    // handled at the begin_patch layer
                } else if other == "--preset" {
                    idx += 1;
                    if idx >= argv.len() {
                        return Err(ApplyPatchError::IoError(IoError {
                            context: "begin_patch missing value for --preset".to_string(),
                            source: std::io::Error::other("missing value"),
                        }));
                    }
                    apply_command.push(other.to_string());
                    apply_command.push(argv[idx].clone());
                } else if other.starts_with("-f") && other.len() > 2 {
                    patch_file = Some(PathBuf::from(&other[2..]));
                    apply_command.push(format!("-f{}", &other[2..]));
                } else if other.starts_with("-C") && other.len() > 2 {
                    root_override = Some(PathBuf::from(&other[2..]));
                    apply_command.push(format!("-C{}", &other[2..]));
                } else {
                    apply_command.push(argv[idx].clone());
                }
            }
        }
        idx += 1;
    }

    let Some(patch_path) = patch_file else {
        return Ok(None);
    };

    let resolved_patch = if patch_path.is_absolute() {
        patch_path
    } else {
        cwd.join(patch_path)
    };

    let patch_contents = fs::read_to_string(&resolved_patch).map_err(|e| {
        ApplyPatchError::IoError(IoError {
            context: format!("Failed to read patch file {}", resolved_patch.display()),
            source: e,
        })
    })?;

    let mut parsed = parse_patch(&patch_contents)?;
    if let Some(root) = root_override {
        let effective_root = if root.is_absolute() {
            root
        } else {
            cwd.join(root)
        };
        parsed.workdir = Some(effective_root.to_string_lossy().into_owned());
    }

    Ok(Some((parsed, apply_command)))
}

fn build_apply_patch_action(
    args: ApplyPatchArgs,
    cwd: &Path,
    command: Option<Vec<String>>,
) -> Result<ApplyPatchAction, ApplyPatchError> {
    let ApplyPatchArgs {
        patch,
        hunks,
        workdir,
    } = args;

    let effective_cwd = workdir
        .as_ref()
        .map(|dir| {
            let path = Path::new(dir);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
        })
        .unwrap_or_else(|| cwd.to_path_buf());

    let mut changes = HashMap::new();
    for hunk in hunks {
        let path = hunk.resolve_path(&effective_cwd);
        match hunk {
            Hunk::AddFile { contents, .. } => {
                changes.insert(path, ApplyPatchFileChange::Add { content: contents });
            }
            Hunk::DeleteFile { .. } => {
                let content = fs::read_to_string(&path).map_err(|e| {
                    ApplyPatchError::IoError(IoError {
                        context: format!("Failed to read {}", path.display()),
                        source: e,
                    })
                })?;
                changes.insert(path, ApplyPatchFileChange::Delete { content });
            }
            Hunk::UpdateFile {
                move_path, chunks, ..
            } => {
                let ApplyPatchFileUpdate {
                    unified_diff,
                    content: contents,
                } = unified_diff_from_chunks(&path, &chunks)?;
                changes.insert(
                    path,
                    ApplyPatchFileChange::Update {
                        unified_diff,
                        move_path: move_path.map(|p| cwd.join(p)),
                        new_content: contents,
                    },
                );
            }
            Hunk::InsertBeforeSymbol {
                path: rel_path,
                symbol,
                new_lines,
            } => {
                let path = resolve_path(&effective_cwd, &rel_path)
                    .map_err(map_anyhow_to_apply_patch_error)?;
                update_symbol_change(
                    &mut changes,
                    path,
                    &symbol,
                    new_lines.as_slice(),
                    SymbolEditKind::InsertBefore,
                )?;
            }
            Hunk::InsertAfterSymbol {
                path: rel_path,
                symbol,
                new_lines,
            } => {
                let path = resolve_path(&effective_cwd, &rel_path)
                    .map_err(map_anyhow_to_apply_patch_error)?;
                update_symbol_change(
                    &mut changes,
                    path,
                    &symbol,
                    new_lines.as_slice(),
                    SymbolEditKind::InsertAfter,
                )?;
            }
            Hunk::ReplaceSymbolBody {
                path: rel_path,
                symbol,
                new_lines,
            } => {
                let path = resolve_path(&effective_cwd, &rel_path)
                    .map_err(map_anyhow_to_apply_patch_error)?;
                update_symbol_change(
                    &mut changes,
                    path,
                    &symbol,
                    new_lines.as_slice(),
                    SymbolEditKind::ReplaceBody,
                )?;
            }
        }
    }

    Ok(ApplyPatchAction {
        changes,
        patch,
        cwd: effective_cwd,
        command,
    })
}

#[derive(Debug, PartialEq)]
pub enum ExtractHeredocError {
    CommandDidNotStartWithApplyPatch,
    FailedToLoadBashGrammar(LanguageError),
    HeredocNotUtf8(Utf8Error),
    FailedToParsePatchIntoAst,
    FailedToFindHeredocBody,
}

pub fn apply_patch_with_config(
    patch: &str,
    config: &ApplyPatchConfig,
) -> Result<PatchReport, ApplyPatchError> {
    let args = parse_patch(patch)?;
    apply_hunks_with_config(&args.hunks, config)
}

/// Applies the patch and prints a begin_patch-style summary to stdout/stderr.
pub fn apply_patch(
    patch: &str,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    let args = match parse_patch(patch) {
        Ok(args) => args,
        Err(e) => {
            match &e {
                InvalidPatchError(message) => {
                    writeln!(stderr, "Invalid patch: {message}").map_err(ApplyPatchError::from)?;
                }
                InvalidHunkError {
                    message,
                    line_number,
                } => {
                    writeln!(
                        stderr,
                        "Invalid patch hunk on line {line_number}: {message}"
                    )
                    .map_err(ApplyPatchError::from)?;
                }
            }
            return Err(ApplyPatchError::ParseError(e));
        }
    };

    let config = ApplyPatchConfig::default();
    match apply_hunks_with_config(&args.hunks, &config) {
        Ok(report) => {
            emit_report(stdout, &report).map_err(ApplyPatchError::from)?;
            Ok(())
        }
        Err(err) => {
            writeln!(stderr, "{err}").map_err(ApplyPatchError::from)?;
            Err(err)
        }
    }
}

/// Applies hunks and continues to update stdout/stderr
pub fn apply_hunks(
    hunks: &[Hunk],
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<(), ApplyPatchError> {
    let config = ApplyPatchConfig::default();
    match apply_hunks_with_config(hunks, &config) {
        Ok(report) => {
            emit_report(stdout, &report).map_err(ApplyPatchError::from)?;
            Ok(())
        }
        Err(err) => {
            writeln!(stderr, "{err}").map_err(ApplyPatchError::from)?;
            Err(err)
        }
    }
}

#[derive(Debug)]
enum AppliedChange {
    Add {
        path: PathBuf,
    },
    Update {
        original: PathBuf,
        dest: PathBuf,
        backup_path: PathBuf,
    },
    Delete {
        original: PathBuf,
        backup_path: PathBuf,
    },
}

#[derive(Debug)]
struct PlannedChange {
    summary_index: usize,
    kind: PlannedChangeKind,
}

#[derive(Debug)]
enum PlannedChangeKind {
    Add {
        path: PathBuf,
        content: String,
        line_ending: LineEnding,
    },
    Update {
        original: PathBuf,
        dest: PathBuf,
        new_content: String,
        line_ending: LineEnding,
        metadata: FileMetadataSnapshot,
        reference_had_final_newline: bool,
    },
    Delete {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Planned,
    Applied,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAction {
    Add,
    Update,
    Move,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSummary {
    pub action: OperationAction,
    pub path: PathBuf,
    pub target_path: Option<PathBuf>,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub status: OperationStatus,
    pub message: Option<String>,
    pub symbol: Option<SymbolOperationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchReportMode {
    Apply,
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchReportStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchReport {
    pub mode: PatchReportMode,
    pub status: PatchReportStatus,
    pub duration_ms: u128,
    pub operations: Vec<OperationSummary>,
    pub options: ReportOptions,
    pub formatting: Vec<FormattingOutcome>,
    pub post_checks: Vec<PostCheckOutcome>,
    pub diagnostics: Vec<DiagnosticItem>,
    pub batch: Option<BatchSummary>,
    pub artifacts: ArtifactSummary,
    pub amendment_template: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ApplyPatchConfig {
    pub root: PathBuf,
    pub mode: PatchReportMode,
    pub encoding: String,
    pub normalization: NormalizationOptions,
    pub preserve_mode: bool,
    pub preserve_times: bool,
    pub new_file_mode: Option<u32>,
    pub symbol_fallback_mode: SymbolFallbackMode,
}

impl Default for ApplyPatchConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            mode: PatchReportMode::Apply,
            encoding: "utf-8".to_string(),
            normalization: NormalizationOptions::default(),
            preserve_mode: true,
            preserve_times: true,
            new_file_mode: None,
            symbol_fallback_mode: SymbolFallbackMode::default(),
        }
    }
}

impl OperationSummary {
    fn new(action: OperationAction, path: PathBuf) -> Self {
        Self {
            action,
            path,
            target_path: None,
            lines_added: 0,
            lines_removed: 0,
            status: OperationStatus::Applied,
            message: None,
            symbol: None,
        }
    }

    fn with_target(mut self, target: PathBuf) -> Self {
        self.target_path = Some(target);
        self
    }

    fn with_added(mut self, added: usize) -> Self {
        self.lines_added = added;
        self
    }

    fn with_removed(mut self, removed: usize) -> Self {
        self.lines_removed = removed;
        self
    }

    fn with_status(mut self, status: OperationStatus) -> Self {
        self.status = status;
        self
    }

    fn with_symbol(mut self, symbol: SymbolOperationSummary) -> Self {
        self.symbol = Some(symbol);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineMode {
    Preserve,
    Lf,
    Crlf,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineEnding {
    kind: LineEndingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEndingKind {
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    fn detected(input: &str) -> Option<Self> {
        if input.contains("\r\n") {
            return Some(Self {
                kind: LineEndingKind::Crlf,
            });
        }
        if input.contains('\r') {
            return Some(Self {
                kind: LineEndingKind::Cr,
            });
        }
        if input.contains('\n') {
            return Some(Self {
                kind: LineEndingKind::Lf,
            });
        }
        None
    }

    fn from_newline_mode(mode: NewlineMode) -> Self {
        match mode {
            NewlineMode::Lf => Self {
                kind: LineEndingKind::Lf,
            },
            NewlineMode::Crlf => Self {
                kind: LineEndingKind::Crlf,
            },
            NewlineMode::Native => Self::native(),
            NewlineMode::Preserve => Self::native(),
        }
    }

    fn native() -> Self {
        if cfg!(windows) {
            Self {
                kind: LineEndingKind::Crlf,
            }
        } else {
            Self {
                kind: LineEndingKind::Lf,
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self.kind {
            LineEndingKind::Lf => "\n",
            LineEndingKind::Crlf => "\r\n",
            LineEndingKind::Cr => "\r",
        }
    }
}

impl Default for LineEnding {
    fn default() -> Self {
        Self::native()
    }
}

#[derive(Debug, Clone)]
pub struct NormalizationOptions {
    pub newline: NewlineMode,
    pub strip_trailing_whitespace: bool,
    pub ensure_final_newline: Option<bool>,
}

impl Default for NormalizationOptions {
    fn default() -> Self {
        Self {
            newline: NewlineMode::Preserve,
            strip_trailing_whitespace: false,
            ensure_final_newline: None,
        }
    }
}

#[derive(Debug, Clone)]
struct FileMetadataSnapshot {
    permissions: Option<u32>,
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOptions {
    pub encoding: String,
    pub newline: NewlineMode,
    pub strip_trailing_whitespace: bool,
    pub ensure_final_newline: Option<bool>,
    pub preserve_mode: bool,
    pub preserve_times: bool,
    pub new_file_mode: Option<u32>,
    pub symbol_fallback_mode: SymbolFallbackMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Applied,
    Skipped,
    Failed,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Applied => "applied",
            TaskStatus::Skipped => "skipped",
            TaskStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagnosticItem {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    pub blocks: usize,
    pub applied: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDiagnostic {
    message: String,
    pub path: PathBuf,
    pub kind: ConflictKind,
    pub hunk_context: Option<String>,
    pub expected: Vec<String>,
    pub actual: Vec<String>,
    pub diff_hint: Vec<String>,
}

impl ConflictDiagnostic {
    fn new(
        message: String,
        path: PathBuf,
        kind: ConflictKind,
        hunk_context: Option<String>,
        expected: Vec<String>,
        actual: Vec<String>,
        diff_hint: Vec<String>,
    ) -> Self {
        Self {
            message,
            path,
            kind,
            hunk_context,
            expected,
            actual,
            diff_hint,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ConflictDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    ContextNotFound,
    UnexpectedContent,
    SymbolNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchExecutionError {
    pub message: String,
    pub report: PatchReport,
    pub conflict: Option<ConflictDiagnostic>,
}

impl fmt::Display for PatchExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PatchExecutionError {}

impl PatchExecutionError {
    pub fn conflict(&self) -> Option<&ConflictDiagnostic> {
        self.conflict.as_ref()
    }
}

impl ConflictDiagnostic {
    pub fn diff_hint(&self) -> &[String] {
        &self.diff_hint
    }
}

impl From<&ApplyPatchConfig> for ReportOptions {
    fn from(config: &ApplyPatchConfig) -> Self {
        Self {
            encoding: config.encoding.clone(),
            newline: config.normalization.newline,
            strip_trailing_whitespace: config.normalization.strip_trailing_whitespace,
            ensure_final_newline: config.normalization.ensure_final_newline,
            preserve_mode: config.preserve_mode,
            preserve_times: config.preserve_times,
            new_file_mode: config.new_file_mode,
            symbol_fallback_mode: config.symbol_fallback_mode,
        }
    }
}
impl NormalizationOptions {
    fn target_line_ending(&self, original: LineEnding) -> LineEnding {
        match self.newline {
            NewlineMode::Preserve => original,
            NewlineMode::Lf => LineEnding {
                kind: LineEndingKind::Lf,
            },
            NewlineMode::Crlf => LineEnding {
                kind: LineEndingKind::Crlf,
            },
            NewlineMode::Native => LineEnding::native(),
        }
    }
}

impl NewlineMode {
    fn as_str(self) -> &'static str {
        match self {
            NewlineMode::Preserve => "preserve",
            NewlineMode::Lf => "lf",
            NewlineMode::Crlf => "crlf",
            NewlineMode::Native => "native",
        }
    }
}

impl FileMetadataSnapshot {
    fn from_path(path: &Path) -> anyhow::Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?;
        #[cfg(unix)]
        let permissions = Some(std::os::unix::fs::PermissionsExt::mode(
            &metadata.permissions(),
        ));
        #[cfg(not(unix))]
        let permissions = None;
        let accessed = metadata.accessed().ok();
        let modified = metadata.modified().ok();
        Ok(Self {
            permissions,
            accessed,
            modified,
        })
    }
}

fn resolve_path(root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    if relative.is_absolute() {
        return Ok(relative.to_path_buf());
    }
    for component in relative.components() {
        if matches!(component, Component::ParentDir) {
            anyhow::bail!("Path {} may not contain '..' segments", relative.display());
        }
    }
    Ok(root.join(relative))
}

fn schedule_symbol_edit(
    relative_path: &PathBuf,
    symbol: &SymbolPath,
    new_lines: &[String],
    kind: SymbolEditKind,
    config: &ApplyPatchConfig,
    summaries: &mut Vec<OperationSummary>,
    planned: &mut Vec<PlannedChange>,
) -> anyhow::Result<()> {
    let absolute = resolve_path(&config.root, relative_path)?;
    if !absolute.exists() {
        anyhow::bail!(
            "Cannot edit {} because it does not exist",
            relative_path.display()
        );
    }

    let plan = plan_symbol_edit_for_path(&absolute, symbol, new_lines, kind, config)
        .map_err(anyhow::Error::new)?;

    let SymbolEditPlanResult {
        original_contents,
        new_contents,
        summary: symbol_summary,
    } = plan;

    let (added, removed) = diff_line_counts(&original_contents, &new_contents);
    let line_ending = LineEnding::detected(&original_contents).unwrap_or_else(LineEnding::native);
    let reference_had_final_newline = has_trailing_newline(&original_contents);
    let metadata = FileMetadataSnapshot::from_path(&absolute)?;

    let summary_index = summaries.len();
    let operation_summary = OperationSummary::new(OperationAction::Update, relative_path.clone())
        .with_added(added)
        .with_removed(removed)
        .with_status(OperationStatus::Planned)
        .with_symbol(symbol_summary);
    summaries.push(operation_summary);

    planned.push(PlannedChange {
        summary_index,
        kind: PlannedChangeKind::Update {
            original: absolute.clone(),
            dest: absolute,
            new_content: new_contents,
            line_ending,
            metadata,
            reference_had_final_newline,
        },
    });

    Ok(())
}

fn read_file_with_encoding(path: &Path, encoding: &str) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    decode_content(&bytes, encoding).map_err(anyhow::Error::new)
}

fn plan_symbol_edit_for_path(
    absolute: &Path,
    symbol: &SymbolPath,
    new_lines: &[String],
    kind: SymbolEditKind,
    config: &ApplyPatchConfig,
) -> Result<SymbolEditPlanResult, ApplyPatchError> {
    let original_contents = read_file_with_encoding(absolute, &config.encoding)
        .map_err(map_anyhow_to_apply_patch_error)?;
    let (new_contents, summary) = compute_symbol_edit(
        &original_contents,
        absolute,
        symbol,
        new_lines,
        kind,
        config.symbol_fallback_mode,
    )?;
    Ok(SymbolEditPlanResult {
        original_contents,
        new_contents,
        summary,
    })
}

fn compute_symbol_edit(
    original: &str,
    path: &Path,
    symbol: &SymbolPath,
    new_lines: &[String],
    kind: SymbolEditKind,
    fallback_mode: SymbolFallbackMode,
) -> Result<(String, SymbolOperationSummary), ApplyPatchError> {
    let context = locate_symbol_match(path, original, symbol, kind, fallback_mode, new_lines)?;
    let newline = LineEnding::detected(original)
        .unwrap_or_else(LineEnding::native)
        .as_str();
    let insertion_text = join_symbol_lines(new_lines, newline);

    let mut updated = String::with_capacity(original.len() + insertion_text.len());
    match kind {
        SymbolEditKind::InsertBefore => {
            let insert_idx = context.insert_before.min(original.len());
            updated.push_str(&original[..insert_idx]);
            updated.push_str(&insertion_text);
            updated.push_str(&original[insert_idx..]);
        }
        SymbolEditKind::InsertAfter => {
            let insert_idx = context.insert_after.min(original.len());
            updated.push_str(&original[..insert_idx]);
            updated.push_str(&insertion_text);
            updated.push_str(&original[insert_idx..]);
        }
        SymbolEditKind::ReplaceBody => {
            let body_range = context.body_range.clone().ok_or_else(|| {
                symbol_conflict_error(
                    path,
                    symbol,
                    new_lines,
                    kind,
                    "symbol body is unavailable for replacement",
                    None,
                    None,
                    context.reason.clone(),
                )
            })?;
            let start = body_range.start.min(original.len());
            let end = body_range.end.min(original.len());
            updated.push_str(&original[..start]);
            updated.push_str(&insertion_text);
            updated.push_str(&original[end..]);
        }
    }

    let summary = SymbolOperationSummary {
        kind,
        symbol: symbol.clone(),
        strategy: context.strategy,
        location: context.location.into(),
        reason: context.reason,
    };

    Ok((updated, summary))
}

fn locate_symbol_match(
    path: &Path,
    source: &str,
    symbol: &SymbolPath,
    kind: SymbolEditKind,
    fallback_mode: SymbolFallbackMode,
    new_lines: &[String],
) -> Result<SymbolMatchContext, ApplyPatchError> {
    let ast_failure = match resolve_locator(path) {
        Some(locator) => match locator.locate(source, symbol) {
            SymbolResolution::Match(target) => {
                return Ok(convert_ast_match(target, source));
            }
            SymbolResolution::Unsupported { reason } => Some(reason),
            SymbolResolution::NotFound { reason } => Some(reason),
        },
        None => Some("no symbol locator available for file".to_string()),
    };

    match fallback_mode {
        SymbolFallbackMode::Ast => {
            let reason =
                ast_failure.unwrap_or_else(|| "symbol locator did not find a match".to_string());
            return Err(symbol_conflict_error(
                path, symbol, new_lines, kind, &reason, None, None, None,
            ));
        }
        SymbolFallbackMode::Disabled => {
            return Err(symbol_conflict_error(
                path,
                symbol,
                new_lines,
                kind,
                "symbol fallback is disabled",
                None,
                None,
                None,
            ));
        }
        SymbolFallbackMode::Fuzzy => {}
    }

    match fallback::locate_symbol(source, symbol, fallback_mode) {
        Ok(fallback_match) => convert_fallback_match(
            source,
            fallback_match,
            path,
            symbol,
            kind,
            new_lines,
            ast_failure,
        ),
        Err(failure) => Err(symbol_conflict_error(
            path,
            symbol,
            new_lines,
            kind,
            &failure.reason,
            Some(&failure.excerpt),
            failure.location,
            ast_failure,
        )),
    }
}

fn convert_ast_match(target: SymbolTarget, source: &str) -> SymbolMatchContext {
    let body_range = target.body_range.clone();
    let insert_before = target.header_range.start;
    let insert_after = body_range
        .as_ref()
        .map(|r| r.end)
        .unwrap_or(target.header_range.end);
    let location = byte_range_to_line_col(target.header_range.clone(), source);
    SymbolMatchContext {
        body_range,
        insert_before,
        insert_after,
        strategy: SymbolFallbackStrategy::Ast,
        location,
        reason: Some(format!("matched via {} AST", target.language)),
    }
}

fn convert_fallback_match(
    source: &str,
    mut fallback_match: FallbackMatch,
    path: &Path,
    symbol: &SymbolPath,
    kind: SymbolEditKind,
    new_lines: &[String],
    ast_reason: Option<String>,
) -> Result<SymbolMatchContext, ApplyPatchError> {
    let line_start = find_line_start(source, fallback_match.match_index);
    let line_end = find_line_end(source, fallback_match.match_index);
    let mut insert_after = line_end.min(source.len());
    let mut body_range = None;

    // Try to detect block boundaries for insert-after / replace-body.
    if let Some(block) = detect_brace_block(source, line_end) {
        insert_after = block.end.min(source.len());
        body_range = Some(block);
    } else {
        let header_line = &source[line_start..line_end];
        if header_line.trim_end().ends_with(':') {
            let header_indent = count_indent(header_line);
            if let Some(block) = detect_indented_block(source, line_end, header_indent) {
                insert_after = block.end.min(source.len());
                body_range = Some(block);
            }
        }
    }

    if matches!(kind, SymbolEditKind::ReplaceBody) && body_range.is_none() {
        return Err(symbol_conflict_error(
            path,
            symbol,
            new_lines,
            kind,
            "unable to determine symbol body without AST match",
            Some(&fallback_match.excerpt),
            Some(fallback_match.location),
            Some(fallback_match.reason.clone()),
        ));
    }

    if let Some(ast) = ast_reason {
        if fallback_match.reason.is_empty() {
            fallback_match.reason = format!("AST locator reported: {ast}");
        } else {
            fallback_match.reason = format!("{}; AST locator: {ast}", fallback_match.reason);
        }
    }

    Ok(SymbolMatchContext {
        body_range,
        insert_before: line_start,
        insert_after,
        strategy: fallback_match.strategy,
        location: fallback_match.location,
        reason: Some(fallback_match.reason),
    })
}

fn join_symbol_lines(new_lines: &[String], newline: &str) -> String {
    if new_lines.is_empty() {
        return String::new();
    }
    let mut text = String::new();
    for line in new_lines {
        text.push_str(line);
        text.push_str(newline);
    }
    text
}

fn find_line_start(source: &str, idx: usize) -> usize {
    let mut cursor = idx.min(source.len());
    while cursor > 0 {
        let byte = source.as_bytes()[cursor - 1];
        if byte == b'\n' {
            break;
        }
        cursor -= 1;
    }
    cursor
}

fn find_line_end(source: &str, idx: usize) -> usize {
    let mut cursor = idx.min(source.len());
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        cursor += 1;
        if byte == b'\n' {
            break;
        }
    }
    cursor
}

fn detect_brace_block(source: &str, mut idx: usize) -> Option<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    while idx < bytes.len() {
        let ch = bytes[idx] as char;
        if ch == '{' {
            let start = idx;
            idx += 1;
            let mut depth = 1usize;
            while idx < bytes.len() {
                let ch = bytes[idx] as char;
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(start..(idx + 1));
                        }
                    }
                    '"' | '\'' => {
                        let quote = ch;
                        idx += 1;
                        while idx < bytes.len() {
                            let c = bytes[idx] as char;
                            if c == '\\' {
                                idx += 2;
                                continue;
                            }
                            if c == quote {
                                idx += 1;
                                break;
                            }
                            idx += 1;
                        }
                        continue;
                    }
                    _ => {}
                }
                idx += 1;
            }
            break;
        } else if ch == ';' {
            break;
        }
        idx += 1;
    }
    None
}

fn detect_indented_block(
    source: &str,
    mut idx: usize,
    header_indent: usize,
) -> Option<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    let mut start: Option<usize> = None;
    let mut end = idx;

    while idx < bytes.len() {
        let line_start = idx;
        while idx < bytes.len() && bytes[idx] != b'\n' {
            idx += 1;
        }
        let mut line_end = idx.min(bytes.len());
        if idx < bytes.len() {
            idx += 1;
            line_end = idx.min(bytes.len());
        }
        let line = &source[line_start..line_end];
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let content = trimmed.trim_start();
        if content.is_empty() {
            if start.is_some() {
                end = line_end;
            }
            continue;
        }
        let indent = count_indent(trimmed);
        if indent <= header_indent {
            break;
        }
        if start.is_none() {
            start = Some(line_start);
        }
        end = line_end;
    }

    start.map(|s| s..end)
}

fn count_indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

fn symbol_conflict_error(
    path: &Path,
    symbol: &SymbolPath,
    new_lines: &[String],
    kind: SymbolEditKind,
    reason: &str,
    excerpt: Option<&[String]>,
    location: Option<LineColumnRange>,
    fallback_reason: Option<String>,
) -> ApplyPatchError {
    let expected = new_lines.to_vec();
    let actual = excerpt
        .map(<[std::string::String]>::to_vec)
        .unwrap_or_default();
    let mut message = format!(
        "Failed to apply {} for symbol '{}' in {}",
        kind.as_str(),
        symbol,
        path.display()
    );
    if !reason.is_empty() {
        message.push_str(": ");
        message.push_str(reason);
    }
    if let Some(extra) = &fallback_reason
        && !extra.is_empty()
        && extra != reason
    {
        message.push_str(" — ");
        message.push_str(extra);
    }
    let hunk_context = location.map(|loc| format!("{}:{}", loc.start_line, symbol));
    let diagnostic = ConflictDiagnostic::new(
        message,
        path.to_path_buf(),
        ConflictKind::SymbolNotFound,
        hunk_context,
        expected,
        actual,
        Vec::new(),
    );
    ApplyPatchError::ComputeReplacements(Box::new(diagnostic))
}

fn normalize_content(
    content: &str,
    options: &NormalizationOptions,
    reference_line_ending: LineEnding,
    reference_had_final_newline: Option<bool>,
) -> String {
    let mut canonical = content.replace("\r\n", "\n").replace('\r', "\n");

    if options.strip_trailing_whitespace {
        canonical = canonical
            .split('\n')
            .map(|segment| segment.trim_end_matches([' ', '\t']).to_string())
            .collect::<Vec<_>>()
            .join("\n");
    }

    let target = options.target_line_ending(reference_line_ending);
    let newline_seq = target.as_str();

    let mut normalized = if newline_seq == "\n" {
        canonical
    } else {
        canonical.replace('\n', newline_seq)
    };

    match options.ensure_final_newline {
        Some(true) => {
            if !normalized.is_empty() && !normalized.ends_with(newline_seq) {
                normalized.push_str(newline_seq);
            }
        }
        Some(false) => {
            normalized = rstrip_terminal_newlines(normalized, newline_seq);
        }
        None => {
            let should_have_newline = match reference_had_final_newline {
                Some(value) => value,
                None => !normalized.is_empty(),
            };
            if should_have_newline {
                if !normalized.is_empty() && !normalized.ends_with(newline_seq) {
                    normalized.push_str(newline_seq);
                }
            } else {
                normalized = rstrip_terminal_newlines(normalized, newline_seq);
            }
        }
    }

    normalized
}

fn rstrip_terminal_newlines(mut text: String, newline_seq: &str) -> String {
    while !text.is_empty() && text.ends_with(newline_seq) {
        let new_len = text.len().saturating_sub(newline_seq.len());
        text.truncate(new_len);
    }
    text
}

fn has_trailing_newline(content: &str) -> bool {
    content.ends_with('\n') || content.ends_with('\r')
}

fn decode_content(bytes: &[u8], encoding: &str) -> Result<String, ApplyPatchError> {
    if encoding.eq_ignore_ascii_case("utf-8") {
        return String::from_utf8(bytes.to_vec())
            .map_err(|err| ApplyPatchError::EncodingError(err.to_string()));
    }

    let encoding = Encoding::for_label(encoding.as_bytes())
        .ok_or_else(|| ApplyPatchError::UnsupportedEncoding(encoding.to_string()))?;
    let (cow, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        return Err(ApplyPatchError::EncodingError(format!(
            "Failed to decode bytes using {}",
            encoding.name()
        )));
    }
    Ok(cow.into_owned())
}

fn encode_content(content: &str, encoding: &str) -> Result<Vec<u8>, ApplyPatchError> {
    if encoding.eq_ignore_ascii_case("utf-8") {
        return Ok(content.as_bytes().to_vec());
    }

    let encoding = Encoding::for_label(encoding.as_bytes())
        .ok_or_else(|| ApplyPatchError::UnsupportedEncoding(encoding.to_string()))?;
    let (cow, _, had_errors) = encoding.encode(content);
    if had_errors {
        return Err(ApplyPatchError::EncodingError(format!(
            "Failed to encode text using {}",
            encoding.name()
        )));
    }
    Ok(cow.into_owned())
}

fn apply_metadata(
    path: &Path,
    snapshot: &FileMetadataSnapshot,
    config: &ApplyPatchConfig,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        if config.preserve_mode
            && let Some(mode) = snapshot.permissions
        {
            let perms = std::fs::Permissions::from_mode(mode);
            fs::set_permissions(path, perms)
                .with_context(|| format!("Failed to set permissions for {}", path.display()))?;
        }
    }

    if config.preserve_times
        && let (Some(accessed), Some(modified)) = (snapshot.accessed, snapshot.modified)
    {
        let atime = FileTime::from_system_time(accessed);
        let mtime = FileTime::from_system_time(modified);
        filetime::set_file_times(path, atime, mtime)
            .with_context(|| format!("Failed to restore timestamps for {}", path.display()))?;
    }

    Ok(())
}

#[cfg(unix)]
fn apply_new_file_mode(path: &Path, mode: Option<u32>) -> anyhow::Result<()> {
    if let Some(mode) = mode {
        let perms = std::fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms)
            .with_context(|| format!("Failed to set permissions for {}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_new_file_mode(_path: &Path, _mode: Option<u32>) -> anyhow::Result<()> {
    Ok(())
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("Failed to create parent directories for {}", path.display()))?;
    let mut temp = TempFileBuilder::new()
        .prefix(".codex_apply_patch_new_")
        .tempfile_in(&parent)?;
    temp.as_file_mut()
        .write_all(bytes)
        .with_context(|| format!("Failed to write patch contents for {}", path.display()))?;
    temp.as_file_mut()
        .sync_all()
        .with_context(|| format!("Failed to flush patch contents for {}", path.display()))?;
    temp.persist(path)
        .with_context(|| format!("Failed to move patch into place for {}", path.display()))?;
    Ok(())
}

fn plan_hunks(
    hunks: &[Hunk],
    config: &ApplyPatchConfig,
) -> anyhow::Result<(Vec<OperationSummary>, Vec<PlannedChange>)> {
    let mut summaries = Vec::new();
    let mut planned = Vec::new();

    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let absolute = resolve_path(&config.root, path)?;
                let detected = LineEnding::detected(contents)
                    .unwrap_or_else(|| LineEnding::from_newline_mode(config.normalization.newline));
                let summary_index = summaries.len();

                if absolute.exists() {
                    let original = read_file_with_encoding(&absolute, &config.encoding)?;
                    let (added_lines, removed_lines) = diff_line_counts(&original, contents);
                    let summary = OperationSummary::new(OperationAction::Update, path.clone())
                        .with_added(added_lines)
                        .with_removed(removed_lines)
                        .with_status(OperationStatus::Planned);
                    summaries.push(summary);
                    let line_ending = LineEnding::detected(&original).unwrap_or(detected);
                    let reference_had_final_newline = has_trailing_newline(&original);
                    let metadata = FileMetadataSnapshot::from_path(&absolute)?;
                    planned.push(PlannedChange {
                        summary_index,
                        kind: PlannedChangeKind::Update {
                            original: absolute.clone(),
                            dest: absolute.clone(),
                            new_content: contents.clone(),
                            line_ending,
                            metadata,
                            reference_had_final_newline,
                        },
                    });
                } else {
                    let summary = OperationSummary::new(OperationAction::Add, path.clone())
                        .with_added(count_lines(contents))
                        .with_status(OperationStatus::Planned);
                    summaries.push(summary);
                    planned.push(PlannedChange {
                        summary_index,
                        kind: PlannedChangeKind::Add {
                            path: absolute,
                            content: contents.clone(),
                            line_ending: detected,
                        },
                    });
                }
            }
            Hunk::DeleteFile { path } => {
                let absolute = resolve_path(&config.root, path)?;
                if !absolute.exists() {
                    anyhow::bail!("Cannot delete {} because it does not exist", path.display());
                }
                let original = read_file_with_encoding(&absolute, &config.encoding)?;
                let summary = OperationSummary::new(OperationAction::Delete, path.clone())
                    .with_removed(count_lines(&original))
                    .with_status(OperationStatus::Planned);
                let summary_index = summaries.len();
                summaries.push(summary);
                planned.push(PlannedChange {
                    summary_index,
                    kind: PlannedChangeKind::Delete { path: absolute },
                });
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let absolute_src = resolve_path(&config.root, path)?;
                if !absolute_src.exists() {
                    anyhow::bail!("Cannot update {} because it does not exist", path.display());
                }

                let AppliedPatch {
                    original_contents,
                    new_contents,
                } = derive_new_contents_from_chunks_with_encoding(
                    &absolute_src,
                    chunks,
                    &config.encoding,
                )?;

                let reference_had_final_newline = has_trailing_newline(&original_contents);

                let dest_rel = move_path.clone().unwrap_or_else(|| path.clone());
                let absolute_dest = resolve_path(&config.root, &dest_rel)?;
                if absolute_dest != absolute_src && absolute_dest.exists() {
                    anyhow::bail!(
                        "Cannot move {} to {} because the destination already exists",
                        path.display(),
                        dest_rel.display()
                    );
                }

                let (added_lines, removed_lines) =
                    diff_line_counts(&original_contents, &new_contents);
                let action = if absolute_dest == absolute_src {
                    OperationAction::Update
                } else {
                    OperationAction::Move
                };
                let mut summary = OperationSummary::new(action, path.clone())
                    .with_added(added_lines)
                    .with_removed(removed_lines)
                    .with_status(OperationStatus::Planned);
                if absolute_dest != absolute_src {
                    summary = summary.with_target(dest_rel.clone());
                }
                let line_ending =
                    LineEnding::detected(&original_contents).unwrap_or_else(LineEnding::native);
                let summary_index = summaries.len();
                summaries.push(summary);
                let metadata = FileMetadataSnapshot::from_path(&absolute_src)?;
                planned.push(PlannedChange {
                    summary_index,
                    kind: PlannedChangeKind::Update {
                        original: absolute_src.clone(),
                        dest: absolute_dest,
                        new_content: new_contents,
                        line_ending,
                        metadata,
                        reference_had_final_newline,
                    },
                });
            }
            Hunk::InsertBeforeSymbol {
                path,
                symbol,
                new_lines,
            } => {
                schedule_symbol_edit(
                    path,
                    symbol,
                    new_lines,
                    SymbolEditKind::InsertBefore,
                    config,
                    &mut summaries,
                    &mut planned,
                )?;
            }
            Hunk::InsertAfterSymbol {
                path,
                symbol,
                new_lines,
            } => {
                schedule_symbol_edit(
                    path,
                    symbol,
                    new_lines,
                    SymbolEditKind::InsertAfter,
                    config,
                    &mut summaries,
                    &mut planned,
                )?;
            }
            Hunk::ReplaceSymbolBody {
                path,
                symbol,
                new_lines,
            } => {
                schedule_symbol_edit(
                    path,
                    symbol,
                    new_lines,
                    SymbolEditKind::ReplaceBody,
                    config,
                    &mut summaries,
                    &mut planned,
                )?;
            }
        }
    }

    Ok((summaries, planned))
}

fn apply_planned_changes(
    planned: &[PlannedChange],
    summaries: &mut [OperationSummary],
    config: &ApplyPatchConfig,
) -> anyhow::Result<()> {
    let mut applied: Vec<AppliedChange> = Vec::new();

    for change in planned {
        let summary = &mut summaries[change.summary_index];
        let result: anyhow::Result<()> = match &change.kind {
            PlannedChangeKind::Add {
                path,
                content,
                line_ending,
            } => {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("Failed to create parent directories for {}", path.display())
                    })?
                }
                let normalized =
                    normalize_content(content, &config.normalization, *line_ending, None);
                let encoded =
                    encode_content(&normalized, &config.encoding).map_err(anyhow::Error::new)?;
                write_atomic_bytes(path, &encoded)?;
                apply_new_file_mode(path, config.new_file_mode)?;
                applied.push(AppliedChange::Add { path: path.clone() });
                Ok(())
            }
            PlannedChangeKind::Update {
                original,
                dest,
                new_content,
                line_ending,
                metadata,
                reference_had_final_newline,
            } => {
                if let Some(parent) = dest.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("Failed to create parent directories for {}", dest.display())
                    })?
                }
                let backup_path = create_backup(original)?;
                applied.push(AppliedChange::Update {
                    original: original.clone(),
                    dest: dest.clone(),
                    backup_path,
                });
                let normalized = normalize_content(
                    new_content,
                    &config.normalization,
                    *line_ending,
                    Some(*reference_had_final_newline),
                );
                let encoded =
                    encode_content(&normalized, &config.encoding).map_err(anyhow::Error::new)?;
                write_atomic_bytes(dest, &encoded)?;
                apply_metadata(dest, metadata, config)?;
                Ok(())
            }
            PlannedChangeKind::Delete { path } => {
                let backup_path = create_backup(path)?;
                applied.push(AppliedChange::Delete {
                    original: path.clone(),
                    backup_path,
                });
                Ok(())
            }
        };

        match result {
            Ok(()) => {
                summary.status = OperationStatus::Applied;
            }
            Err(err) => {
                summary.status = OperationStatus::Failed;
                summary.message = Some(err.to_string());
                rollback(applied);
                return Err(err);
            }
        }
    }

    cleanup_backups(&applied);
    Ok(())
}

fn collect_unapplied_entries(
    planned: &[PlannedChange],
    summaries: &[OperationSummary],
) -> Vec<UnappliedEntry> {
    let mut entries = Vec::new();
    for change in planned {
        let summary = &summaries[change.summary_index];
        if summary.status == OperationStatus::Applied {
            continue;
        }

        match &change.kind {
            PlannedChangeKind::Add { path, content, .. } => {
                entries.push(UnappliedEntry {
                    path: path.clone(),
                    kind: UnappliedKind::Add,
                    contents: content.clone(),
                });
            }
            PlannedChangeKind::Update {
                dest, new_content, ..
            } => {
                entries.push(UnappliedEntry {
                    path: dest.clone(),
                    kind: UnappliedKind::Update,
                    contents: new_content.clone(),
                });
            }
            PlannedChangeKind::Delete { path } => match fs::read(path) {
                Ok(bytes) => entries.push(UnappliedEntry {
                    path: path.clone(),
                    kind: UnappliedKind::Delete,
                    contents: String::from_utf8_lossy(&bytes).into_owned(),
                }),
                Err(_) => {}
            },
        }
    }

    entries
}

fn summarize_conflict(conflict: &ConflictDiagnostic) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "Conflict in {}: {}",
        conflict.path.display(),
        conflict.message()
    ));
    if let Some(context) = conflict.hunk_context.as_ref() {
        parts.push(format!("Context: {context}"));
    }
    if !conflict.diff_hint().is_empty() {
        parts.push("Diff hint:".to_string());
        parts.extend(conflict.diff_hint().iter().cloned());
    }
    match conflict.kind {
        ConflictKind::ContextNotFound => parts.push(
            "Hint: обнови патч по актуальному контексту (перечитай файл и перестрой @@ блок)."
                .to_string(),
        ),
        ConflictKind::UnexpectedContent => parts.push(
            "Hint: файл изменён иначе, чем ожидалось — синхронизируй изменения и обнови hunks."
                .to_string(),
        ),
        ConflictKind::SymbolNotFound => parts.push(
            "Hint: символ не найден — проверь правильность пути и имени, либо добавь его в файл."
                .to_string(),
        ),
    }
    parts.join("\n")
}

fn map_anyhow_to_apply_patch_error(err: anyhow::Error) -> ApplyPatchError {
    match err.downcast::<ApplyPatchError>() {
        Ok(patch_err) => patch_err,
        Err(err) => match err.downcast::<std::io::Error>() {
            Ok(io_err) => {
                let context = io_err.to_string();
                ApplyPatchError::IoError(IoError {
                    context,
                    source: io_err,
                })
            }
            Err(other) => {
                let message = other.to_string();
                ApplyPatchError::IoError(IoError {
                    context: message.clone(),
                    source: std::io::Error::other(message),
                })
            }
        },
    }
}

fn apply_hunks_with_config(
    hunks: &[Hunk],
    config: &ApplyPatchConfig,
) -> Result<PatchReport, ApplyPatchError> {
    if hunks.is_empty() {
        return Err(ApplyPatchError::IoError(IoError {
            context: "No files were modified.".to_string(),
            source: std::io::Error::other("No files were modified."),
        }));
    }

    let start_time = Instant::now();
    let (mut summaries, planned) = match plan_hunks(hunks, config) {
        Ok(value) => value,
        Err(err) => {
            let apply_err = map_anyhow_to_apply_patch_error(err);
            if let Some(conflict) = apply_err.conflict().cloned() {
                let message = apply_err.to_string();
                let mut report = PatchReport {
                    mode: config.mode,
                    status: PatchReportStatus::Failed,
                    duration_ms: 0,
                    operations: Vec::new(),
                    options: ReportOptions::from(config),
                    formatting: Vec::new(),
                    post_checks: Vec::new(),
                    diagnostics: Vec::new(),
                    batch: None,
                    artifacts: ArtifactSummary::default(),
                    amendment_template: None,
                    errors: vec![message.clone()],
                };
                report.diagnostics.push(DiagnosticItem {
                    code: "conflict".to_string(),
                    message: summarize_conflict(&conflict),
                });
                return Err(ApplyPatchError::Execution(Box::new(PatchExecutionError {
                    message,
                    report,
                    conflict: Some(conflict),
                })));
            } else {
                return Err(apply_err);
            }
        }
    };

    if matches!(config.mode, PatchReportMode::Apply)
        && let Err(err) = apply_planned_changes(&planned, &mut summaries, config)
            .map_err(map_anyhow_to_apply_patch_error)
    {
        let duration_ms = start_time.elapsed().as_millis();
        let message = err.to_string();
        let entries = collect_unapplied_entries(&planned, &summaries);
        let mut report = PatchReport {
            mode: config.mode,
            status: PatchReportStatus::Failed,
            duration_ms,
            operations: summaries.clone(),
            options: ReportOptions::from(config),
            formatting: Vec::new(),
            post_checks: Vec::new(),
            diagnostics: Vec::new(),
            batch: None,
            artifacts: ArtifactSummary::default(),
            amendment_template: None,
            errors: vec![message.clone()],
        };
        if let Some(conflict) = err.conflict() {
            report.diagnostics.push(DiagnosticItem {
                code: "conflict".to_string(),
                message: summarize_conflict(conflict),
            });
        }
        for message in summarize_unapplied(&entries) {
            report.diagnostics.push(DiagnosticItem {
                code: "unapplied".to_string(),
                message,
            });
        }
        return Err(ApplyPatchError::Execution(Box::new(PatchExecutionError {
            message,
            report,
            conflict: err.conflict().cloned(),
        })));
    }

    let duration_ms = if matches!(config.mode, PatchReportMode::Apply) {
        start_time.elapsed().as_millis()
    } else {
        0
    };

    let mut report = PatchReport {
        mode: config.mode,
        status: PatchReportStatus::Success,
        duration_ms,
        operations: summaries,
        options: ReportOptions::from(config),
        formatting: Vec::new(),
        post_checks: Vec::new(),
        diagnostics: Vec::new(),
        batch: None,
        artifacts: ArtifactSummary::default(),
        amendment_template: None,
        errors: Vec::new(),
    };

    if matches!(report.mode, PatchReportMode::Apply)
        && matches!(report.status, PatchReportStatus::Success)
    {
        report.formatting = formatting::run_auto_formatters(&config.root, &report.operations);
        if let Err(err) = stage_git_changes(&config.root, &report.operations) {
            eprintln!("Warning: failed to stage changes in git: {err}");
        }
        report.post_checks = post_checks::run_post_checks(&config.root, &report.operations);

        if report
            .formatting
            .iter()
            .any(|outcome| outcome.status == TaskStatus::Failed)
        {
            report.diagnostics.push(DiagnosticItem {
                code: "formatting_failed".to_string(),
                message: "One or more formatters exited with an error".to_string(),
            });
        } else if report
            .formatting
            .iter()
            .any(|outcome| outcome.status == TaskStatus::Skipped)
        {
            report.diagnostics.push(DiagnosticItem {
                code: "formatting_skipped".to_string(),
                message: "Some formatters were skipped because the tool was unavailable"
                    .to_string(),
            });
        }

        if report
            .post_checks
            .iter()
            .any(|outcome| outcome.status == TaskStatus::Failed)
        {
            report.diagnostics.push(DiagnosticItem {
                code: "post_checks_failed".to_string(),
                message: "Post-check command reported a failure".to_string(),
            });
        } else if report
            .post_checks
            .iter()
            .any(|outcome| outcome.status == TaskStatus::Skipped)
        {
            report.diagnostics.push(DiagnosticItem {
                code: "post_checks_skipped".to_string(),
                message: "Some post-checks were skipped because the command was unavailable"
                    .to_string(),
            });
        }
    }

    Ok(report)
}

fn stage_git_changes(root: &Path, operations: &[OperationSummary]) -> std::io::Result<()> {
    let root = match root.canonicalize() {
        Ok(path) => path,
        Err(_) => root.to_path_buf(),
    };

    if !is_git_repository(&root) {
        return Ok(());
    }

    let mut paths = HashSet::new();
    for op in operations {
        if op.status != OperationStatus::Applied {
            continue;
        }

        if let Some(rel) = relative_to_root(&root, &op.path) {
            paths.insert(rel);
        }
        if let Some(target) = op.target_path.as_ref()
            && let Some(rel) = relative_to_root(&root, target)
        {
            paths.insert(rel);
        }
    }

    if paths.is_empty() {
        return Ok(());
    }

    let mut sorted_paths: Vec<_> = paths.into_iter().collect();
    sorted_paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));

    let mut cmd = Command::new("git");
    cmd.current_dir(&root);
    cmd.arg("add").arg("--all").arg("--");
    for path in &sorted_paths {
        cmd.arg(path);
    }
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(std::io::Error::other(format!("git add failed: {stderr}")))
    }
}

fn is_git_repository(root: &Path) -> bool {
    let mut cmd = Command::new("git");
    cmd.current_dir(root);
    cmd.arg("rev-parse").arg("--is-inside-work-tree");
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd.status().map(|status| status.success()).unwrap_or(false)
}

fn relative_to_root(root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        path.strip_prefix(root).map(PathBuf::from).ok()
    } else {
        Some(path.to_path_buf())
    }
}

fn rollback(applied: Vec<AppliedChange>) {
    for change in applied.into_iter().rev() {
        match change {
            AppliedChange::Add { path } => {
                let _ = fs::remove_file(&path);
            }
            AppliedChange::Update {
                original,
                dest,
                backup_path,
            } => {
                let _ = fs::remove_file(&dest);
                let _ = fs::rename(&backup_path, &original);
            }
            AppliedChange::Delete {
                original,
                backup_path,
            } => {
                let _ = fs::rename(&backup_path, &original);
            }
        }
    }
}

fn cleanup_backups(applied: &[AppliedChange]) {
    for change in applied {
        match change {
            AppliedChange::Update { backup_path, .. }
            | AppliedChange::Delete { backup_path, .. } => {
                let _ = fs::remove_file(backup_path);
            }
            AppliedChange::Add { .. } => {}
        }
    }
}

fn create_backup(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut attempt: u32 = 0;
    loop {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let candidate = parent.join(format!(
            ".codex_apply_patch_backup_{}_{}_{}",
            process::id(),
            timestamp,
            attempt,
        ));
        attempt = attempt.saturating_add(1);
        if candidate.exists() {
            continue;
        }
        std::fs::rename(path, &candidate).with_context(|| {
            format!(
                "Failed to rename {} to {}",
                path.display(),
                candidate.display()
            )
        })?;
        return Ok(candidate);
    }
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count()
    }
}

fn diff_line_counts(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

fn print_operations_summary(
    label: &str,
    operations: &[OperationSummary],
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "{label}:")?;
    for op in operations {
        let added = op.lines_added;
        let removed = op.lines_removed;
        if let Some(symbol) = op.symbol.as_ref() {
            writeln!(
                out,
                "- symbol({}): {} :: {} (+{}, -{}) [{}]",
                symbol.kind.as_str(),
                op.path.display(),
                symbol.symbol,
                added,
                removed,
                symbol.strategy.as_str()
            )?;
            if let Some(reason) = &symbol.reason
                && !reason.is_empty()
            {
                writeln!(out, "  -> {reason}")?;
            }
            continue;
        }
        match op.action {
            OperationAction::Add => {
                writeln!(out, "- add: {} (+{})", op.path.display(), added)?;
            }
            OperationAction::Update => {
                writeln!(
                    out,
                    "- update: {} (+{}, -{})",
                    op.path.display(),
                    added,
                    removed
                )?;
            }
            OperationAction::Move => {
                if let Some(target) = op.target_path.as_ref() {
                    writeln!(
                        out,
                        "- move: {} -> {} (+{}, -{})",
                        op.path.display(),
                        target.display(),
                        added,
                        removed
                    )?;
                }
            }
            OperationAction::Delete => {
                writeln!(out, "- delete: {} (-{})", op.path.display(), removed)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn emit_report(
    stdout: &mut impl std::io::Write,
    report: &PatchReport,
) -> std::io::Result<()> {
    let label = match (report.mode, report.status) {
        (PatchReportMode::Apply, PatchReportStatus::Success) => "Applied operations",
        (PatchReportMode::Apply, PatchReportStatus::Failed) => "Attempted operations",
        (PatchReportMode::DryRun, _) => "Planned operations",
    };
    print_operations_summary(label, &report.operations, stdout)?;
    match report.mode {
        PatchReportMode::Apply => match report.status {
            PatchReportStatus::Success => writeln!(stdout, "✔ Patch applied successfully."),
            PatchReportStatus::Failed => writeln!(stdout, "✘ Patch apply failed."),
        },
        PatchReportMode::DryRun => match report.status {
            PatchReportStatus::Success => {
                writeln!(stdout, "✔ Dry run successful. No changes applied.")
            }
            PatchReportStatus::Failed => writeln!(stdout, "✘ Dry run failed."),
        },
    }
}

pub(crate) fn report_to_json(report: &PatchReport) -> serde_json::Value {
    let operations = report
        .operations
        .iter()
        .map(|op| {
            let symbol = op.symbol.as_ref().map(|info| {
                json!({
                    "kind": info.kind.as_str(),
                    "symbol": info.symbol.to_string(),
                    "strategy": info.strategy.as_str(),
                    "location": {
                        "start_line": info.location.start_line,
                        "start_col": info.location.start_col,
                        "end_line": info.location.end_line,
                        "end_col": info.location.end_col,
                    },
                    "reason": info.reason.clone(),
                })
            });
            json!({
                "action": match op.action {
                    OperationAction::Add => "add",
                    OperationAction::Update => "update",
                    OperationAction::Move => "move",
                    OperationAction::Delete => "delete",
                },
                "path": op.path.display().to_string(),
                "renamed_to": op
                    .target_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                "added": op.lines_added,
                "removed": op.lines_removed,
                "status": match op.status {
                    OperationStatus::Planned => "planned",
                    OperationStatus::Applied => "applied",
                    OperationStatus::Failed => "failed",
                },
                "message": op.message.clone(),
                "symbol": symbol,
            })
        })
        .collect::<Vec<_>>();

    let formatting = report
        .formatting
        .iter()
        .map(|item| {
            json!({
                "tool": item.tool,
                "scope": item.scope,
                "status": item.status.as_str(),
                "duration_ms": item.duration_ms,
                "files": item
                    .files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
                "note": item.note,
            })
        })
        .collect::<Vec<_>>();

    let post_checks = report
        .post_checks
        .iter()
        .map(|item| {
            json!({
                "name": item.name,
                "command": item.command,
                "cwd": item.cwd.display().to_string(),
                "status": item.status.as_str(),
                "duration_ms": item.duration_ms,
                "note": item.note,
                "stdout": item.stdout,
                "stderr": item.stderr,
            })
        })
        .collect::<Vec<_>>();

    let diagnostics = report
        .diagnostics
        .iter()
        .map(|item| json!({ "code": item.code, "message": item.message }))
        .collect::<Vec<_>>();

    let artifacts = json!({
        "log": report
            .artifacts
            .log
            .as_ref()
            .map(|path| path.display().to_string()),
        "conflicts": report
            .artifacts
            .conflicts
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "unapplied": report
            .artifacts
            .unapplied
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    });

    let batch = report.batch.as_ref().map(|batch| {
        json!({
            "blocks": batch.blocks,
            "applied": batch.applied,
            "failed": batch.failed,
        })
    });

    json!({
        "status": match report.status {
            PatchReportStatus::Success => "success",
            PatchReportStatus::Failed => "failed",
        },
        "mode": match report.mode {
            PatchReportMode::Apply => "apply",
            PatchReportMode::DryRun => "dry-run",
        },
        "duration_ms": report.duration_ms,
        "operations": operations,
        "errors": report.errors,
        "options": {
            "encoding": report.options.encoding,
            "newline": report.options.newline.as_str(),
            "strip_trailing_whitespace": report.options.strip_trailing_whitespace,
            "ensure_final_newline": report.options.ensure_final_newline,
            "preserve_mode": report.options.preserve_mode,
            "preserve_times": report.options.preserve_times,
            "new_file_mode": report.options.new_file_mode,
            "symbol_fallback_mode": report.options.symbol_fallback_mode.as_str(),
        },
        "formatting": formatting,
        "post_checks": post_checks,
        "diagnostics": diagnostics,
        "batch": batch,
        "artifacts": artifacts,
        "amendment_template": report.amendment_template,
    })
}

pub(crate) fn report_to_machine_json(report: &PatchReport) -> serde_json::Value {
    json!({
        "schema": APPLY_PATCH_MACHINE_SCHEMA,
        "report": report_to_json(report),
    })
}

/// Return *only* the new file contents (joined into a single `String`) after
/// applying the chunks to the file at `path`.
struct AppliedPatch {
    original_contents: String,
    new_contents: String,
}

fn derive_new_contents_from_chunks(
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    derive_new_contents_from_chunks_with_encoding(path, chunks, "utf-8")
}

fn derive_new_contents_from_chunks_with_encoding(
    path: &Path,
    chunks: &[UpdateFileChunk],
    encoding: &str,
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let bytes = fs::read(path).map_err(|err| {
        ApplyPatchError::IoError(IoError {
            context: format!("Failed to read file to update {}", path.display()),
            source: err,
        })
    })?;
    let original_contents = decode_content(&bytes, encoding)?;
    derive_new_contents_from_original_contents(path, chunks, original_contents)
}

fn derive_new_contents_from_original_contents(
    path: &Path,
    chunks: &[UpdateFileChunk],
    original_contents: String,
) -> std::result::Result<AppliedPatch, ApplyPatchError> {
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();

    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }

    let separator = if original_contents.contains("\r\n") {
        "\r\n"
    } else if original_contents.contains('\r') && !original_contents.contains('\n') {
        "\r"
    } else {
        "\n"
    };

    let new_contents = if new_lines.is_empty() {
        String::new()
    } else {
        new_lines.join(separator)
    };

    Ok(AppliedPatch {
        original_contents,
        new_contents,
    })
}

/// Compute a list of replacements needed to transform `original_lines` into the
/// new lines, given the patch `chunks`. Each replacement is returned as
/// `(start_index, old_len, new_lines)`.
fn collect_actual_slice(original_lines: &[String], start: usize, len: usize) -> Vec<String> {
    original_lines
        .iter()
        .skip(start)
        .take(len)
        .cloned()
        .collect()
}

fn make_diff_hint(expected: &[String], actual: &[String]) -> Vec<String> {
    if expected.is_empty() && actual.is_empty() {
        return Vec::new();
    }
    let expected_text = expected.join(
        "
",
    );
    let actual_text = actual.join(
        "
",
    );
    let diff = TextDiff::from_lines(&actual_text, &expected_text);
    diff.unified_diff()
        .context_radius(2)
        .header("current", "expected")
        .to_string()
        .lines()
        .map(std::string::ToString::to_string)
        .collect()
}

fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        // If a chunk has a `change_context`, we use seek_sequence to find it, then
        // adjust our `line_index` to continue from there.
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence::seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
                line_index = idx + 1;
            } else {
                let actual = collect_actual_slice(original_lines, line_index, 1);
                let expected = vec![ctx_line.clone()];
                let diff_hint = make_diff_hint(&expected, &actual);
                let diagnostic = ConflictDiagnostic::new(
                    format!(
                        "Failed to find context '{}' in {}",
                        ctx_line,
                        path.display()
                    ),
                    path.to_path_buf(),
                    ConflictKind::ContextNotFound,
                    Some(ctx_line.clone()),
                    expected,
                    actual,
                    diff_hint,
                );
                return Err(ApplyPatchError::ComputeReplacements(Box::new(diagnostic)));
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure addition (no old lines). We'll add them at the end or just
            // before the final empty line if one exists.
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        // Otherwise, try to match the existing lines in the file with the old lines
        // from the chunk. If found, schedule that region for replacement.
        // Attempt to locate the `old_lines` verbatim within the file.  In many
        // real‑world diffs the last element of `old_lines` is an *empty* string
        // representing the terminating newline of the region being replaced.
        // This sentinel is not present in `original_lines` because we strip the
        // trailing empty slice emitted by `split('\n')`.  If a direct search
        // fails and the pattern ends with an empty string, retry without that
        // final element so that modifications touching the end‑of‑file can be
        // located reliably.

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found =
            seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            // Retry without the trailing empty line which represents the final
            // newline in the file.
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }

            found = seek_sequence::seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
            );
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            let actual = collect_actual_slice(original_lines, line_index, chunk.old_lines.len());
            let diff_hint = make_diff_hint(&chunk.old_lines, &actual);
            let diagnostic = ConflictDiagnostic::new(
                format!(
                    "Failed to find expected lines in {}:\n{}",
                    path.display(),
                    chunk.old_lines.join("\n"),
                ),
                path.to_path_buf(),
                ConflictKind::UnexpectedContent,
                chunk.change_context.clone(),
                chunk.old_lines.clone(),
                actual,
                diff_hint,
            );
            return Err(ApplyPatchError::ComputeReplacements(Box::new(diagnostic)));
        }
    }

    replacements.sort_by(|(lhs_idx, _, _), (rhs_idx, _, _)| lhs_idx.cmp(rhs_idx));

    Ok(replacements)
}

/// Apply the `(start_index, old_len, new_lines)` replacements to `original_lines`,
/// returning the modified file contents as a vector of lines.
fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    // We must apply replacements in descending order so that earlier replacements
    // don't shift the positions of later ones.
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        // Remove old lines.
        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        // Insert new lines.
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

/// Intended result of a file update for apply_patch.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplyPatchFileUpdate {
    unified_diff: String,
    content: String,
}

pub fn unified_diff_from_chunks(
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    unified_diff_from_chunks_with_context(path, chunks, 1)
}

pub fn unified_diff_from_chunks_with_context(
    path: &Path,
    chunks: &[UpdateFileChunk],
    context: usize,
) -> std::result::Result<ApplyPatchFileUpdate, ApplyPatchError> {
    let AppliedPatch {
        original_contents,
        new_contents,
    } = derive_new_contents_from_chunks(path, chunks)?;
    let text_diff = TextDiff::from_lines(&original_contents, &new_contents);
    let unified_diff = text_diff.unified_diff().context_radius(context).to_string();
    Ok(ApplyPatchFileUpdate {
        unified_diff,
        content: new_contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::process::Command;
    use std::process::Stdio;
    use std::string::ToString;
    use tempfile::tempdir;

    /// Helper to construct a patch with the given body.
    fn wrap_patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    fn strs_to_strings(strs: &[&str]) -> Vec<String> {
        strs.iter().map(ToString::to_string).collect()
    }

    // Test helpers to reduce repetition when building bash -lc heredoc scripts
    fn args_bash(script: &str) -> Vec<String> {
        strs_to_strings(&["bash", "-lc", script])
    }

    fn heredoc_script(prefix: &str) -> String {
        format!(
            "{prefix}apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH"
        )
    }

    fn heredoc_script_ps(prefix: &str, suffix: &str) -> String {
        format!(
            "{prefix}apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH{suffix}"
        )
    }

    fn init_git(dir: &Path) -> bool {
        Command::new("git")
            .arg("init")
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn git_add_all(dir: &Path) -> bool {
        Command::new("git")
            .args(["add", "--all"])
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn git_commit_all(dir: &Path, message: &str) -> bool {
        Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg(message)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Codex")
            .env("GIT_AUTHOR_EMAIL", "codex@example.com")
            .env("GIT_COMMITTER_NAME", "Codex")
            .env("GIT_COMMITTER_EMAIL", "codex@example.com")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn git_status(dir: &Path) -> Option<String> {
        let output = Command::new("git")
            .args(["status", "--short"])
            .current_dir(dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn expected_single_add() -> Vec<Hunk> {
        vec![Hunk::AddFile {
            path: PathBuf::from("foo"),
            contents: "hi\n".to_string(),
        }]
    }

    fn assert_match(script: &str, expected_workdir: Option<&str>) {
        let args = args_bash(script);
        match maybe_parse_apply_patch(&args) {
            MaybeApplyPatch::Body(ApplyPatchArgs { hunks, workdir, .. }) => {
                assert_eq!(workdir.as_deref(), expected_workdir);
                assert_eq!(hunks, expected_single_add());
            }
            result => panic!("expected MaybeApplyPatch::Body got {result:?}"),
        }
    }

    fn assert_not_match(script: &str) {
        let args = args_bash(script);
        assert_matches!(
            maybe_parse_apply_patch(&args),
            MaybeApplyPatch::NotApplyPatch
        );
    }

    #[test]
    fn test_implicit_patch_single_arg_is_error() {
        let patch = "*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch".to_string();
        let args = vec![patch];
        let dir = tempdir().unwrap();
        assert_matches!(
            maybe_parse_apply_patch_verified(&args, dir.path()),
            MaybeApplyPatchVerified::CorrectnessError(ApplyPatchError::ImplicitInvocation)
        );
    }

    #[test]
    fn test_implicit_patch_bash_script_is_error() {
        let script = "*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch";
        let args = args_bash(script);
        let dir = tempdir().unwrap();
        assert_matches!(
            maybe_parse_apply_patch_verified(&args, dir.path()),
            MaybeApplyPatchVerified::CorrectnessError(ApplyPatchError::ImplicitInvocation)
        );
    }

    #[test]
    fn test_literal() {
        let args = strs_to_strings(&[
            "apply_patch",
            r#"*** Begin Patch
*** Add File: foo
+hi
*** End Patch
"#,
        ]);

        match maybe_parse_apply_patch(&args) {
            MaybeApplyPatch::Body(ApplyPatchArgs { hunks, .. }) => {
                assert_eq!(
                    hunks,
                    vec![Hunk::AddFile {
                        path: PathBuf::from("foo"),
                        contents: "hi\n".to_string()
                    }]
                );
            }
            result => panic!("expected MaybeApplyPatch::Body got {result:?}"),
        }
    }

    #[test]
    fn test_literal_applypatch() {
        let args = strs_to_strings(&[
            "applypatch",
            r#"*** Begin Patch
*** Add File: foo
+hi
*** End Patch
"#,
        ]);

        match maybe_parse_apply_patch(&args) {
            MaybeApplyPatch::Body(ApplyPatchArgs { hunks, .. }) => {
                assert_eq!(
                    hunks,
                    vec![Hunk::AddFile {
                        path: PathBuf::from("foo"),
                        contents: "hi\n".to_string()
                    }]
                );
            }
            result => panic!("expected MaybeApplyPatch::Body got {result:?}"),
        }
    }

    #[test]
    fn test_heredoc() {
        assert_match(&heredoc_script(""), None);
    }

    #[test]
    fn test_heredoc_applypatch() {
        let args = strs_to_strings(&[
            "bash",
            "-lc",
            r#"applypatch <<'PATCH'
*** Begin Patch
*** Add File: foo
+hi
*** End Patch
PATCH"#,
        ]);

        match maybe_parse_apply_patch(&args) {
            MaybeApplyPatch::Body(ApplyPatchArgs { hunks, workdir, .. }) => {
                assert_eq!(workdir, None);
                assert_eq!(
                    hunks,
                    vec![Hunk::AddFile {
                        path: PathBuf::from("foo"),
                        contents: "hi\n".to_string()
                    }]
                );
            }
            result => panic!("expected MaybeApplyPatch::Body got {result:?}"),
        }
    }

    #[test]
    fn test_heredoc_with_leading_cd() {
        assert_match(&heredoc_script("cd foo && "), Some("foo"));
    }

    #[test]
    fn test_cd_with_semicolon_matches() {
        assert_match(&heredoc_script("cd foo; "), Some("foo"));
    }

    #[test]
    fn test_cd_or_apply_patch_is_ignored() {
        assert_not_match(&heredoc_script("cd bar || "));
    }

    #[test]
    fn test_cd_pipe_apply_patch_is_ignored() {
        assert_not_match(&heredoc_script("cd bar | "));
    }

    #[test]
    fn test_cd_single_quoted_path_with_spaces() {
        assert_match(&heredoc_script("cd 'foo bar' && "), Some("foo bar"));
    }

    #[test]
    fn test_cd_double_quoted_path_with_spaces() {
        assert_match(&heredoc_script("cd \"foo bar\" && "), Some("foo bar"));
    }

    #[test]
    fn test_set_and_cd_inline_allowed() {
        assert_match(
            &heredoc_script("set -euo pipefail && cd repo && "),
            Some("repo"),
        );
    }

    #[test]
    fn test_cd_on_previous_line_matches() {
        assert_match(&heredoc_script("cd foo\n"), Some("foo"));
    }

    #[test]
    fn test_set_e_prefix_is_allowed() {
        assert_match(&heredoc_script("set -e\n"), None);
    }

    #[test]
    fn test_echo_and_apply_patch_is_ignored() {
        assert_not_match(&heredoc_script("echo foo && "));
    }

    #[test]
    fn test_apply_patch_with_arg_is_ignored() {
        let script = "apply_patch foo <<'PATCH'\n*** Begin Patch\n*** Add File: foo\n+hi\n*** End Patch\nPATCH";
        assert_not_match(script);
    }

    #[test]
    fn test_double_cd_then_apply_patch_uses_last_path() {
        assert_match(&heredoc_script("cd foo && cd bar && "), Some("bar"));
    }

    #[test]
    fn test_cd_two_args_is_ignored() {
        assert_not_match(&heredoc_script("cd foo bar && "));
    }

    #[test]
    fn test_cd_then_apply_patch_then_extra_is_ignored() {
        let script = heredoc_script_ps("cd bar && ", " && echo done");
        assert_not_match(&script);
    }

    #[test]
    fn test_echo_then_cd_and_apply_patch_is_ignored() {
        // Ensure preceding commands before the `cd && apply_patch <<...` sequence do not match.
        assert_not_match(&heredoc_script("echo foo; cd bar && "));
    }

    #[test]
    fn test_add_file_hunk_creates_file_with_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("add.txt");
        let patch = wrap_patch(&format!(
            r#"*** Add File: {}
+ab
+cd"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        // Verify expected stdout and stderr outputs.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Applied operations:\n- add: {} (+2)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents, "ab\ncd\n");
    }

    #[test]
    fn test_delete_file_hunk_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("del.txt");
        fs::write(&path, "x").unwrap();
        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Applied operations:\n- delete: {} (-1)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!path.exists());
    }

    #[test]
    fn test_update_file_hunk_modifies_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("update.txt");
        fs::write(&path, "foo\nbar\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+baz"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        // Validate modified file contents and expected stdout/stderr.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Applied operations:
- update: {} (+1, -1)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nbaz\n");
    }

    #[test]
    fn test_replace_symbol_body_updates_function() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        fs::write(&file, "fn greet() {\n    println!(\"hi\");\n}\n").unwrap();

        let patch = format!(
            "*** Begin Patch\n*** Replace Symbol Body: {}::greet\n+{{\n+    println!(\"hello\");\n+}}\n*** End Patch",
            file.display()
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();

        let updated = fs::read_to_string(&file).unwrap();
        assert_eq!(
            updated.trim_end(),
            "fn greet() {\n    println!(\"hello\");\n}"
        );

        let summary = String::from_utf8(stdout).unwrap();
        assert!(summary.contains("symbol(replace-body):"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_update_file_hunk_can_move_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("dst.txt");
        fs::write(&src, "line\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
*** Move to: {}
@@
-line
+line2"#,
            src.display(),
            dest.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        // Validate move semantics and expected stdout/stderr.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Applied operations:\n- move: {} -> {} (+1, -1)\n✔ Patch applied successfully.\n",
            src.display(),
            dest.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!src.exists());
        let contents = fs::read_to_string(&dest).unwrap();
        assert_eq!(contents, "line2\n");
    }

    /// Verify that a single `Update File` hunk with multiple change chunks can update different
    /// parts of a file and that the file is listed only once in the summary.
    #[test]
    fn test_multiple_update_chunks_apply_to_single_file() {
        // Start with a file containing four lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "foo\nbar\nbaz\nqux\n").unwrap();
        // Construct an update patch with two separate change chunks.
        // The first chunk uses the line `foo` as context and transforms `bar` into `BAR`.
        // The second chunk uses `baz` as context and transforms `qux` into `QUX`.
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+BAR
@@
 baz
-qux
+QUX"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Applied operations:\n- update: {} (+2, -2)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nBAR\nbaz\nQUX\n");
    }

    #[test]
    fn test_apply_patch_multiple_file_operations_summary() {
        let dir = tempdir().unwrap();
        let update_path = dir.path().join("keep.txt");
        let add_path = dir.path().join("added.txt");
        let delete_path = dir.path().join("remove.txt");
        fs::write(
            &update_path,
            "stay
old
",
        )
        .unwrap();
        fs::write(
            &delete_path,
            "gone
",
        )
        .unwrap();

        let hunks = vec![
            Hunk::UpdateFile {
                path: update_path.clone(),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec!["old".to_string()],
                    new_lines: vec!["new".to_string()],
                    is_end_of_file: false,
                }],
            },
            Hunk::AddFile {
                path: add_path.clone(),
                contents: "created
"
                .to_string(),
            },
            Hunk::DeleteFile {
                path: delete_path.clone(),
            },
        ];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_hunks(&hunks, &mut stdout, &mut stderr).unwrap();

        let stdout_str = String::from_utf8(stdout).unwrap();
        let expected = format!(
            "Applied operations:
- update: {} (+1, -1)
- add: {} (+1)
- delete: {} (-1)
✔ Patch applied successfully.
",
            update_path.display(),
            add_path.display(),
            delete_path.display()
        );
        assert_eq!(stdout_str, expected);
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
        assert_eq!(
            fs::read_to_string(&update_path).unwrap(),
            "stay
new
"
        );
        assert_eq!(
            fs::read_to_string(&add_path).unwrap(),
            "created
"
        );
        assert!(!delete_path.exists());
    }

    /// A more involved `Update File` hunk that exercises additions, deletions and
    /// replacements in separate chunks that appear in non‑adjacent parts of the
    /// file.  Verifies that all edits are applied and that the summary lists the
    /// file only once.
    #[test]
    fn test_update_file_hunk_interleaved_changes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interleaved.txt");

        // Original file: six numbered lines.
        fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();

        // Patch performs:
        //  • Replace `b` → `B`
        //  • Replace `e` → `E` (using surrounding context)
        //  • Append new line `g` at the end‑of‑file
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 a
-b
+B
@@
 c
 d
-e
+E
@@
 f
+g
*** End of File"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        let expected_out = format!(
            "Applied operations:\n- update: {} (+3, -2)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "a\nB\nc\nd\nE\nf\ng\n");
    }

    #[test]
    fn test_pure_addition_chunk_followed_by_removal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("panic.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
+after-context
+second-line
@@
 line1
-line2
-line3
+line2-replacement"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(
            contents,
            "line1\nline2-replacement\nafter-context\nsecond-line\n"
        );
    }

    #[test]
    fn test_git_staging_after_apply() {
        let repo = tempdir().unwrap();
        if !init_git(repo.path()) {
            eprintln!("git not available, skipping staging test");
            return;
        }

        let patch = wrap_patch(
            "*** Add File: foo.txt
+hello",
        );

        let config = ApplyPatchConfig {
            root: repo.path().to_path_buf(),
            ..ApplyPatchConfig::default()
        };
        apply_patch_with_config(&patch, &config).expect("apply_patch_with_config");

        let summary = git_status(repo.path()).expect("git status");
        assert!(
            summary.contains("A  foo.txt"),
            "expected staged addition, git status: {summary:?}"
        );
    }

    #[test]
    fn test_git_staging_after_update() {
        let repo = tempdir().unwrap();
        if !init_git(repo.path()) {
            eprintln!("git not available, skipping staging test");
            return;
        }

        let path = repo.path().join("foo.txt");
        fs::write(&path, "hello\n").unwrap();
        assert!(git_add_all(repo.path()));
        assert!(git_commit_all(repo.path(), "init file"));

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-hello
+world"#,
            path.display()
        ));

        let config = ApplyPatchConfig {
            root: repo.path().to_path_buf(),
            ..ApplyPatchConfig::default()
        };
        apply_patch_with_config(&patch, &config).expect("apply_patch_with_config");

        let summary = git_status(repo.path()).expect("git status");
        assert!(
            summary.contains("M  foo.txt"),
            "expected staged modification, git status: {summary:?}"
        );
    }

    #[test]
    fn test_git_staging_after_delete() {
        let repo = tempdir().unwrap();
        if !init_git(repo.path()) {
            eprintln!("git not available, skipping staging test");
            return;
        }

        let path = repo.path().join("foo.txt");
        fs::write(&path, "hello\n").unwrap();
        assert!(git_add_all(repo.path()));
        assert!(git_commit_all(repo.path(), "init file"));

        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));

        let config = ApplyPatchConfig {
            root: repo.path().to_path_buf(),
            ..ApplyPatchConfig::default()
        };
        apply_patch_with_config(&patch, &config).expect("apply_patch_with_config");

        let summary = git_status(repo.path()).expect("git status");
        assert!(
            summary.contains("D  foo.txt"),
            "expected staged deletion, git status: {summary:?}"
        );
    }

    #[test]
    fn test_git_staging_after_move() {
        let repo = tempdir().unwrap();
        if !init_git(repo.path()) {
            eprintln!("git not available, skipping staging test");
            return;
        }

        let src = repo.path().join("src.txt");
        fs::write(&src, "hello\n").unwrap();
        assert!(git_add_all(repo.path()));
        assert!(git_commit_all(repo.path(), "init file"));

        let dest = repo.path().join("dst.txt");
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
*** Move to: {}
@@
-hello
+world"#,
            src.display(),
            dest.display()
        ));

        let config = ApplyPatchConfig {
            root: repo.path().to_path_buf(),
            ..ApplyPatchConfig::default()
        };
        apply_patch_with_config(&patch, &config).expect("apply_patch_with_config");

        let summary = git_status(repo.path()).expect("git status");
        let lines: Vec<&str> = summary.lines().collect();
        let rename_staged = lines
            .iter()
            .any(|line| line.trim().starts_with("R  ") && line.contains("src.txt -> dst.txt"));
        let add_delete_pair = lines.iter().any(|line| line.contains("A  dst.txt"))
            && lines.iter().any(|line| line.contains("D  src.txt"));
        assert!(
            rename_staged || add_delete_pair,
            "expected staged rename, git status: {summary:?}"
        );
    }

    /// Ensure that patches authored with ASCII characters can update lines that
    /// contain typographic Unicode punctuation (e.g. EN DASH, NON-BREAKING
    /// HYPHEN). Historically `git apply` succeeds in such scenarios but our
    /// internal matcher failed requiring an exact byte-for-byte match.  The
    /// fuzzy-matching pass that normalises common punctuation should now bridge
    /// the gap.
    #[test]
    fn test_update_line_with_unicode_dash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unicode.py");

        // Original line contains EN DASH (\u{2013}) and NON-BREAKING HYPHEN (\u{2011}).
        let original = "import asyncio  # local import \u{2013} avoids top\u{2011}level dep\n";
        std::fs::write(&path, original).unwrap();

        // Patch uses plain ASCII dash / hyphen.
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-import asyncio  # local import - avoids top-level dep
+import asyncio  # HELLO"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();

        // File should now contain the replaced comment.
        let expected = "import asyncio  # HELLO\n";
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, expected);

        // Ensure success summary lists the file as modified.
        let stdout_str = String::from_utf8(stdout).unwrap();
        let expected_out = format!(
            "Applied operations:\n- update: {} (+1, -1)\n✔ Patch applied successfully.\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);

        // No stderr expected.
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[test]
    fn test_unified_diff() {
        // Start with a file containing four lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "foo\nbar\nbaz\nqux\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+BAR
@@
 baz
-qux
+QUX"#,
            path.display()
        ));
        let patch = parse_patch(&patch).unwrap();

        let update_file_chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };
        let diff = unified_diff_from_chunks(&path, update_file_chunks).unwrap();
        let expected_diff = r#"@@ -1,4 +1,4 @@
 foo
-bar
+BAR
 baz
-qux
+QUX
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nBAR\nbaz\nQUX\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[test]
    fn test_unified_diff_first_line_replacement() {
        // Replace the very first line of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("first.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-foo
+FOO
 bar
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let diff = unified_diff_from_chunks(&path, chunks).unwrap();
        let expected_diff = r#"@@ -1,2 +1,2 @@
-foo
+FOO
 bar
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "FOO\nbar\nbaz\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[test]
    fn test_unified_diff_last_line_replacement() {
        // Replace the very last line of the file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("last.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
 bar
-baz
+BAZ
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let diff = unified_diff_from_chunks(&path, chunks).unwrap();
        let expected_diff = r#"@@ -2,2 +2,2 @@
 bar
-baz
+BAZ
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nbar\nBAZ\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[test]
    fn test_unified_diff_insert_at_eof() {
        // Insert a new line at end‑of‑file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("insert.txt");
        fs::write(&path, "foo\nbar\nbaz\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
+quux
*** End of File
"#,
            path.display()
        ));

        let patch = parse_patch(&patch).unwrap();
        let chunks = match patch.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let diff = unified_diff_from_chunks(&path, chunks).unwrap();
        let expected_diff = r#"@@ -3 +3,2 @@
 baz
+quux
"#;
        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "foo\nbar\nbaz\nquux\n".to_string(),
        };
        assert_eq!(expected, diff);
    }

    #[test]
    fn test_unified_diff_interleaved_changes() {
        // Original file with six lines.
        let dir = tempdir().unwrap();
        let path = dir.path().join("interleaved.txt");
        fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();

        // Patch replaces two separate lines and appends a new one at EOF using
        // three distinct chunks.
        let patch_body = format!(
            r#"*** Update File: {}
@@
 a
-b
+B
@@
 d
-e
+E
@@
 f
+g
*** End of File"#,
            path.display()
        );
        let patch = wrap_patch(&patch_body);

        // Extract chunks then build the unified diff.
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match parsed.hunks.as_slice() {
            [Hunk::UpdateFile { chunks, .. }] => chunks,
            _ => panic!("Expected a single UpdateFile hunk"),
        };

        let diff = unified_diff_from_chunks(&path, chunks).unwrap();

        let expected_diff = r#"@@ -1,6 +1,7 @@
 a
-b
+B
 c
 d
-e
+E
 f
+g
"#;

        let expected = ApplyPatchFileUpdate {
            unified_diff: expected_diff.to_string(),
            content: "a\nB\nc\nd\nE\nf\ng\n".to_string(),
        };

        assert_eq!(expected, diff);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(&patch, &mut stdout, &mut stderr).unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(
            contents,
            r#"a
B
c
d
E
f
g
"#
        );
    }

    #[test]
    fn test_apply_patch_should_resolve_absolute_paths_in_cwd() {
        let session_dir = tempdir().unwrap();
        let relative_path = "source.txt";

        // Note that we need this file to exist for the patch to be "verified"
        // and parsed correctly.
        let session_file_path = session_dir.path().join(relative_path);
        fs::write(&session_file_path, "session directory content\n").unwrap();

        let argv = vec![
            "apply_patch".to_string(),
            r#"*** Begin Patch
*** Update File: source.txt
@@
-session directory content
+updated session directory content
*** End Patch"#
                .to_string(),
        ];

        let result = maybe_parse_apply_patch_verified(&argv, session_dir.path());

        // Verify the patch contents - as otherwise we may have pulled contents
        // from the wrong file (as we're using relative paths)
        assert_eq!(
            result,
            MaybeApplyPatchVerified::Body(ApplyPatchAction {
                changes: HashMap::from([(
                    session_dir.path().join(relative_path),
                    ApplyPatchFileChange::Update {
                        unified_diff: r#"@@ -1 +1 @@
-session directory content
+updated session directory content
"#
                        .to_string(),
                        move_path: None,
                        new_content: "updated session directory content\n".to_string(),
                    },
                )]),
                patch: argv[1].clone(),
                cwd: session_dir.path().to_path_buf(),
                command: None,
            })
        );
    }

    #[test]
    fn test_apply_patch_fails_on_write_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("readonly.txt");
        fs::write(&path, "before\n").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();

        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@\n-before\n+after\n*** End Patch",
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch(&patch, &mut stdout, &mut stderr);
        assert!(result.is_err());
    }

    #[test]
    fn maybe_parse_begin_patch_basic_command() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let target = root.join("demo.txt");
        fs::write(&target, "old\n").expect("write target");

        let patch_path = root.join("patch.diff");
        let patch_body =
            "*** Begin Patch\n*** Update File: demo.txt\n@@\n-old\n+new\n*** End Patch\n";
        fs::write(&patch_path, patch_body).expect("write patch");

        let argv = vec![
            "begin_patch".to_string(),
            "-f".to_string(),
            patch_path.to_string_lossy().to_string(),
        ];

        match maybe_parse_apply_patch_verified(&argv, root) {
            MaybeApplyPatchVerified::Body(action) => {
                assert_eq!(action.cwd, root);
                assert_eq!(action.patch.trim_end(), patch_body.trim_end());
                let expected = vec![
                    "begin_patch".to_string(),
                    "-f".to_string(),
                    patch_path.to_string_lossy().to_string(),
                ];
                assert_eq!(action.command, Some(expected));
            }
            other => panic!("expected Body, got {other:?}"),
        }
    }

    #[test]
    fn maybe_parse_begin_patch_sanitizes_dry_run_flags() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let target = root.join("demo.txt");
        fs::write(&target, "old\n").expect("write target");

        let patch_path = root.join("patch.diff");
        let patch_body =
            "*** Begin Patch\n*** Update File: demo.txt\n@@\n-old\n+new\n*** End Patch\n";
        fs::write(&patch_path, patch_body).expect("write patch");

        let argv = vec![
            "begin_patch".to_string(),
            "--dry-run".to_string(),
            "--no-logs".to_string(),
            "--machine".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
            "--stdout-schema".to_string(),
            "v2".to_string(),
            "-f".to_string(),
            patch_path.to_string_lossy().to_string(),
        ];

        match maybe_parse_apply_patch_verified(&argv, root) {
            MaybeApplyPatchVerified::Body(action) => {
                let expected = vec![
                    "begin_patch".to_string(),
                    "-f".to_string(),
                    patch_path.to_string_lossy().to_string(),
                ];
                assert_eq!(action.command, Some(expected));
            }
            other => panic!("expected Body, got {other:?}"),
        }
    }

    #[test]
    fn collect_unapplied_entries_captures_failed_changes() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();

        let add_path = root.join("add.txt");
        let update_path = root.join("update.txt");
        let delete_path = root.join("delete.txt");

        fs::write(&update_path, "old\n").expect("write update");
        fs::write(&delete_path, "remove\n").expect("write delete");

        let planned = vec![
            PlannedChange {
                summary_index: 0,
                kind: PlannedChangeKind::Add {
                    path: add_path.clone(),
                    content: "new file\n".to_string(),
                    line_ending: LineEnding::default(),
                },
            },
            PlannedChange {
                summary_index: 1,
                kind: PlannedChangeKind::Update {
                    original: update_path.clone(),
                    dest: update_path.clone(),
                    new_content: "updated\n".to_string(),
                    line_ending: LineEnding::default(),
                    metadata: FileMetadataSnapshot {
                        permissions: None,
                        accessed: None,
                        modified: None,
                    },
                    reference_had_final_newline: true,
                },
            },
            PlannedChange {
                summary_index: 2,
                kind: PlannedChangeKind::Delete {
                    path: delete_path.clone(),
                },
            },
        ];

        let summaries = vec![
            OperationSummary::new(OperationAction::Add, add_path.clone())
                .with_status(OperationStatus::Planned),
            OperationSummary::new(OperationAction::Update, update_path.clone())
                .with_status(OperationStatus::Failed),
            OperationSummary::new(OperationAction::Delete, delete_path.clone())
                .with_status(OperationStatus::Failed),
        ];

        let entries = collect_unapplied_entries(&planned, &summaries);
        assert_eq!(entries.len(), 3);

        let add_entry = entries
            .iter()
            .find(|entry| entry.path == add_path)
            .expect("add entry");
        assert_eq!(add_entry.kind, UnappliedKind::Add);
        assert_eq!(add_entry.contents, "new file\n");

        let update_entry = entries
            .iter()
            .find(|entry| entry.path == update_path)
            .expect("update entry");
        assert_eq!(update_entry.kind, UnappliedKind::Update);
        assert_eq!(update_entry.contents, "updated\n");

        let delete_entry = entries
            .iter()
            .find(|entry| entry.path == delete_path)
            .expect("delete entry");
        assert_eq!(delete_entry.kind, UnappliedKind::Delete);
        assert_eq!(delete_entry.contents, "remove\n");
    }

    #[test]
    fn summarize_unapplied_produces_messages() {
        let entry = UnappliedEntry {
            path: PathBuf::from("foo.rs"),
            kind: UnappliedKind::Update,
            contents: "fn main() {}\n".to_string(),
        };

        let summaries = summarize_unapplied(&[entry]);
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("foo.rs"));
        assert!(summaries[0].contains("fn main() {}"));
    }
}
