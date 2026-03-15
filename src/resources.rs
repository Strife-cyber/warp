use std::thread;
use sysinfo::{System};


pub struct ResourceStats {
    pub suggested_workers: usize,
    pub cpu_usage: f32,
}

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