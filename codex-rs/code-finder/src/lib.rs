pub mod client;
pub mod daemon;
pub mod freeform;
pub mod index;
mod metadata;
pub mod planner;
mod project;
pub mod proto;
pub mod summary;

pub use client::ClientOptions;
pub use client::CodeFinderClient;
pub use client::DaemonSpawn;
pub use client::resolve_daemon_launcher;
pub use daemon::DaemonOptions;
pub use daemon::run_daemon;
pub use index::IndexCoordinator;
pub use planner::CodeFinderSearchArgs;
pub use planner::SearchPlannerError;
pub use planner::plan_search_request;
pub use proto::SearchProfile;
pub use summary::collect_flags as code_finder_flags;
pub use summary::profile_badges;
pub use summary::summarize_args;

pub const CODE_FINDER_TOOL_INSTRUCTIONS: &str = include_str!("../code_finder_tool_instructions.md");
