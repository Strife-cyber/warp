//! HTTP download engine — workers, chunks, resume snapshots, probing, throttling.

pub mod beat;
pub mod manager;
pub mod probe;
pub mod rate_limit;
pub mod resources;
pub mod segment;

pub use manager::Manager;
pub use rate_limit::RunLimits;
pub use resources::calculate_optimal_workers;
