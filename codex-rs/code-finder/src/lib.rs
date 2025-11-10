pub mod client;
pub mod daemon;
pub mod freeform;
pub mod index;
mod metadata;
mod project;
pub mod proto;

pub use client::ClientOptions;
pub use client::CodeFinderClient;
pub use client::DaemonSpawn;
pub use daemon::DaemonOptions;
pub use daemon::run_daemon;
pub use index::IndexCoordinator;
