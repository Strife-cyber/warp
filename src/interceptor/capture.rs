use anyhow::{Result, Context};
use pcap::{Capture, Device};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Finds the default network interface
pub fn find_default_interface() -> Result<Device> {
    let interfaces = Device::list()?;
    
    // Try to find a non-loopback interface with an IPv4 address
    for interface in &interfaces {
        if interface.name != "lo" && !interface.name.starts_with("Loopback") {
            if interface.addresses.iter().any(|addr| addr.addr.is_ipv4() || addr.addr.is_ipv6()) {
                return Ok(interface.clone());
            }
        }
    }
    
    // Fallback to first available interface
    interfaces.into_iter()
        .next()
        .context("No network interface found")
}

/// Main capture loop
pub async fn capture_loop(
    interface_name: Option<String>,
    promiscuous: bool,
    snaplen: i32,
    timeout: i32,
    captured_requests: Arc<RwLock<Vec<crate::interceptor::types::CapturedRequest>>>,
    filter: Arc<RwLock<crate::interceptor::types::RequestFilter>>,
    is_running: Arc<RwLock<bool>>,
) -> Result<()> {
    use crate::interceptor::{parser::parse_packet, filter::matches_filter, signal::analyze_signal};
    
    // Find the appropriate network interface
    let interface = if let Some(name) = interface_name {
        Device::list()?
            .into_iter()
            .find(|dev| dev.name == name)
            .context(format!("Interface '{}' not found", name))?
    } else {
        find_default_interface()?
    };

    let interface_name_for_log = interface.name.clone();

    // Open the capture device
    let mut cap = Capture::from_device(interface)?
        .promisc(promiscuous)
        .snaplen(snaplen)
        .timeout(timeout)
        .open()
        .context("Failed to open capture device")?;

    // Set filter for HTTP/HTTPS traffic
    cap.filter("tcp port 80 or tcp port 443", true)?;

    println!("[INTERCEPTOR] Started capturing on interface: {}", interface_name_for_log);

    while *is_running.read().await {
        match cap.next_packet() {
            Ok(packet) => {
                if let Some(request) = parse_packet(&packet) {
                    let current_filter = filter.read().await;
                    if matches_filter(&request, &current_filter) {
                        let mut requests = captured_requests.write().await;
                        
                        // Limit buffer size
                        if requests.len() >= 10000 {
                            requests.remove(0);
                        }
                        
                        requests.push(request.clone());
                        
                        // Print captured request
                        println!("[CAPTURED] {} {} from {}:{} to {}:{}",
                            request.method.as_deref().unwrap_or("UNKNOWN"),
                            request.url.as_deref().unwrap_or("-"),
                            request.source_ip,
                            request.source_port,
                            request.destination_ip,
                            request.destination_port
                        );
                        
                        // Analyze for potential signals (e.g., HLS streams)
                        if let (Some(url), Some(ct)) = (&request.url, &request.content_type) {
                            analyze_signal(url.clone(), ct.clone());
                        }
                    }
                }
            }
            Err(pcap::Error::TimeoutExpired) => {
                // Timeout is expected, continue
            }
            Err(e) => {
                eprintln!("[INTERCEPTOR] Capture error: {}", e);
            }
        }
    }

    println!("[INTERCEPTOR] Stopped capturing");
    Ok(())
}
