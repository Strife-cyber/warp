// Interceptor module - Network request capture and analysis
// 
// This module provides functionality to intercept and collect network requests
// made on the machine, with support for filtering and signal analysis (e.g., HLS streams).

pub mod types;
pub mod filter;
pub mod signal;
pub mod example;
pub mod npcap_check;

#[cfg(feature = "capture")]
pub mod parser;

#[cfg(feature = "capture")]
pub mod capture;

#[cfg(feature = "capture")]
pub mod interceptor;

// Re-export commonly used types for convenience
pub use types::CapturedRequest;

#[cfg(feature = "capture")]
pub use interceptor::Interceptor;

#[allow(dead_code)]
/// Handles a request (can be called externally to manually add requests)
pub async fn handle_request(req: CapturedRequest) -> anyhow::Result<()> {
    println!("[REQUEST] {} {} from {}:{} to {}:{}",
        req.method.unwrap_or_else(|| "UNKNOWN".to_string()),
        req.url.unwrap_or_else(|| "-".to_string()),
        req.source_ip,
        req.source_port,
        req.destination_ip,
        req.destination_port
    );
    Ok(())
}
