use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::proto::NavReference;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;

pub fn find_references(
    snapshot: &IndexSnapshot,
    project_root: &Path,
    symbol: &SymbolRecord,
    limit: usize,
) -> Vec<NavReference> {
    let key = symbol.identifier.to_lowercase();
    let mut refs = Vec::new();
    let Some(files) = snapshot.token_to_files.get(&key) else {
        return refs;
    };
    for rel_path in files {
        if refs.len() >= limit {
            break;
        }
        let path = project_root.join(rel_path);
        if let Ok(mut hits) = scan_file(&path, rel_path, &symbol.identifier, limit - refs.len()) {
            refs.append(&mut hits);
        }
    }
    refs.truncate(limit);
    refs
}

fn scan_file(
    path: &Path,
    rel_path: &str,
    needle: &str,
    remaining: usize,
) -> std::io::Result<Vec<NavReference>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut refs = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        if refs.len() >= remaining {
            break;
        }
        let line = line?;
        if !line.contains(needle) {
            continue;
        }
        refs.push(NavReference {
            path: rel_path.to_string(),
            line: (idx + 1) as u32,
            preview: line.trim().to_string(),
        });
    }
    Ok(refs)
}
