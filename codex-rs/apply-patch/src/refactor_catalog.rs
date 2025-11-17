use crate::ApplyPatchError;
use crate::parser::ParseError::InvalidPatchError;
use crate::refactor_script::ScriptMetadata;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub struct ScriptCatalog {
    entries: HashMap<String, CatalogEntry>,
    ordered: Vec<CatalogEntry>,
}

impl ScriptCatalog {
    pub fn load(root: &Path) -> Result<Option<Self>, ApplyPatchError> {
        let path = root.join("refactors/catalog.json");
        let contents = match fs::read_to_string(&path) {
            Ok(value) => value,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                    "Failed to read script catalog {}: {err}",
                    path.display()
                ))));
            }
        };
        let doc: CatalogDocument = serde_json::from_str(&contents).map_err(|err| {
            ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Failed to parse script catalog {}: {err}",
                path.display()
            )))
        })?;
        let mut entries = HashMap::with_capacity(doc.scripts.len());
        let mut ordered = Vec::with_capacity(doc.scripts.len());
        for entry in doc.scripts {
            entries.insert(normalize(&entry.path), entry.clone());
            ordered.push(entry);
        }
        Ok(Some(Self { entries, ordered }))
    }

    pub fn verify(
        &self,
        rel_path: &Path,
        metadata: &ScriptMetadata,
    ) -> Result<(), ApplyPatchError> {
        let key = normalize(rel_path.to_string_lossy().as_ref());
        let Some(entry) = self.entries.get(&key) else {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Script {key} is not listed in refactors/catalog.json (hash {}, version {})",
                metadata.hash, metadata.version
            ))));
        };
        if entry.hash != metadata.hash {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Script {key} hash mismatch: catalog {} vs actual {}",
                entry.hash, metadata.hash
            ))));
        }
        if entry.version != metadata.version.to_string() {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Script {key} version mismatch: catalog {} vs actual {}",
                entry.version, metadata.version
            ))));
        }
        if let Some(expected_name) = entry.name.as_deref()
            && expected_name != metadata.name
        {
            return Err(ApplyPatchError::ParseError(InvalidPatchError(format!(
                "Script {key} registered as '{expected_name}' but metadata declares '{}'",
                metadata.name
            ))));
        }
        Ok(())
    }

    pub fn entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.ordered.iter()
    }
}

#[derive(Debug, Deserialize)]
struct CatalogDocument {
    #[serde(default)]
    _version: u32,
    #[serde(default)]
    scripts: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogEntry {
    pub path: String,
    pub hash: String,
    pub version: String,
    #[serde(default)]
    pub name: Option<String>,
}

fn normalize(value: &str) -> String {
    value.replace('\\', "/")
}
