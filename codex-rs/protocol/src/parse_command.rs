use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParsedCommand {
    Read {
        cmd: String,
        name: String,
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
    Write {
        cmd: String,
        targets: Vec<String>,
        append: bool,
        line_count: Option<usize>,
    },
    Run {
        cmd: String,
        program: String,
        line_count: Option<usize>,
    },
    Unknown {
        cmd: String,
    },
}
