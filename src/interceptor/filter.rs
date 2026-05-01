use crate::interceptor::types::{CapturedRequest, RequestFilter};
use regex::Regex;

/// Checks if a request matches the filter
pub fn matches_filter(request: &CapturedRequest, filter: &RequestFilter) -> bool {
    if let Some(domain) = &filter.domain {
        if let Some(host) = &request.host {
            if !host.contains(domain) {
                return false;
            }
        } else {
            return false;
        }
    }

    if let Some(method) = &filter.method {
        if request.method.as_ref().map(|m| m != method).unwrap_or(true) {
            return false;
        }
    }

    if let Some(content_type) = &filter.content_type {
        if request.content_type.as_ref().map(|ct| !ct.contains(content_type)).unwrap_or(true) {
            return false;
        }
    }
    
    if let Some(regex_str) = &filter.url_regex {
        if let Ok(re) = Regex::new(regex_str) {
            if let Some(url) = &request.url {
                if !re.is_match(url) {
                    return false;
                }
            } else {
                return false;
            }
        }
    }

    if let Some(regex_str) = &filter.content_type_regex {
        if let Ok(re) = Regex::new(regex_str) {
            if let Some(ct) = &request.content_type {
                if !re.is_match(ct) {
                    return false;
                }
            } else {
                return false;
            }
        }
    }

    if let Some(min_size) = filter.min_size {
        if request.payload_size < min_size {
            return false;
        }
    }

    if let Some(max_size) = filter.max_size {
        if request.payload_size > max_size {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_request_filter() {
        let request = CapturedRequest {
            id: "1".to_string(),
            timestamp: 0,
            source_ip: "192.168.1.1".to_string(),
            destination_ip: "example.com".to_string(),
            source_port: 12345,
            destination_port: 80,
            protocol: "TCP".to_string(),
            method: Some("GET".to_string()),
            url: Some("http://example.com/video.m3u8".to_string()),
            host: Some("example.com".to_string()),
            user_agent: None,
            content_type: Some("application/vnd.apple.mpegurl".to_string()),
            content_length: None,
            headers: HashMap::new(),
            payload_size: 100,
        };

        let mut filter = RequestFilter::default();
        filter.domain = Some("example.com".to_string());
        assert!(matches_filter(&request, &filter));

        filter.method = Some("POST".to_string());
        assert!(!matches_filter(&request, &filter));
    }
}
