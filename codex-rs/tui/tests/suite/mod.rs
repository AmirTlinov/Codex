// Aggregates all former standalone integration tests as modules.
#[cfg(feature = "vt100-tests")]
mod code_finder_history;
mod status_indicator;
mod vt100_history;
mod vt100_live_commit;
