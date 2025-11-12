use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::OnceLock;

pub(crate) fn env_vars_lossy() -> Vec<(String, String)> {
    collect_pairs()
}

pub(crate) fn env_map_lossy() -> HashMap<String, String> {
    collect_pairs().into_iter().collect()
}

fn collect_pairs() -> Vec<(String, String)> {
    static WARNED: OnceLock<()> = OnceLock::new();
    let mut lossy_pairs = 0usize;
    let vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (convert(k, &mut lossy_pairs), convert(v, &mut lossy_pairs)))
        .collect();
    if lossy_pairs > 0 && WARNED.set(()).is_ok() {
        tracing::warn!(
            lossy_pairs,
            "Encountered non-UTF-8 environment variables; converting lossily",
        );
    }
    vars
}

fn convert(os: OsString, lossy_pairs: &mut usize) -> String {
    match os.into_string() {
        Ok(value) => value,
        Err(os) => {
            *lossy_pairs += 1;
            os.to_string_lossy().into_owned()
        }
    }
}
