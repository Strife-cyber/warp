/// Example/test file demonstrating interceptor usage without actual packet capture
/// This is useful for testing on systems without Npcap/WinPcap installed

use crate::interceptor::{types::CapturedRequest, filter::matches_filter, signal::analyze_signal};
use std::collections::HashMap;

/// Simulates capturing a request (for testing without packet capture)
pub fn simulate_capture() {
    println!("=== Simulating Network Request Capture ===\n");
    
    // Simulate various HTTP requests
    let test_requests = vec![
        CapturedRequest {
            id: "1".to_string(),
            timestamp: 0,
            source_ip: "192.168.1.100".to_string(),
            destination_ip: "142.250.185.78".to_string(),
            source_port: 54321,
            destination_port: 443,
            protocol: "TCP".to_string(),
            method: Some("GET".to_string()),
            url: Some("/video/stream.m3u8".to_string()),
            host: Some("example.com".to_string()),
            user_agent: Some("Mozilla/5.0".to_string()),
            content_type: Some("application/vnd.apple.mpegurl".to_string()),
            content_length: Some(1024),
            headers: {
                let mut h = HashMap::new();
                h.insert("host".to_string(), "example.com".to_string());
                h.insert("user-agent".to_string(), "Mozilla/5.0".to_string());
                h
            },
            payload_size: 512,
        },
        CapturedRequest {
            id: "2".to_string(),
            timestamp: 1,
            source_ip: "192.168.1.100".to_string(),
            destination_ip: "172.217.14.206".to_string(),
            source_port: 54322,
            destination_port: 80,
            protocol: "TCP".to_string(),
            method: Some("GET".to_string()),
            url: Some("/index.html".to_string()),
            host: Some("google.com".to_string()),
            user_agent: Some("Chrome/120.0".to_string()),
            content_type: Some("text/html".to_string()),
            content_length: Some(2048),
            headers: {
                let mut h = HashMap::new();
                h.insert("host".to_string(), "google.com".to_string());
                h.insert("accept".to_string(), "text/html".to_string());
                h
            },
            payload_size: 1024,
        },
        CapturedRequest {
            id: "3".to_string(),
            timestamp: 2,
            source_ip: "192.168.1.100".to_string(),
            destination_ip: "151.101.1.140".to_string(),
            source_port: 54323,
            destination_port: 443,
            protocol: "TCP".to_string(),
            method: Some("POST".to_string()),
            url: Some("/api/data".to_string()),
            host: Some("api.example.com".to_string()),
            user_agent: Some("curl/7.68.0".to_string()),
            content_type: Some("application/json".to_string()),
            content_length: Some(512),
            headers: {
                let mut h = HashMap::new();
                h.insert("host".to_string(), "api.example.com".to_string());
                h.insert("content-type".to_string(), "application/json".to_string());
                h
            },
            payload_size: 256,
        },
    ];

    // Process each simulated request
    for request in &test_requests {
        println!("[CAPTURED] {} {} from {}:{} to {}:{}",
            request.method.as_deref().unwrap_or("UNKNOWN"),
            request.url.as_deref().unwrap_or("-"),
            request.source_ip,
            request.source_port,
            request.destination_ip,
            request.destination_port
        );

        // Test signal analysis
        if let (Some(url), Some(ct)) = (&request.url, &request.content_type) {
            analyze_signal(url.clone(), ct.clone());
        }
    }

    println!("\n=== Testing Filter ===");
    
    // Test filtering by domain
    let filter = crate::interceptor::types::RequestFilter {
        domain: Some("example.com".to_string()),
        ..Default::default()
    };
    
    let filtered: Vec<_> = test_requests.iter()
        .filter(|r| matches_filter(r, &filter))
        .collect();
    
    println!("Requests matching domain 'example.com': {}", filtered.len());
    for req in filtered {
        println!("  - {} {}", req.method.as_deref().unwrap_or("-"), req.url.as_deref().unwrap_or("-"));
    }

    println!("\n=== Summary ===");
    println!("Total simulated requests: {}", test_requests.len());
    println!("Test completed successfully!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulate_capture() {
        simulate_capture();
    }
}
