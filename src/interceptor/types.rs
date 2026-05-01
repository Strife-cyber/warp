use std::collections::HashMap;
use serde::{Serialize, Deserialize};

/// Represents a captured network request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedRequest {
    pub id: String,
    pub timestamp: u64,
    pub source_ip: String,
    pub destination_ip: String,
    pub source_port: u16,
    pub destination_port: u16,
    pub protocol: String,
    pub method: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub user_agent: Option<String>,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub headers: HashMap<String, String>,
    pub payload_size: usize,
}

/// Filter criteria for captured requests
#[derive(Debug, Clone, Default)]
pub struct RequestFilter {
    pub domain: Option<String>,
    pub method: Option<String>,
    pub content_type: Option<String>,
    pub url_regex: Option<String>,
    pub content_type_regex: Option<String>,
    pub min_size: Option<usize>,
    pub max_size: Option<usize>,
}

/// Configuration for the interceptor
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct InterceptorConfig {
    pub interface_name: Option<String>,
    pub promiscuous: bool,
    pub snaplen: i32,
    pub timeout: i32,
    #[allow(dead_code)]
    pub buffer_size: usize,
    #[allow(dead_code)]
    pub filter: Option<RequestFilter>,
}
