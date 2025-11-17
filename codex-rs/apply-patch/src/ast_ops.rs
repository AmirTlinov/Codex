use crate::ApplyPatchError;
use crate::ast::SymbolPath;
use crate::ast::symbol_path_from_str;
use crate::parser::ParseError::InvalidPatchError;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct AstOperationSpec {
    pub language: Option<String>,
    pub kind: AstOperationKind,
}

#[derive(Debug, Clone)]
pub enum AstOperationKind {
    RenameSymbol {
        symbol: SymbolPath,
        new_name: String,
        propagate: AstPropagationScope,
    },
    UpdateSignature {
        symbol: SymbolPath,
        new_signature: String,
    },
    MoveBlock {
        symbol: SymbolPath,
        destination: Option<SymbolPath>,
        position: AstInsertPosition,
    },
    UpdateImports {
        mutations: Vec<AstImportChange>,
    },
    InsertAttributes {
        symbol: SymbolPath,
        placement: AstAttributePlacement,
        attributes: Vec<String>,
    },
    TemplateEmit {
        mode: AstTemplateMode,
        symbol: Option<SymbolPath>,
        template: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstPropagationScope {
    DefinitionOnly,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstInsertPosition {
    Before,
    After,
    Replace,
    IntoBody,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstAttributePlacement {
    Before,
    After,
    BodyStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstTemplateMode {
    FileStart,
    FileEnd,
    BeforeSymbol,
    AfterSymbol,
    BodyStart,
    BodyEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstImportChangeKind {
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstImportChange {
    pub kind: AstImportChangeKind,
    pub value: String,
}

impl AstOperationSpec {
    pub fn from_raw(
        options: &BTreeMap<String, String>,
        payload: &[String],
    ) -> Result<Self, ApplyPatchError> {
        let op = options
            .get("op")
            .ok_or_else(|| parse_error("Ast Operation requires 'op=<name>'".to_string()))?
            .to_lowercase();
        let language = options.get("lang").map(|value| value.trim().to_lowercase());

        let kind = match op.as_str() {
            "rename" | "rename-symbol" => {
                let symbol = parse_symbol_option(options, "symbol")?;
                let new_name = parse_required_option(options, "new_name")?;
                if new_name.trim().is_empty() {
                    return Err(parse_error(
                        "new_name for rename-symbol cannot be empty".into(),
                    ));
                }
                let propagate = options
                    .get("propagate")
                    .map(|value| AstPropagationScope::from_str(value))
                    .transpose()? // convert Result<Option>
                    .unwrap_or(AstPropagationScope::DefinitionOnly);
                AstOperationKind::RenameSymbol {
                    symbol,
                    new_name,
                    propagate,
                }
            }
            "update-signature" => {
                let symbol = parse_symbol_option(options, "symbol")?;
                let new_signature = join_payload(payload);
                if new_signature.trim().is_empty() {
                    return Err(parse_error(
                        "update-signature requires body lines with the new signature".into(),
                    ));
                }
                AstOperationKind::UpdateSignature {
                    symbol,
                    new_signature,
                }
            }
            "move-block" => {
                let symbol = parse_symbol_option(options, "symbol")?;
                let destination = options
                    .get("target")
                    .map(|value| parse_symbol_value(value))
                    .transpose()?;
                let position = options
                    .get("position")
                    .map(|value| AstInsertPosition::from_str(value))
                    .transpose()? // Option<Result<_>> -> Result<Option<_>>
                    .unwrap_or(AstInsertPosition::After);
                if !matches!(position, AstInsertPosition::Delete) && destination.is_none() {
                    return Err(parse_error(
                        "move-block requires target=<symbol> unless position=delete".into(),
                    ));
                }
                AstOperationKind::MoveBlock {
                    symbol,
                    destination,
                    position,
                }
            }
            "update-imports" => {
                let mutations = parse_import_payload(payload)?;
                if mutations.is_empty() {
                    return Err(parse_error(
                        "update-imports requires '+add <stmt>' or '+remove <stmt>' lines".into(),
                    ));
                }
                AstOperationKind::UpdateImports { mutations }
            }
            "insert-attributes" => {
                let symbol = parse_symbol_option(options, "symbol")?;
                let placement = options
                    .get("placement")
                    .map(|value| AstAttributePlacement::from_str(value))
                    .transpose()? // Option<Result<_>> -> Result<Option<_>>
                    .unwrap_or(AstAttributePlacement::Before);
                let attributes: Vec<String> = payload.to_vec();
                if attributes.is_empty() {
                    return Err(parse_error(
                        "insert-attributes requires '+#[attribute]' payload lines".into(),
                    ));
                }
                AstOperationKind::InsertAttributes {
                    symbol,
                    placement,
                    attributes,
                }
            }
            "template" | "template-emit" => {
                let mode = options
                    .get("mode")
                    .map(|value| AstTemplateMode::from_str(value))
                    .transpose()? // Option<Result<_>> -> Result<Option<_>>
                    .unwrap_or(AstTemplateMode::FileEnd);
                let symbol = options
                    .get("symbol")
                    .map(|value| parse_symbol_value(value))
                    .transpose()?;
                if matches!(
                    mode,
                    AstTemplateMode::BeforeSymbol
                        | AstTemplateMode::AfterSymbol
                        | AstTemplateMode::BodyStart
                        | AstTemplateMode::BodyEnd
                ) && symbol.is_none()
                {
                    return Err(parse_error(
                        "template mode requires symbol=<path> when targeting a symbol".into(),
                    ));
                }
                let template = join_payload(payload);
                if template.trim().is_empty() {
                    return Err(parse_error("template requires payload content".into()));
                }
                AstOperationKind::TemplateEmit {
                    mode,
                    symbol,
                    template,
                }
            }
            other => {
                return Err(parse_error(format!("Unsupported Ast Operation '{other}'")));
            }
        };

        Ok(Self { language, kind })
    }
}

impl AstPropagationScope {
    fn from_str(raw: &str) -> Result<Self, ApplyPatchError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "definition" | "definition-only" => Ok(AstPropagationScope::DefinitionOnly),
            "file" | "cascade" => Ok(AstPropagationScope::File),
            other => Err(parse_error(format!("Unknown propagate value '{other}'"))),
        }
    }
}

impl AstInsertPosition {
    fn from_str(raw: &str) -> Result<Self, ApplyPatchError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "before" => Ok(AstInsertPosition::Before),
            "after" => Ok(AstInsertPosition::After),
            "replace" => Ok(AstInsertPosition::Replace),
            "into-body" | "body" => Ok(AstInsertPosition::IntoBody),
            "delete" => Ok(AstInsertPosition::Delete),
            other => Err(parse_error(format!("Unknown position '{other}'"))),
        }
    }
}

impl AstAttributePlacement {
    fn from_str(raw: &str) -> Result<Self, ApplyPatchError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "before" => Ok(AstAttributePlacement::Before),
            "after" => Ok(AstAttributePlacement::After),
            "body" | "body-start" => Ok(AstAttributePlacement::BodyStart),
            other => Err(parse_error(format!("Unknown placement '{other}'"))),
        }
    }
}

impl AstTemplateMode {
    fn from_str(raw: &str) -> Result<Self, ApplyPatchError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "file-start" | "start" => Ok(AstTemplateMode::FileStart),
            "file-end" | "end" => Ok(AstTemplateMode::FileEnd),
            "before-symbol" | "before" => Ok(AstTemplateMode::BeforeSymbol),
            "after-symbol" | "after" => Ok(AstTemplateMode::AfterSymbol),
            "body-start" => Ok(AstTemplateMode::BodyStart),
            "body-end" => Ok(AstTemplateMode::BodyEnd),
            other => Err(parse_error(format!("Unknown template mode '{other}'"))),
        }
    }
}

fn parse_symbol_option(
    options: &BTreeMap<String, String>,
    key: &str,
) -> Result<SymbolPath, ApplyPatchError> {
    let raw = options
        .get(key)
        .ok_or_else(|| parse_error(format!("Ast Operation requires {key}=<symbol>")))?;
    parse_symbol_value(raw)
}

fn parse_symbol_value(raw: &str) -> Result<SymbolPath, ApplyPatchError> {
    let path = symbol_path_from_str(raw);
    if path.is_empty() {
        return Err(parse_error(format!("Symbol '{raw}' is empty")));
    }
    Ok(path)
}

fn parse_required_option(
    options: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, ApplyPatchError> {
    options
        .get(key)
        .cloned()
        .ok_or_else(|| parse_error(format!("Ast Operation requires {key}=<value>")))
}

fn parse_import_payload(payload: &[String]) -> Result<Vec<AstImportChange>, ApplyPatchError> {
    let mut mutations = Vec::new();
    for line in payload {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (cmd, rest) = trimmed.split_once(' ').ok_or_else(|| {
            parse_error("update-imports lines must be 'add <stmt>' or 'remove <stmt>'".into())
        })?;
        let value = rest.trim();
        if value.is_empty() {
            return Err(parse_error(
                "update-imports lines must provide the import or use statement".into(),
            ));
        }
        let kind = match cmd.to_ascii_lowercase().as_str() {
            "add" => AstImportChangeKind::Add,
            "remove" | "rm" => AstImportChangeKind::Remove,
            other => {
                return Err(parse_error(format!(
                    "update-imports command '{other}' must be 'add' or 'remove'"
                )));
            }
        };
        mutations.push(AstImportChange {
            kind,
            value: value.to_string(),
        });
    }
    Ok(mutations)
}

fn join_payload(payload: &[String]) -> String {
    payload.join("\n")
}

fn parse_error(message: String) -> ApplyPatchError {
    ApplyPatchError::ParseError(InvalidPatchError(message))
}
