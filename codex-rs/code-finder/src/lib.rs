pub mod client;
pub mod daemon;
pub mod freeform;
pub mod index;
mod metadata;
pub mod planner;
mod project;
pub mod proto;

pub use client::ClientOptions;
pub use client::CodeFinderClient;
pub use client::DaemonSpawn;
pub use daemon::DaemonOptions;
pub use daemon::run_daemon;
pub use index::IndexCoordinator;
pub use planner::CodeFinderSearchArgs;
pub use planner::SearchPlannerError;
pub use planner::plan_search_request;
pub use proto::SearchProfile;
