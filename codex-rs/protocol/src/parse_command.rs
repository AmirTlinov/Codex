use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParsedCommand {
    Navigator {
        /// Human-friendly summary (e.g. `Session (rust)`).
        summary: Option<String>,
        /// Raw query text, when available.
        query: Option<String>,
        /// Optional path/glob context.
        path: Option<String>,
        /// Applied profile names (badges) for badge rendering.
        profiles: Vec<String>,
        /// Boolean flags such as `recent`, `tests`, `with_refs`.
        flags: Vec<String>,
    },
    Read {
        cmd: String,
        name: String,
        /// (Best effort) Path to the file being read by the command. When
        /// possible, this is an absolute path, though when relative, it should
        /// be resolved against the `cwd`` that will be used to run the command
        /// to derive the absolute path.
        path: PathBuf,
    },
    ListFiles {
        cmd: String,
        path: Option<String>,
    },
    Search {
        cmd: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        cmd: String,
    },
}
