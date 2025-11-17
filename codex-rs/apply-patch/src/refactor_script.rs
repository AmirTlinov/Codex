use crate::ApplyPatchError;
use crate::ast_ops::AstOperationKind;
use crate::ast_ops::AstOperationSpec;
use crate::parser::ParseError::InvalidPatchError;
use semver::Version;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use starlark::environment::GlobalsBuilder;
use starlark::environment::Module as StarlarkModule;
use starlark::eval::Evaluator;
use starlark::syntax::AstModule;
use starlark::syntax::Dialect;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

pub const AUTO_SYMBOL_PLACEHOLDER: &str = "__script_auto_symbol__";

#[derive(Debug, Clone)]
pub struct ScriptMetadata {
    pub name: String,
    pub version: Version,
    pub engine: String,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub source_path: PathBuf,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct ScriptStep {
    pub id: usize,
    pub path: PathBuf,
    pub language: Option<String>,
    pub query: Option<String>,
    pub capture: Option<String>,
    pub operation: AstOperationSpec,
}

impl ScriptStep {
    pub fn requires_symbol_injection(&self) -> bool {
        matches!(
            self.operation.kind,
            AstOperationKind::RenameSymbol { .. }
                | AstOperationKind::UpdateSignature { .. }
                | AstOperationKind::MoveBlock { .. }
                | AstOperationKind::InsertAttributes { .. }
                | AstOperationKind::TemplateEmit { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct RefactorScript {
    pub metadata: ScriptMetadata,
    pub steps: Vec<ScriptStep>,
}

impl RefactorScript {
    pub fn load_from_path(
        path: &Path,
        vars: &BTreeMap<String, String>,
        format_hint: Option<&str>,
    ) -> Result<Self, ApplyPatchError> {
        let bytes = fs::read(path).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Failed to read refactor script {}: {err}",
                path.display()
            )))
        })?;
        let contents = String::from_utf8(bytes.clone()).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Refactor script {} is not valid UTF-8: {err}",
                path.display()
            )))
        })?;
        let format = ScriptFormat::resolve(path, format_hint)?;
        Self::from_document(path, &contents, vars, format, bytes)
    }

    fn from_document(
        path: &Path,
        contents: &str,
        vars: &BTreeMap<String, String>,
        format: ScriptFormat,
        raw_bytes: Vec<u8>,
    ) -> Result<Self, ApplyPatchError> {
        let raw = parse_raw_script(path, contents, format)?;
        let version = Version::parse(&raw.version).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Invalid script version '{}': {err}",
                raw.version
            )))
        })?;
        if raw.steps.is_empty() {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(
                "Refactor script must define at least one step".into(),
            )));
        }
        let engine = raw.engine.unwrap_or_else(|| "tree-sitter-service".into());
        if engine != "tree-sitter-service" {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Unsupported AST engine '{engine}' (expected tree-sitter-service)"
            ))));
        }
        let mut steps = Vec::with_capacity(raw.steps.len());
        for (idx, step) in raw.steps.into_iter().enumerate() {
            steps.push(step.into_step(idx, vars)?);
        }
        let hash = compute_script_hash(&raw_bytes);
        let metadata = ScriptMetadata {
            name: raw.name,
            version,
            engine,
            description: raw.description,
            labels: raw.labels,
            source_path: path.to_path_buf(),
            hash,
        };
        Ok(Self { metadata, steps })
    }
}

#[derive(Debug, Deserialize)]
struct RawScript {
    name: String,
    version: String,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    steps: Vec<RawStep>,
}

#[derive(Debug, Deserialize)]
struct RawStep {
    path: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    capture: Option<String>,
    #[serde(default)]
    payload: Vec<String>,
    #[serde(flatten)]
    options: BTreeMap<String, toml::Value>,
}

impl RawStep {
    fn into_step(
        self,
        id: usize,
        vars: &BTreeMap<String, String>,
    ) -> Result<ScriptStep, ApplyPatchError> {
        if self.path.trim().is_empty() {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Refactor step {id} must specify path"
            ))));
        }
        let mut options = convert_options(self.options, vars)?;
        if let Some(lang) = self.language.as_ref() {
            options.entry("lang".into()).or_insert_with(|| lang.clone());
        }
        let op_name = options
            .get("op")
            .cloned()
            .ok_or_else(|| {
                ApplyPatchError::ParseError(InvalidPatchError(format!(
                    "Refactor step {id} missing 'op' option"
                )))
            })?
            .to_lowercase();
        if self.query.is_some() && !options.contains_key("symbol") && op_requires_symbol(&op_name) {
            options.insert("symbol".into(), AUTO_SYMBOL_PLACEHOLDER.into());
        }
        if op_name.is_empty() {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Refactor step {id} missing 'op' option"
            ))));
        }
        let mut payload = self
            .payload
            .into_iter()
            .map(|line| substitute_vars(&line, vars))
            .collect::<Vec<_>>();
        if payload.is_empty() {
            payload = Vec::new();
        }
        let spec = AstOperationSpec::from_raw(&options, &payload)?;
        Ok(ScriptStep {
            id,
            path: PathBuf::from(substitute_vars(&self.path, vars)),
            language: self.language,
            query: self.query.map(|q| substitute_vars(&q, vars)),
            capture: self.capture.map(|c| substitute_vars(&c, vars)),
            operation: spec,
        })
    }
}

fn convert_options(
    options: BTreeMap<String, toml::Value>,
    vars: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ApplyPatchError> {
    options
        .into_iter()
        .map(|(key, value)| {
            let rendered = match value {
                toml::Value::String(s) => s,
                toml::Value::Integer(n) => n.to_string(),
                toml::Value::Float(f) => f.to_string(),
                toml::Value::Boolean(b) => b.to_string(),
                other => {
                    return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                        "Unsupported value for option '{key}': {other}"
                    ))));
                }
            };
            Ok((key, substitute_vars(&rendered, vars)))
        })
        .collect()
}

fn substitute_vars(value: &str, vars: &BTreeMap<String, String>) -> String {
    let mut rendered = value.to_string();
    for (key, val) in vars {
        let needle = format!("{{{{{key}}}}}");
        if rendered.contains(&needle) {
            rendered = rendered.replace(&needle, val);
        }
    }
    rendered
}

fn op_requires_symbol(op: &str) -> bool {
    matches!(
        op,
        "rename"
            | "rename-symbol"
            | "update-signature"
            | "move-block"
            | "insert-attributes"
            | "template"
            | "template-emit"
    )
}

fn parse_raw_script(
    path: &Path,
    contents: &str,
    format: ScriptFormat,
) -> Result<RawScript, ApplyPatchError> {
    match format {
        ScriptFormat::Toml => toml::from_str(contents).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Failed to parse refactor script {}: {err}",
                path.display()
            )))
        }),
        ScriptFormat::Json => serde_json::from_str(contents).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Failed to parse JSON refactor script {}: {err}",
                path.display()
            )))
        }),
        ScriptFormat::Starlark => parse_starlark_script(path, contents),
    }
}

fn parse_starlark_script(path: &Path, contents: &str) -> Result<RawScript, ApplyPatchError> {
    let mut dialect = Dialect::Extended.clone();
    dialect.enable_f_strings = true;
    let ast = AstModule::parse(
        path.to_string_lossy().as_ref(),
        contents.to_owned(),
        &dialect,
    )
    .map_err(|err| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "Failed to parse Starlark script {}: {err}",
            path.display()
        )))
    })?;
    let globals = GlobalsBuilder::standard().build();
    let module = StarlarkModule::new();
    let mut eval = Evaluator::new(&module);
    let value = eval.eval_module(ast, &globals).map_err(|err| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "Failed to evaluate Starlark script {}: {err}",
            path.display()
        )))
    })?;
    let json = value.to_json_value().map_err(|err| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "Starlark script {} produced non-serializable data: {err}",
            path.display()
        )))
    })?;
    serde_json::from_value(json).map_err(|err| {
        ApplyPatchError::ParseError(InvalidPatchError(format!(
            "Starlark script {} does not match refactor schema: {err}",
            path.display()
        )))
    })
}

#[derive(Clone, Copy, Debug)]
enum ScriptFormat {
    Toml,
    Json,
    Starlark,
}

impl ScriptFormat {
    fn resolve(path: &Path, hint: Option<&str>) -> Result<Self, ApplyPatchError> {
        if let Some(hint) = hint {
            return Self::from_hint(hint).ok_or_else(|| {
                ApplyPatchError::ParseError(InvalidPatchError(format!(
                    "Unknown script format override '{hint}'"
                )))
            });
        }
        if let Some(ext) = path.extension().and_then(|ext| ext.to_str())
            && let Some(format) = Self::from_extension(ext)
        {
            return Ok(format);
        }
        Ok(ScriptFormat::Toml)
    }

    fn from_hint(hint: &str) -> Option<Self> {
        match hint.to_ascii_lowercase().as_str() {
            "toml" => Some(ScriptFormat::Toml),
            "json" => Some(ScriptFormat::Json),
            "star" | "starlark" => Some(ScriptFormat::Starlark),
            _ => None,
        }
    }

    fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "toml" | "tml" => Some(ScriptFormat::Toml),
            "json" => Some(ScriptFormat::Json),
            "star" | "starlark" => Some(ScriptFormat::Starlark),
            _ => None,
        }
    }
}

fn compute_script_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    format!("sha256:{digest:x}")
}
