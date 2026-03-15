use std::thread;
use sysinfo::{System};

/// Represents system resource statistics and recommended settings.
pub struct ResourceStats {
    /// The calculated number of concurrent workers suggested for optimal performance.
    pub suggested_workers: usize,
    /// The current global CPU usage percentage (0.0 to 100.0).
    pub cpu_usage: f32,
}

/// Calculates the optimal number of workers based on current system resource availability.
///
/// This function assesses total logical CPU cores and global CPU usage to determine
/// a "multiplier" strategy:
/// - **Idle system (<40%):** 4x workers per core for maximum throughput.
/// - **Moderate load (40-70%):** 2x workers per core.
/// - **Heavy load (>70%):** 1 worker per core to maintain system stability.
///
/// Returns a [`ResourceStats`] struct containing the recommendation and current metrics.
pub fn calculate_optimal_workers() -> ResourceStats {
    let mut sys = System::new_all();
    sys.refresh_cpu_all();

    // 1. Get total logical cores (e.g., 8 or 16)
    let total_cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4); // Fallback to 4 if we can't detect

    // 2. Check overall CPU usage (0.0 to 100.0)
    let global_cpu_usage = sys.global_cpu_usage();

    // 3. Logic: If CPU is > 70% busy, we play it safe.
    // Otherwise, we use our 2x multiplier.
    let multiplier = if global_cpu_usage > 70.0 {
        1 // 1:1 ratio if the system is struggling
    } else if global_cpu_usage > 40.0 {
        2 // 2:1 ratio for a moderate load
    } else {
        4 // 4:1 ratio if the system is mostly idle (max performance)
    };

    let suggested_workers = total_cores * multiplier;

    ResourceStats {
        suggested_workers: suggested_workers.clamp(1, 32), // Never < 1, and let's cap at 32 for safety
        cpu_usage: global_cpu_usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_optimal_workers() {
        // Goal: Ensure the worker calculation returns sane values across different system loads.
        
        let stats = calculate_optimal_workers();
        
        // Verify that we always have at least one worker and don't exceed our safety cap.
        assert!(stats.suggested_workers >= 1, "Suggested workers should never be less than 1");
        assert!(stats.suggested_workers <= 32, "Suggested workers should not exceed the safety cap of 32");
        
        // Verify that CPU usage is within the valid percentage range.
        assert!(stats.cpu_usage >= 0.0 && stats.cpu_usage <= 100.0, "CPU usage must be between 0 and 100%");
    }
}
