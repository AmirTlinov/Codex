//! This module is responsible for parsing & validating a patch into a list of "hunks".
//! (It does not attempt to actually check that the patch can be applied to the filesystem.)
//!
//! The official Lark grammar for the apply-patch format is:
//!
//! start: begin_patch hunk+ end_patch
//! begin_patch: "*** Begin Patch" LF
//! end_patch: "*** End Patch" LF
//!
//! hunk: add_hunk | delete_hunk | update_hunk | insert_before_symbol | insert_after_symbol | replace_symbol_body
//! add_hunk: "*** Add File: " filename LF add_line+
//! delete_hunk: "*** Delete File: " filename LF
//! update_hunk: "*** Update File: " filename LF move_to? change+
//! insert_before_symbol: "*** Insert Before Symbol: " target LF add_line+
//! insert_after_symbol: "*** Insert After Symbol: " target LF add_line+
//! replace_symbol_body: "*** Replace Symbol Body: " target LF add_line+
//! filename: /(.+)/
//! target: /(.+)/
//! add_line: "+" /(.+)/ LF -> line
//! move_to: "*** Move to: " filename LF
//! change: change_context? change_line+ eof_line?
//! change_context: ("@@" | "@@ " /(.+)/) LF
//! change_line: ("+" | "-" | " ") /(.+)/ LF
//! eof_line: "*** End of File" LF
//!
//! The parser below is a little more lenient than the explicit spec and allows for
//! leading/trailing whitespace around patch markers.
use crate::ApplyPatchArgs;
use crate::ast::SymbolPath;
use crate::ast::symbol_path_from_str;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const INSERT_BEFORE_SYMBOL_MARKER: &str = "*** Insert Before Symbol: ";
const INSERT_AFTER_SYMBOL_MARKER: &str = "*** Insert After Symbol: ";
const REPLACE_SYMBOL_BODY_MARKER: &str = "*** Replace Symbol Body: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

/// Currently, the only OpenAI model that knowingly requires lenient parsing is
/// gpt-4.1. While we could try to require everyone to pass in a strictness
/// param when invoking apply_patch, it is a pain to thread it through all of
/// the call sites, so we resign ourselves allowing lenient parsing for all
/// models. See [`ParseMode::Lenient`] for details on the exceptions we make for
/// gpt-4.1.
const PARSE_IN_STRICT_MODE: bool = false;

#[derive(Debug, PartialEq, Error, Clone)]
pub enum ParseError {
    #[error("invalid patch: {0}")]
    InvalidPatchError(String),
    #[error("invalid hunk at line {line_number}, {message}")]
    InvalidHunkError { message: String, line_number: usize },
}
use ParseError::*;

#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,

        /// Chunks should be in order, i.e. the `change_context` of one chunk
        /// should occur later in the file than the previous chunk.
        chunks: Vec<UpdateFileChunk>,
    },
    InsertBeforeSymbol {
        path: PathBuf,
        symbol: SymbolPath,
        new_lines: Vec<String>,
    },
    InsertAfterSymbol {
        path: PathBuf,
        symbol: SymbolPath,
        new_lines: Vec<String>,
    },
    ReplaceSymbolBody {
        path: PathBuf,
        symbol: SymbolPath,
        new_lines: Vec<String>,
    },
}

impl Hunk {
    pub fn resolve_path(&self, cwd: &Path) -> PathBuf {
        match self {
            Hunk::AddFile { path, .. } => cwd.join(path),
            Hunk::DeleteFile { path } => cwd.join(path),
            Hunk::UpdateFile { path, .. } => cwd.join(path),
            Hunk::InsertBeforeSymbol { path, .. } => cwd.join(path),
            Hunk::InsertAfterSymbol { path, .. } => cwd.join(path),
            Hunk::ReplaceSymbolBody { path, .. } => cwd.join(path),
        }
    }
}

use Hunk::*;

#[derive(Debug, PartialEq, Clone)]
pub struct UpdateFileChunk {
    /// A single line of context used to narrow down the position of the chunk
    /// (this is usually a class, method, or function definition.)
    pub change_context: Option<String>,

    /// A contiguous block of lines that should be replaced with `new_lines`.
    /// `old_lines` must occur strictly after `change_context`.
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,

    /// If set to true, `old_lines` must occur at the end of the source file.
    /// (Tolerance around trailing newlines should be encouraged.)
    pub is_end_of_file: bool,
}

pub fn parse_patch(patch: &str) -> Result<ApplyPatchArgs, ParseError> {
    let mode = if PARSE_IN_STRICT_MODE {
        ParseMode::Strict
    } else {
        ParseMode::Lenient
    };
    let trimmed = patch.trim();
    if trimmed.is_empty() {
        return parse_patch_text(trimmed, mode);
    }

    let begin_count = trimmed
        .lines()
        .filter(|line| line.trim_start().starts_with("*** Begin Patch"))
        .count();
    if begin_count <= 1 {
        return parse_patch_text(trimmed, mode);
    }

    let blocks = split_patch_blocks(trimmed)?;
    let mut all_hunks: Vec<Hunk> = Vec::new();
    for block in blocks {
        let parsed = parse_patch_text(&block, mode)?;
        all_hunks.extend(parsed.hunks);
    }
    let normalized_patch = trimmed.lines().collect::<Vec<_>>().join("\n");
    Ok(ApplyPatchArgs {
        patch: normalized_patch,
        hunks: all_hunks,
        workdir: None,
    })
}

fn split_patch_blocks(patch: &str) -> Result<Vec<String>, ParseError> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut inside = false;

    for (idx, line) in patch.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("*** Begin Patch") {
            if inside {
                return Err(InvalidPatchError(format!(
                    "Nested *** Begin Patch at line {}",
                    idx + 1
                )));
            }
            inside = true;
            current.clear();
        }
        if inside {
            current.push(line);
        }
        if trimmed.starts_with("*** End Patch") {
            if !inside {
                return Err(InvalidPatchError(format!(
                    "*** End Patch without matching begin at line {}",
                    idx + 1
                )));
            }
            inside = false;
            let block = current.join("\n");
            blocks.push(block);
            current.clear();
        }
    }

    if inside {
        return Err(InvalidPatchError(
            "Patch ended before *** End Patch".to_string(),
        ));
    }

    if blocks.is_empty() {
        return Err(InvalidPatchError(
            "Patch does not contain any *** Begin Patch blocks.".to_string(),
        ));
    }

    Ok(blocks)
}

#[derive(Clone, Copy)]
enum ParseMode {
    /// Parse the patch text argument as is.
    Strict,

    /// GPT-4.1 is known to formulate the `command` array for the `local_shell`
    /// tool call for `apply_patch` call using something like the following:
    ///
    /// ```json
    /// [
    ///   "apply_patch",
    ///   "<<'EOF'\n*** Begin Patch\n*** Update File: README.md\n@@...\n*** End Patch\nEOF\n",
    /// ]
    /// ```
    ///
    /// This is a problem because `local_shell` is a bit of a misnomer: the
    /// `command` is not invoked by passing the arguments to a shell like Bash,
    /// but are invoked using something akin to `execvpe(3)`.
    ///
    /// This is significant in this case because where a shell would interpret
    /// `<<'EOF'...` as a heredoc and pass the contents via stdin (which is
    /// fine, as `apply_patch` is specified to read from stdin if no argument is
    /// passed), `execvpe(3)` interprets the heredoc as a literal string. To get
    /// the `local_shell` tool to run a command the way shell would, the
    /// `command` array must be something like:
    ///
    /// ```json
    /// [
    ///   "bash",
    ///   "-lc",
    ///   "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: README.md\n@@...\n*** End Patch\nEOF\n",
    /// ]
    /// ```
    ///
    /// In lenient mode, we check if the argument to `apply_patch` starts with
    /// `<<'EOF'` and ends with `EOF\n`. If so, we strip off these markers,
    /// trim() the result, and treat what is left as the patch text.
    Lenient,
}

fn parse_patch_text(patch: &str, mode: ParseMode) -> Result<ApplyPatchArgs, ParseError> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let lines: &[&str] = match check_patch_boundaries_strict(&lines) {
        Ok(()) => &lines,
        Err(e) => match mode {
            ParseMode::Strict => {
                return Err(e);
            }
            ParseMode::Lenient => check_patch_boundaries_lenient(&lines, e)?,
        },
    };

    let mut hunks: Vec<Hunk> = Vec::new();
    // The above checks ensure that lines.len() >= 2.
    let last_line_index = lines.len().saturating_sub(1);
    let mut remaining_lines = &lines[1..last_line_index];
    let mut line_number = 2;

    while !remaining_lines.is_empty() && remaining_lines[0].trim().is_empty() {
        remaining_lines = &remaining_lines[1..];
        line_number += 1;
    }

    while !remaining_lines.is_empty() {
        let (hunk, hunk_lines) = parse_one_hunk(remaining_lines, line_number)?;
        hunks.push(hunk);
        line_number += hunk_lines;
        remaining_lines = &remaining_lines[hunk_lines..]
    }
    let patch = lines.join("\n");
    Ok(ApplyPatchArgs {
        hunks,
        patch,
        workdir: None,
    })
}

/// Checks the start and end lines of the patch text for `apply_patch`,
/// returning an error if they do not match the expected markers.
fn check_patch_boundaries_strict(lines: &[&str]) -> Result<(), ParseError> {
    let (first_line, last_line) = match lines {
        [] => (None, None),
        [first] => (Some(first), Some(first)),
        [first, .., last] => (Some(first), Some(last)),
    };
    check_start_and_end_lines_strict(first_line, last_line)
}

/// If we are in lenient mode, we check if the first line starts with `<<EOF`
/// (possibly quoted) and the last line ends with `EOF`. There must be at least
/// 4 lines total because the heredoc markers take up 2 lines and the patch text
/// must have at least 2 lines.
///
/// If successful, returns the lines of the patch text that contain the patch
/// contents, excluding the heredoc markers.
fn check_patch_boundaries_lenient<'a>(
    original_lines: &'a [&'a str],
    original_parse_error: ParseError,
) -> Result<&'a [&'a str], ParseError> {
    match original_lines {
        [first, .., last] => {
            if (first == &"<<EOF" || first == &"<<'EOF'" || first == &"<<\"EOF\"")
                && last.ends_with("EOF")
                && original_lines.len() >= 4
            {
                let inner_lines = &original_lines[1..original_lines.len() - 1];
                match check_patch_boundaries_strict(inner_lines) {
                    Ok(()) => Ok(inner_lines),
                    Err(e) => Err(e),
                }
            } else {
                Err(original_parse_error)
            }
        }
        _ => Err(original_parse_error),
    }
}

fn check_start_and_end_lines_strict(
    first_line: Option<&&str>,
    last_line: Option<&&str>,
) -> Result<(), ParseError> {
    match (first_line, last_line) {
        (Some(&first), Some(&last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => {
            Ok(())
        }
        (Some(&first), _) if first != BEGIN_PATCH_MARKER => Err(InvalidPatchError(String::from(
            "The first line of the patch must be '*** Begin Patch'",
        ))),
        _ => Err(InvalidPatchError(String::from(
            "The last line of the patch must be '*** End Patch'",
        ))),
    }
}

/// Attempts to parse a single hunk from the start of lines.
/// Returns the parsed hunk and the number of lines parsed (or a ParseError).
fn parse_one_hunk(lines: &[&str], line_number: usize) -> Result<(Hunk, usize), ParseError> {
    // Be tolerant of case mismatches and extra padding around marker strings.
    let first_line = lines[0].trim();
    if let Some(path) = first_line.strip_prefix(ADD_FILE_MARKER) {
        // Add File
        let mut contents = String::new();
        let mut parsed_lines = 1;
        for add_line in &lines[1..] {
            if let Some(line_to_add) = add_line.strip_prefix('+') {
                contents.push_str(line_to_add);
                contents.push('\n');
                parsed_lines += 1;
            } else {
                break;
            }
        }
        return Ok((
            AddFile {
                path: PathBuf::from(path),
                contents,
            },
            parsed_lines,
        ));
    } else if let Some(path) = first_line.strip_prefix(DELETE_FILE_MARKER) {
        // Delete File
        return Ok((
            DeleteFile {
                path: PathBuf::from(path),
            },
            1,
        ));
    } else if let Some(path) = first_line.strip_prefix(UPDATE_FILE_MARKER) {
        // Update File
        let mut remaining_lines = &lines[1..];
        let mut parsed_lines = 1;

        // Optional: move file line
        let move_path = remaining_lines
            .first()
            .and_then(|x| x.strip_prefix(MOVE_TO_MARKER));

        if move_path.is_some() {
            remaining_lines = &remaining_lines[1..];
            parsed_lines += 1;
        }

        let mut chunks = Vec::new();
        // NOTE: we need to know to stop once we reach the next special marker header.
        while !remaining_lines.is_empty() {
            // Skip over any completely blank lines that may separate chunks.
            if remaining_lines[0].trim().is_empty() {
                parsed_lines += 1;
                remaining_lines = &remaining_lines[1..];
                continue;
            }

            if remaining_lines[0].starts_with("***") {
                break;
            }

            let (chunk, chunk_lines) = parse_update_file_chunk(
                remaining_lines,
                line_number + parsed_lines,
                chunks.is_empty(),
            )?;
            chunks.push(chunk);
            parsed_lines += chunk_lines;
            remaining_lines = &remaining_lines[chunk_lines..]
        }

        if chunks.is_empty() {
            return Err(InvalidHunkError {
                message: format!("Update file hunk for path '{path}' is empty"),
                line_number,
            });
        }

        return Ok((
            UpdateFile {
                path: PathBuf::from(path),
                move_path: move_path.map(PathBuf::from),
                chunks,
            },
            parsed_lines,
        ));
    } else if let Some(target) = first_line.strip_prefix(INSERT_BEFORE_SYMBOL_MARKER) {
        let (path, symbol) = parse_symbol_header(target, line_number)?;
        let (new_lines, consumed) =
            parse_symbol_lines(&lines[1..], line_number + 1, "Insert Before Symbol")?;
        return Ok((
            InsertBeforeSymbol {
                path,
                symbol,
                new_lines,
            },
            consumed + 1,
        ));
    } else if let Some(target) = first_line.strip_prefix(INSERT_AFTER_SYMBOL_MARKER) {
        let (path, symbol) = parse_symbol_header(target, line_number)?;
        let (new_lines, consumed) =
            parse_symbol_lines(&lines[1..], line_number + 1, "Insert After Symbol")?;
        return Ok((
            InsertAfterSymbol {
                path,
                symbol,
                new_lines,
            },
            consumed + 1,
        ));
    } else if let Some(target) = first_line.strip_prefix(REPLACE_SYMBOL_BODY_MARKER) {
        let (path, symbol) = parse_symbol_header(target, line_number)?;
        let (new_lines, consumed) =
            parse_symbol_lines(&lines[1..], line_number + 1, "Replace Symbol Body")?;
        return Ok((
            ReplaceSymbolBody {
                path,
                symbol,
                new_lines,
            },
            consumed + 1,
        ));
    }

    Err(InvalidHunkError {
        message: format!(
            "'{first_line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        ),
        line_number,
    })
}

fn parse_symbol_header(raw: &str, line_number: usize) -> Result<(PathBuf, SymbolPath), ParseError> {
    let target = raw.trim();
    let (path_part, symbol_part) = target.split_once("::").ok_or_else(|| InvalidHunkError {
        message: format!(
            "Symbol hunk header '{target}' must include a symbol path (use 'file::Symbol')"
        ),
        line_number,
    })?;

    let path = PathBuf::from(path_part.trim());
    if path.as_os_str().is_empty() {
        return Err(InvalidHunkError {
            message: "Symbol hunk header must include a file path".to_string(),
            line_number,
        });
    }

    let symbol_path = symbol_path_from_str(symbol_part.trim());
    if symbol_path.is_empty() {
        return Err(InvalidHunkError {
            message: format!("Symbol path for '{target}' is empty"),
            line_number,
        });
    }

    Ok((path, symbol_path))
}

fn parse_symbol_lines(
    lines: &[&str],
    line_number: usize,
    op_name: &str,
) -> Result<(Vec<String>, usize), ParseError> {
    let mut collected = Vec::new();
    let mut consumed = 0;
    for line in lines.iter() {
        if let Some(rest) = line.strip_prefix('+') {
            collected.push(rest.to_string());
            consumed += 1;
        } else {
            break;
        }
    }

    if collected.is_empty() {
        return Err(InvalidHunkError {
            message: format!("{op_name} hunk must include at least one '+ line'"),
            line_number,
        });
    }

    Ok((collected, consumed))
}

fn parse_update_file_chunk(
    lines: &[&str],
    line_number: usize,
    allow_missing_context: bool,
) -> Result<(UpdateFileChunk, usize), ParseError> {
    if lines.is_empty() {
        return Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number,
        });
    }
    // If we see an explicit context marker @@ or @@ <context>, consume it; otherwise, optionally
    // allow treating the chunk as starting directly with diff lines.
    let (change_context, start_index) = if lines[0] == EMPTY_CHANGE_CONTEXT_MARKER {
        (None, 1)
    } else if let Some(context) = lines[0].strip_prefix(CHANGE_CONTEXT_MARKER) {
        (Some(context.to_string()), 1)
    } else {
        if !allow_missing_context {
            return Err(InvalidHunkError {
                message: format!(
                    "Expected update hunk to start with a @@ context marker, got: '{}'",
                    lines[0]
                ),
                line_number,
            });
        }
        (None, 0)
    };
    if start_index >= lines.len() {
        return Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number: line_number + 1,
        });
    }
    let mut chunk = UpdateFileChunk {
        change_context,
        old_lines: Vec::new(),
        new_lines: Vec::new(),
        is_end_of_file: false,
    };
    let mut parsed_lines = 0;
    for line in &lines[start_index..] {
        match *line {
            EOF_MARKER => {
                if parsed_lines == 0 {
                    return Err(InvalidHunkError {
                        message: "Update hunk does not contain any lines".to_string(),
                        line_number: line_number + 1,
                    });
                }
                chunk.is_end_of_file = true;
                parsed_lines += 1;
                break;
            }
            line_contents => {
                match line_contents.chars().next() {
                    None => {
                        // Interpret this as an empty line.
                        chunk.old_lines.push(String::new());
                        chunk.new_lines.push(String::new());
                    }
                    Some(' ') => {
                        chunk.old_lines.push(line_contents[1..].to_string());
                        chunk.new_lines.push(line_contents[1..].to_string());
                    }
                    Some('+') => {
                        chunk.new_lines.push(line_contents[1..].to_string());
                    }
                    Some('-') => {
                        chunk.old_lines.push(line_contents[1..].to_string());
                    }
                    _ => {
                        if parsed_lines == 0 {
                            return Err(InvalidHunkError {
                                message: format!(
                                    "Unexpected line found in update hunk: '{line_contents}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                                ),
                                line_number: line_number + 1,
                            });
                        }
                        // Assume this is the start of the next hunk.
                        break;
                    }
                }
                parsed_lines += 1;
            }
        }
    }

    Ok((chunk, parsed_lines + start_index))
}

#[test]
fn test_parse_patch() {
    assert_eq!(
        parse_patch_text("bad", ParseMode::Strict),
        Err(InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string()
        ))
    );

    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
bad",
            ParseMode::Strict
        ),
        Err(InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string()
        ))
    );

    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
*** Update File: test.py
*** End Patch",
            ParseMode::Strict
        ),
        Err(InvalidHunkError {
            message: "Update file hunk for path 'test.py' is empty".to_string(),
            line_number: 2,
        })
    );

    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
*** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        Vec::new()
    );

    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
*** Add File: path/add.py
+abc
+def
*** Delete File: path/delete.py
*** Update File: path/update.py
*** Move to: path/update2.py
@@ def f():
-    pass
+    return 123
*** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![
            AddFile {
                path: PathBuf::from("path/add.py"),
                contents: "abc\ndef\n".to_string()
            },
            DeleteFile {
                path: PathBuf::from("path/delete.py")
            },
            UpdateFile {
                path: PathBuf::from("path/update.py"),
                move_path: Some(PathBuf::from("path/update2.py")),
                chunks: vec![UpdateFileChunk {
                    change_context: Some("def f():".to_string()),
                    old_lines: vec!["    pass".to_string()],
                    new_lines: vec!["    return 123".to_string()],
                    is_end_of_file: false
                }]
            }
        ]
    );

    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
*** Update File: file.py
@@
+line
*** Add File: other.py
+content
*** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![
            UpdateFile {
                path: PathBuf::from("file.py"),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec![],
                    new_lines: vec!["line".to_string()],
                    is_end_of_file: false
                }],
            },
            AddFile {
                path: PathBuf::from("other.py"),
                contents: "content\n".to_string()
            }
        ]
    );

    // Update hunk without an explicit @@ header for the first chunk should parse.
    // Use a raw string to preserve the leading space diff marker on the context line.
    assert_eq!(
        parse_patch_text(
            r"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![UpdateFile {
            path: PathBuf::from("file2.py"),
            move_path: None,
            chunks: vec![UpdateFileChunk {
                change_context: None,
                old_lines: vec!["import foo".to_string()],
                new_lines: vec!["import foo".to_string(), "bar".to_string()],
                is_end_of_file: false,
            }],
        }]
    );
}

#[test]
fn test_parse_patch_lenient() {
    let patch_text = r#"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch"#;
    let expected_patch = vec![UpdateFile {
        path: PathBuf::from("file2.py"),
        move_path: None,
        chunks: vec![UpdateFileChunk {
            change_context: None,
            old_lines: vec!["import foo".to_string()],
            new_lines: vec!["import foo".to_string(), "bar".to_string()],
            is_end_of_file: false,
        }],
    }];
    let expected_error =
        InvalidPatchError("The first line of the patch must be '*** Begin Patch'".to_string());

    let patch_text_in_heredoc = format!("<<EOF\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch.clone(),
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_single_quoted_heredoc = format!("<<'EOF'\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_single_quoted_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_single_quoted_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch.clone(),
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_double_quoted_heredoc = format!("<<\"EOF\"\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_double_quoted_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_double_quoted_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch,
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_mismatched_quotes_heredoc = format!("<<\"EOF'\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_mismatched_quotes_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_mismatched_quotes_heredoc, ParseMode::Lenient),
        Err(expected_error.clone())
    );

    let patch_text_with_missing_closing_heredoc = "<<EOF\n*** Begin Patch
*** Update File: file2.py\nEOF\n"
        .to_string();
    assert_eq!(
        parse_patch_text(&patch_text_with_missing_closing_heredoc, ParseMode::Strict),
        Err(expected_error)
    );
    assert_eq!(
        parse_patch_text(&patch_text_with_missing_closing_heredoc, ParseMode::Lenient),
        Err(InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string()
        ))
    );
}

#[test]
fn test_parse_one_hunk() {
    assert_eq!(
        parse_one_hunk(&["bad"], 234),
        Err(InvalidHunkError {
            message: "'bad' is not a valid hunk header. \
            Valid hunk headers: '*** Add File: {path}', '*** Delete File: {path}', '*** Update File: {path}'".to_string(),
            line_number: 234
        })
    );
    // Other edge cases are already covered by tests above/below.
}

#[test]
fn test_update_file_chunk() {
    assert_eq!(
        parse_update_file_chunk(&["bad"], 123, false),
        Err(InvalidHunkError {
            message: "Expected update hunk to start with a @@ context marker, got: 'bad'"
                .to_string(),
            line_number: 123
        })
    );
    assert_eq!(
        parse_update_file_chunk(&["@@"], 123, false),
        Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number: 124
        })
    );
    assert_eq!(
        parse_update_file_chunk(&["@@", "bad"], 123, false),
        Err(InvalidHunkError {
            message:  "Unexpected line found in update hunk: 'bad'. \
                       Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)".to_string(),
            line_number: 124
        })
    );
    assert_eq!(
        parse_update_file_chunk(&["@@", "*** End of File"], 123, false),
        Err(InvalidHunkError {
            message: "Update hunk does not contain any lines".to_string(),
            line_number: 124
        })
    );
    assert_eq!(
        parse_update_file_chunk(
            &[
                "@@ change_context",
                "",
                " context",
                "-remove",
                "+add",
                " context2",
                "*** End Patch",
            ],
            123,
            false
        ),
        Ok((
            (UpdateFileChunk {
                change_context: Some("change_context".to_string()),
                old_lines: vec![
                    "".to_string(),
                    "context".to_string(),
                    "remove".to_string(),
                    "context2".to_string()
                ],
                new_lines: vec![
                    "".to_string(),
                    "context".to_string(),
                    "add".to_string(),
                    "context2".to_string()
                ],
                is_end_of_file: false
            }),
            6
        ))
    );
    assert_eq!(
        parse_update_file_chunk(&["@@", "+line", "*** End of File"], 123, false),
        Ok((
            (UpdateFileChunk {
                change_context: None,
                old_lines: vec![],
                new_lines: vec!["line".to_string()],
                is_end_of_file: true
            }),
            3
        ))
    );
}

#[test]
fn parse_symbol_insert_before_hunk() {
    let patch = [
        "*** Begin Patch",
        "*** Insert Before Symbol: lib.rs::greet",
        "+// comment",
        "*** End Patch",
    ]
    .join("\n");

    let parsed = parse_patch(&patch).unwrap();
    pretty_assertions::assert_eq!(
        parsed.hunks,
        vec![InsertBeforeSymbol {
            path: PathBuf::from("lib.rs"),
            symbol: symbol_path_from_str("greet"),
            new_lines: vec!["// comment".to_string()],
        }]
    );
}

#[test]
fn parse_symbol_insert_after_hunk() {
    let patch = [
        "*** Begin Patch",
        "*** Insert After Symbol: main.py::Greeter::greet",
        "+print('ok')",
        "*** End Patch",
    ]
    .join("\n");

    let parsed = parse_patch(&patch).unwrap();
    pretty_assertions::assert_eq!(
        parsed.hunks,
        vec![InsertAfterSymbol {
            path: PathBuf::from("main.py"),
            symbol: symbol_path_from_str("Greeter::greet"),
            new_lines: vec!["print('ok')".to_string()],
        }]
    );
}

#[test]
fn parse_symbol_replace_body_hunk() {
    let patch = [
        "*** Begin Patch",
        "*** Replace Symbol Body: service.ts::Service::run",
        "+{",
        "+  return true;",
        "+}",
        "*** End Patch",
    ]
    .join("\n");

    let parsed = parse_patch(&patch).unwrap();
    pretty_assertions::assert_eq!(
        parsed.hunks,
        vec![ReplaceSymbolBody {
            path: PathBuf::from("service.ts"),
            symbol: symbol_path_from_str("Service::run"),
            new_lines: vec![
                "{".to_string(),
                "  return true;".to_string(),
                "}".to_string()
            ],
        }]
    );
}

#[test]
fn parse_symbol_header_requires_symbol_path() {
    match parse_symbol_header("lib.rs", 42) {
        Err(InvalidHunkError {
            message,
            line_number,
        }) => {
            pretty_assertions::assert_eq!(
                message,
                "Symbol hunk header 'lib.rs' must include a symbol path (use 'file::Symbol')"
                    .to_string()
            );
            pretty_assertions::assert_eq!(line_number, 42);
        }
        other => panic!("ожидалась ошибка, получено: {other:?}"),
    }
}

#[test]
fn parse_symbol_lines_require_changes() {
    match parse_symbol_lines(&["context"], 10, "Insert Before Symbol") {
        Err(InvalidHunkError {
            message,
            line_number,
        }) => {
            pretty_assertions::assert_eq!(
                message,
                "Insert Before Symbol hunk must include at least one '+ line'".to_string()
            );
            pretty_assertions::assert_eq!(line_number, 10);
        }
        other => panic!("ожидалась ошибка, получено: {other:?}"),
    }
}
