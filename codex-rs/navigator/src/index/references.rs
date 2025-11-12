use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::proto::NavReference;
use crate::proto::NavReferences;
use crate::proto::ReferenceRole;
use std::cmp::Ordering;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;

const MAX_REFERENCE_PREVIEW: usize = 96;

pub fn find_references(
    snapshot: &IndexSnapshot,
    project_root: &Path,
    symbol: &SymbolRecord,
    limit: usize,
) -> NavReferences {
    let key = symbol.identifier.to_lowercase();
    let Some(files) = snapshot.token_to_files.get(&key) else {
        return NavReferences::default();
    };
    let mut aggregated = NavReferences::default();
    let mut remaining = limit;
    for rel_path in files {
        if remaining == 0 {
            break;
        }
        let path = project_root.join(rel_path);
        if let Ok(chunk) = scan_file(&path, rel_path, &symbol.identifier, symbol, remaining) {
            let added = aggregated.extend_with_limit(chunk, remaining);
            remaining = remaining.saturating_sub(added);
        }
    }
    aggregated
}

fn scan_file(
    path: &Path,
    rel_path: &str,
    needle: &str,
    symbol: &SymbolRecord,
    remaining: usize,
) -> std::io::Result<NavReferences> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut chunk = NavReferences::default();
    for (idx, line) in reader.lines().enumerate() {
        if chunk.len() >= remaining {
            break;
        }
        let line = line?;
        if !line.contains(needle) {
            continue;
        }
        let line_no = (idx + 1) as u32;
        let reference = NavReference {
            path: rel_path.to_string(),
            line: line_no,
            preview: shorten_preview(&line),
            role: Some(role_for(symbol, rel_path, line_no)),
        };
        match reference.role {
            Some(ReferenceRole::Definition) => chunk.definitions.push(reference),
            _ => chunk.usages.push(reference),
        }
    }
    chunk
        .definitions
        .sort_by(|a, b| reference_order(symbol, a, b));
    chunk.usages.sort_by(|a, b| reference_order(symbol, a, b));
    Ok(chunk)
}

fn reference_order(symbol: &SymbolRecord, left: &NavReference, right: &NavReference) -> Ordering {
    reference_score(symbol, right)
        .cmp(&reference_score(symbol, left))
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.line.cmp(&right.line))
}

fn reference_score(symbol: &SymbolRecord, reference: &NavReference) -> i32 {
    let mut score = 0;
    if matches!(reference.role, Some(ReferenceRole::Definition)) {
        score += 200;
    }
    if reference.path == symbol.path {
        score += 60;
    }
    if same_directory(&reference.path, &symbol.path) {
        score += 15;
    }
    score
}

fn same_directory(left: &str, right: &str) -> bool {
    left.rsplit_once('/').map(|(dir, _)| dir) == right.rsplit_once('/').map(|(dir, _)| dir)
}

fn role_for(symbol: &SymbolRecord, rel_path: &str, line_no: u32) -> ReferenceRole {
    if rel_path == symbol.path && line_no == symbol.range.start {
        ReferenceRole::Definition
    } else {
        ReferenceRole::Usage
    }
}

fn shorten_preview(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.len() <= MAX_REFERENCE_PREVIEW {
        trimmed.to_string()
    } else {
        trimmed
            .chars()
            .take(MAX_REFERENCE_PREVIEW)
            .collect::<String>()
    }
}
