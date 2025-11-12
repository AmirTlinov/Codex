use crate::ApplyPatchError;
use crate::DiagnosticItem;
use crate::ast::SymbolLocator;
use crate::ast::SymbolPath;
use crate::ast::SymbolResolution;
use crate::ast::SymbolTarget;
use crate::ast::parse_tree_for_language;
use crate::ast::resolve_locator;
use crate::ast::resolve_locator_by_language;
use crate::ast::service::global_service_handle;
use crate::ast_ops::AstAttributePlacement;
use crate::ast_ops::AstImportChange;
use crate::ast_ops::AstImportChangeKind;
use crate::ast_ops::AstInsertPosition;
use crate::ast_ops::AstOperationKind;
use crate::ast_ops::AstOperationSpec;
use crate::ast_ops::AstPropagationScope;
use crate::ast_ops::AstTemplateMode;
use crate::parser::ParseError::InvalidPatchError;
use similar::TextDiff;
use std::ops::Range;
use std::path::Path;

const MAX_COMPLEXITY: usize = 10;

pub struct AstEditPlan {
    pub new_content: String,
    pub message: String,
    pub diagnostics: Vec<DiagnosticItem>,
    pub preview: Option<String>,
}

pub fn apply_ast_operation(
    path: &Path,
    original: &str,
    spec: &AstOperationSpec,
) -> Result<AstEditPlan, ApplyPatchError> {
    global_service_handle().plan_operation(path, original.to_string(), spec.clone())
}

pub(crate) fn plan_ast_operation(
    path: &Path,
    original: &str,
    spec: &AstOperationSpec,
) -> Result<AstEditPlan, ApplyPatchError> {
    let (locator, language) = resolve_language_and_locator(path, spec.language.as_deref())?;
    match &spec.kind {
        AstOperationKind::RenameSymbol {
            symbol,
            new_name,
            propagate,
        } => rename_symbol(
            path, original, locator, language, symbol, new_name, *propagate,
        ),
        AstOperationKind::UpdateSignature {
            symbol,
            new_signature,
        } => update_signature(path, original, locator, language, symbol, new_signature),
        AstOperationKind::MoveBlock {
            symbol,
            destination,
            position,
        } => move_block(
            path,
            original,
            locator,
            language,
            symbol,
            destination.as_ref(),
            *position,
        ),
        AstOperationKind::UpdateImports { mutations } => {
            update_imports(original, language, mutations)
        }
        AstOperationKind::InsertAttributes {
            symbol,
            placement,
            attributes,
        } => insert_attributes(
            path, original, locator, language, symbol, *placement, attributes,
        ),
        AstOperationKind::TemplateEmit {
            mode,
            symbol,
            template,
        } => template_emit(
            path,
            original,
            locator,
            language,
            *mode,
            symbol.as_ref(),
            template,
        ),
    }
}

fn resolve_language_and_locator(
    path: &Path,
    hint: Option<&str>,
) -> Result<(&'static dyn SymbolLocator, &'static str), ApplyPatchError> {
    if let Some(lang) = hint {
        if let Some(locator) = resolve_locator_by_language(lang) {
            return Ok((locator, locator.language()));
        }
        return Err(parse_error(format!("Unknown language override '{lang}'")));
    }
    let locator = resolve_locator(path).ok_or_else(|| {
        parse_error(format!(
            "Cannot infer language for {}; provide lang=<...>",
            path.display()
        ))
    })?;
    Ok((locator, locator.language()))
}

fn rename_symbol(
    path: &Path,
    source: &str,
    locator: &dyn SymbolLocator,
    language: &str,
    symbol: &SymbolPath,
    new_name: &str,
    propagate: AstPropagationScope,
) -> Result<AstEditPlan, ApplyPatchError> {
    let target = resolve_symbol(locator, source, symbol, path)?;
    let name_range = target
        .name_range
        .clone()
        .ok_or_else(|| parse_error("Symbol does not expose a name range".into()))?;
    let mut edits = Vec::new();
    edits.push(TextEdit::replace(name_range, new_name.to_string()));

    if matches!(propagate, AstPropagationScope::File) {
        let mut cascade = collect_identifier_ranges(language, source, &target, new_name)?;
        edits.append(&mut cascade);
    }

    let updated = apply_edits(source, &mut edits);
    let diagnostics =
        validate_complexity(language, locator, source, &updated, symbol, Some(new_name))?;
    let message = format!(
        "ast: rename {} -> {} ({})",
        symbol,
        new_name,
        match propagate {
            AstPropagationScope::DefinitionOnly => "definition",
            AstPropagationScope::File => "file",
        }
    );
    Ok(build_ast_edit_plan(source, updated, message, diagnostics))
}

fn update_signature(
    path: &Path,
    source: &str,
    locator: &dyn SymbolLocator,
    language: &str,
    symbol: &SymbolPath,
    new_signature: &str,
) -> Result<AstEditPlan, ApplyPatchError> {
    let target = resolve_symbol(locator, source, symbol, path)?;
    let signature_end = target
        .body_range
        .as_ref()
        .map(|range| range.start)
        .unwrap_or(target.header_range.end);
    let signature_range = target.header_range.start..signature_end;
    let mut edits = vec![TextEdit::replace(
        signature_range,
        ensure_trailing_newline(new_signature),
    )];
    let updated = apply_edits(source, &mut edits);
    let diagnostics = validate_complexity(language, locator, source, &updated, symbol, None)?;
    Ok(build_ast_edit_plan(
        source,
        updated,
        format!("ast: updated signature of {symbol}"),
        diagnostics,
    ))
}

fn move_block(
    path: &Path,
    source: &str,
    locator: &dyn SymbolLocator,
    language: &str,
    symbol: &SymbolPath,
    destination: Option<&SymbolPath>,
    position: AstInsertPosition,
) -> Result<AstEditPlan, ApplyPatchError> {
    let source_target = resolve_symbol(locator, source, symbol, path)?;
    let block_range = compute_block_range(&source_target)?;
    let block = source
        .get(block_range.clone())
        .ok_or_else(|| parse_error("Failed to slice source block".into()))?
        .to_string();

    let mut edits = Vec::new();
    if !matches!(position, AstInsertPosition::Delete) {
        let dest_symbol = destination.ok_or_else(|| {
            parse_error("move-block requires target when position is not delete".into())
        })?;
        let dest_target = resolve_symbol(locator, source, dest_symbol, path)?;
        if matches!(position, AstInsertPosition::Replace) {
            let dest_range = compute_block_range(&dest_target)?;
            edits.push(TextEdit::replace(dest_range, block));
        } else {
            let insert_index = compute_destination_index(&dest_target, &source_target, position)?;
            edits.push(TextEdit::insert(insert_index, block));
        }
    }
    edits.push(TextEdit::replace(block_range, String::new()));

    let updated = apply_edits(source, &mut edits);
    let mut diagnostics = validate_complexity(language, locator, source, &updated, symbol, None)?;
    if let Some(dest_symbol) = destination {
        let mut extra =
            validate_complexity(language, locator, source, &updated, dest_symbol, None)?;
        diagnostics.append(&mut extra);
    }
    Ok(build_ast_edit_plan(
        source,
        updated,
        format!("ast: move-block on {symbol}"),
        diagnostics,
    ))
}

fn update_imports(
    source: &str,
    language: &str,
    mutations: &[AstImportChange],
) -> Result<AstEditPlan, ApplyPatchError> {
    let updated = rewrite_import_block(source, language, mutations)?;
    Ok(build_ast_edit_plan(
        source,
        updated,
        format!("ast: update-imports {} mutations", mutations.len()),
        Vec::new(),
    ))
}

fn insert_attributes(
    path: &Path,
    source: &str,
    locator: &dyn SymbolLocator,
    _language: &str,
    symbol: &SymbolPath,
    placement: AstAttributePlacement,
    attributes: &[String],
) -> Result<AstEditPlan, ApplyPatchError> {
    let target = resolve_symbol(locator, source, symbol, path)?;
    let insertion_index = match placement {
        AstAttributePlacement::Before => target.header_range.start,
        AstAttributePlacement::After => target.header_range.end,
        AstAttributePlacement::BodyStart => target
            .body_range
            .as_ref()
            .map(|range| range.start + 1)
            .ok_or_else(|| parse_error("BodyStart placement requires a symbol body".into()))?,
    };
    let payload = ensure_trailing_newline(&attributes.join("\n"));
    let mut edits = vec![TextEdit::insert(insertion_index, payload)];
    let updated = apply_edits(source, &mut edits);
    Ok(build_ast_edit_plan(
        source,
        updated,
        format!("ast: insert-attributes ({placement:?}) for {symbol}"),
        Vec::new(),
    ))
}

fn template_emit(
    path: &Path,
    source: &str,
    locator: &dyn SymbolLocator,
    language: &str,
    mode: AstTemplateMode,
    symbol: Option<&SymbolPath>,
    template: &str,
) -> Result<AstEditPlan, ApplyPatchError> {
    let rendered = render_template(template, language, symbol);
    let insertion_index = compute_template_index(source, locator, path, mode, symbol)?;
    let mut edits = vec![TextEdit::insert(
        insertion_index,
        ensure_trailing_newline(&rendered),
    )];
    let updated = apply_edits(source, &mut edits);
    Ok(build_ast_edit_plan(
        source,
        updated,
        format!("ast: template {mode:?}"),
        Vec::new(),
    ))
}

struct TextEdit {
    range: Range<usize>,
    replacement: String,
}

impl TextEdit {
    fn replace(range: Range<usize>, replacement: String) -> Self {
        Self { range, replacement }
    }

    fn insert(index: usize, text: String) -> Self {
        Self {
            range: index..index,
            replacement: text,
        }
    }
}

fn apply_edits(source: &str, edits: &mut [TextEdit]) -> String {
    edits.sort_by(|a, b| b.range.start.cmp(&a.range.start));
    let mut result = source.to_string();
    for edit in edits {
        let start = edit.range.start.min(result.len());
        let end = edit.range.end.min(result.len());
        result.replace_range(start..end, &edit.replacement);
    }
    result
}

fn resolve_symbol(
    locator: &dyn SymbolLocator,
    source: &str,
    symbol: &SymbolPath,
    path: &Path,
) -> Result<SymbolTarget, ApplyPatchError> {
    match locator.locate(source, symbol) {
        SymbolResolution::Match(target) => Ok(target),
        SymbolResolution::Unsupported { reason } | SymbolResolution::NotFound { reason } => {
            Err(parse_error(format!(
                "Failed to locate symbol {} in {}: {reason}",
                symbol,
                path.display()
            )))
        }
    }
}

fn ensure_trailing_newline(input: &str) -> String {
    if input.ends_with('\n') {
        input.to_string()
    } else {
        format!("{input}\n")
    }
}

fn compute_block_range(target: &SymbolTarget) -> Result<Range<usize>, ApplyPatchError> {
    let body = target
        .body_range
        .as_ref()
        .ok_or_else(|| parse_error("move-block requires a symbol body".into()))?;
    Ok(target.header_range.start..body.end)
}

fn compute_destination_index(
    dest: &SymbolTarget,
    source: &SymbolTarget,
    position: AstInsertPosition,
) -> Result<usize, ApplyPatchError> {
    let idx = match position {
        AstInsertPosition::Before => dest.header_range.start,
        AstInsertPosition::After => dest
            .body_range
            .as_ref()
            .map(|r| r.end)
            .unwrap_or(dest.header_range.end),
        AstInsertPosition::Replace => dest.header_range.start,
        AstInsertPosition::IntoBody => dest
            .body_range
            .as_ref()
            .map(|r| r.start + 1)
            .ok_or_else(|| parse_error("Destination symbol has no body".into()))?,
        AstInsertPosition::Delete => dest.header_range.start,
    };
    let block_range = compute_block_range(source)?;
    if idx >= block_range.start && idx <= block_range.end {
        Ok(block_range.end)
    } else {
        Ok(idx)
    }
}

fn collect_identifier_ranges(
    language: &str,
    source: &str,
    target: &SymbolTarget,
    new_name: &str,
) -> Result<Vec<TextEdit>, ApplyPatchError> {
    let tree = parse_tree_for_language(language, source)
        .map_err(|err| parse_error(format!("Failed to parse {language}: {err}")))?;
    let kinds = identifier_kinds(language);
    let mut stack = vec![tree.root_node()];
    let mut edits = Vec::new();
    let Some(old_name) = target.symbol_path.last() else {
        return Ok(edits);
    };
    let name_range = target.name_range.clone().unwrap_or(0..0);
    while let Some(node) = stack.pop() {
        if kinds.contains(&node.kind()) {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            if text == old_name && node.byte_range() != name_range {
                edits.push(TextEdit::replace(node.byte_range(), new_name.to_string()));
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    Ok(edits)
}

fn identifier_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &["identifier", "type_identifier", "field_identifier"],
        "typescript" | "javascript" => &["identifier", "property_identifier"],
        "python" => &["identifier"],
        "shell" => &["word"],
        "go" => &["identifier"],
        "cpp" => &["identifier"],
        _ => &["identifier"],
    }
}

fn rewrite_import_block(
    source: &str,
    language: &str,
    mutations: &[AstImportChange],
) -> Result<String, ApplyPatchError> {
    let had_trailing_newline = source.ends_with('\n');
    let lines: Vec<String> = if source.is_empty() {
        Vec::new()
    } else {
        source
            .lines()
            .map(std::string::ToString::to_string)
            .collect()
    };
    let mut shebang_offset = 0;
    if lines
        .first()
        .map(|line| line.trim_start().starts_with("#!"))
        .unwrap_or(false)
    {
        shebang_offset = 1;
    }

    let mut import_end = shebang_offset;
    while import_end < lines.len() {
        let trimmed = lines[import_end].trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            import_end += 1;
            continue;
        }
        if is_import_line(language, trimmed) {
            import_end += 1;
            continue;
        }
        break;
    }

    let mut comments = Vec::new();
    let mut existing = Vec::new();
    for line in &lines[shebang_offset..import_end] {
        let trimmed = line.trim();
        if is_import_line(language, trimmed) {
            existing.push(trimmed.to_string());
        } else {
            comments.push(line.clone());
        }
    }

    for change in mutations {
        match change.kind {
            AstImportChangeKind::Add => {
                if !existing.iter().any(|line| line == &change.value) {
                    existing.push(change.value.clone());
                }
            }
            AstImportChangeKind::Remove => {
                existing.retain(|line| line != &change.value);
            }
        }
    }
    existing.sort();

    let mut rebuilt: Vec<String> = Vec::new();
    rebuilt.extend_from_slice(&lines[..shebang_offset]);
    rebuilt.extend(comments);
    rebuilt.extend(existing);
    if import_end < lines.len() && !rebuilt.is_empty() {
        rebuilt.push(String::new());
    }
    rebuilt.extend_from_slice(&lines[import_end..]);

    let mut result = rebuilt.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

fn is_import_line(language: &str, line: &str) -> bool {
    let trimmed = line.trim_start();
    match language {
        "rust" => trimmed.starts_with("use ") || trimmed.starts_with("pub use"),
        "typescript" | "javascript" => {
            trimmed.starts_with("import ") || trimmed.starts_with("export ")
        }
        "python" => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        "shell" => trimmed.starts_with("source ") || trimmed.starts_with(". "),
        "go" => trimmed.starts_with("import "),
        _ => trimmed.starts_with("use "),
    }
}

fn render_template(template: &str, language: &str, symbol: Option<&SymbolPath>) -> String {
    let mut rendered = template.replace("{{language}}", language);
    if let Some(symbol) = symbol {
        rendered = rendered.replace("{{symbol}}", &symbol.to_string());
    }
    let timestamp = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    rendered.replace("{{timestamp}}", &timestamp)
}

fn compute_template_index(
    source: &str,
    locator: &dyn SymbolLocator,
    path: &Path,
    mode: AstTemplateMode,
    symbol: Option<&SymbolPath>,
) -> Result<usize, ApplyPatchError> {
    let index = match mode {
        AstTemplateMode::FileStart => {
            if source.starts_with("#!") {
                source.find('\n').map(|idx| idx + 1).unwrap_or(source.len())
            } else {
                0
            }
        }
        AstTemplateMode::FileEnd => source.len(),
        AstTemplateMode::BeforeSymbol
        | AstTemplateMode::AfterSymbol
        | AstTemplateMode::BodyStart
        | AstTemplateMode::BodyEnd => {
            let symbol = symbol.ok_or_else(|| {
                parse_error("template mode targeting symbol requires symbol=<...>".into())
            })?;
            let target = resolve_symbol(locator, source, symbol, path)?;
            match mode {
                AstTemplateMode::BeforeSymbol => target.header_range.start,
                AstTemplateMode::AfterSymbol => target
                    .body_range
                    .as_ref()
                    .map(|r| r.end)
                    .unwrap_or(target.header_range.end),
                AstTemplateMode::BodyStart => target
                    .body_range
                    .as_ref()
                    .map(|r| r.start + 1)
                    .ok_or_else(|| parse_error("Symbol has no body".into()))?,
                AstTemplateMode::BodyEnd => target
                    .body_range
                    .as_ref()
                    .map(|r| r.end)
                    .ok_or_else(|| parse_error("Symbol has no body".into()))?,
                _ => unreachable!(),
            }
        }
    };
    Ok(index)
}

fn validate_complexity(
    language: &str,
    locator: &dyn SymbolLocator,
    previous: &str,
    updated: &str,
    symbol: &SymbolPath,
    renamed: Option<&str>,
) -> Result<Vec<DiagnosticItem>, ApplyPatchError> {
    let before = compute_complexity(language, locator, previous, symbol).unwrap_or(0);
    let lookup_symbol = if let Some(new_name) = renamed {
        symbol.replace_last(new_name.to_string())
    } else {
        symbol.clone()
    };
    let after = compute_complexity(language, locator, updated, &lookup_symbol).unwrap_or(0);
    if after > MAX_COMPLEXITY {
        return Err(parse_error(format!(
            "Cyclomatic complexity for {lookup_symbol} is {after}, exceeding limit {MAX_COMPLEXITY}"
        )));
    }
    let mut diagnostics = Vec::new();
    if after > before {
        diagnostics.push(DiagnosticItem {
            code: "cyclomatic_hint".into(),
            message: format!(
                "Complexity for {lookup_symbol} increased from {before} to {after} — consider extracting helpers"
            ),
        });
    }
    Ok(diagnostics)
}

fn compute_complexity(
    language: &str,
    locator: &dyn SymbolLocator,
    source: &str,
    symbol: &SymbolPath,
) -> Option<usize> {
    match locator.locate(source, symbol) {
        SymbolResolution::Match(target) => target
            .body_range
            .map(|range| count_complexity(language, source, range)),
        _ => None,
    }
}

fn count_complexity(language: &str, source: &str, range: Range<usize>) -> usize {
    if let Ok(tree) = parse_tree_for_language(language, source) {
        let mut stack = vec![tree.root_node()];
        let kinds = complexity_kinds(language);
        let mut score = 1;
        while let Some(node) = stack.pop() {
            if range.contains(&node.start_byte()) && kinds.contains(&node.kind()) {
                score += 1;
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        score
    } else {
        0
    }
}

fn complexity_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "if_expression",
            "match_expression",
            "while_expression",
            "loop_expression",
            "for_expression",
        ],
        "typescript" | "javascript" => &[
            "if_statement",
            "for_statement",
            "while_statement",
            "switch_statement",
            "switch_case",
        ],
        "python" => &[
            "if_statement",
            "elif_clause",
            "for_statement",
            "while_statement",
            "try_statement",
        ],
        "shell" => &[
            "if_statement",
            "elif_clause",
            "for_statement",
            "while_statement",
            "case_item",
        ],
        _ => &["if_statement"],
    }
}

fn build_ast_edit_plan(
    original: &str,
    updated: String,
    message: String,
    diagnostics: Vec<DiagnosticItem>,
) -> AstEditPlan {
    AstEditPlan {
        preview: diff_preview(original, &updated),
        new_content: updated,
        message,
        diagnostics,
    }
}

fn diff_preview(original: &str, updated: &str) -> Option<String> {
    if original == updated {
        return None;
    }
    let diff = TextDiff::from_lines(original, updated)
        .unified_diff()
        .context_radius(3)
        .to_string();
    if diff.trim().is_empty() {
        None
    } else {
        Some(diff)
    }
}

fn parse_error(message: String) -> ApplyPatchError {
    ApplyPatchError::ParseError(InvalidPatchError(message))
}
