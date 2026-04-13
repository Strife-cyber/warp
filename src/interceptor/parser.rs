#[cfg(feature = "capture")]
use std::collections::HashMap;
#[cfg(feature = "capture")]
use std::net::IpAddr;
#[cfg(feature = "capture")]
use pnet::packet::ethernet::{EthernetPacket, EtherTypes};
#[cfg(feature = "capture")]
use pnet::packet::ipv4::Ipv4Packet;
#[cfg(feature = "capture")]
use pnet::packet::ipv6::Ipv6Packet;
#[cfg(feature = "capture")]
use pnet::packet::tcp::TcpPacket;
#[cfg(feature = "capture")]
use pnet::packet::udp::UdpPacket;
#[cfg(feature = "capture")]
use pnet::packet::Packet;
#[cfg(feature = "capture")]
use crate::interceptor::types::CapturedRequest;

#[cfg(feature = "capture")]
/// Intermediate HTTP request structure
#[derive(Debug)]
struct HttpRequest {
    method: Option<String>,
    url: Option<String>,
    host: Option<String>,
    user_agent: Option<String>,
    content_type: Option<String>,
    content_length: Option<u64>,
    headers: HashMap<String, String>,
}

#[cfg(feature = "capture")]
/// Parses a raw packet into a CapturedRequest
pub fn parse_packet(packet: &pcap::Packet) -> Option<CapturedRequest> {
    let data = packet.data;
    
    // Parse Ethernet frame
    let ethernet = EthernetPacket::new(data)?;
    
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    
    match ethernet.get_ethertype() {
        EtherTypes::Ipv4 => {
            let ipv4 = Ipv4Packet::new(ethernet.packet())?;
            parse_ipv4_packet(&ipv4, timestamp)
        }
        EtherTypes::Ipv6 => {
            let ipv6 = Ipv6Packet::new(ethernet.packet())?;
            parse_ipv6_packet(&ipv6, timestamp)
        }
        _ => None,
    }
}

#[cfg(feature = "capture")]
/// Parses an IPv4 packet
fn parse_ipv4_packet(ipv4: &Ipv4Packet, timestamp: u64) -> Option<CapturedRequest> {
    match ipv4.get_next_level_protocol() {
        pnet::packet::ip::IpNextHeaderProtocols::Tcp => {
            let tcp = TcpPacket::new(ipv4.packet())?;
            parse_tcp_packet(
                IpAddr::V4(ipv4.get_source()),
                IpAddr::V4(ipv4.get_destination()),
                &tcp,
                timestamp,
            )
        }
        pnet::packet::ip::IpNextHeaderProtocols::Udp => {
            let udp = UdpPacket::new(ipv4.packet())?;
            parse_udp_packet(
                IpAddr::V4(ipv4.get_source()),
                IpAddr::V4(ipv4.get_destination()),
                &udp,
                timestamp,
            )
        }
        _ => None,
    }
}

#[cfg(feature = "capture")]
/// Parses an IPv6 packet
fn parse_ipv6_packet(ipv6: &Ipv6Packet, timestamp: u64) -> Option<CapturedRequest> {
    match ipv6.get_next_header() {
        pnet::packet::ip::IpNextHeaderProtocols::Tcp => {
            let tcp = TcpPacket::new(ipv6.packet())?;
            parse_tcp_packet(
                IpAddr::V6(ipv6.get_source()),
                IpAddr::V6(ipv6.get_destination()),
                &tcp,
                timestamp,
            )
        }
        pnet::packet::ip::IpNextHeaderProtocols::Udp => {
            let udp = UdpPacket::new(ipv6.packet())?;
            parse_udp_packet(
                IpAddr::V6(ipv6.get_source()),
                IpAddr::V6(ipv6.get_destination()),
                &udp,
                timestamp,
            )
        }
        _ => None,
    }
}

#[cfg(feature = "capture")]
/// Parses a TCP packet for HTTP data
fn parse_tcp_packet(
    source_ip: IpAddr,
    dest_ip: IpAddr,
    tcp: &TcpPacket,
    timestamp: u64,
) -> Option<CapturedRequest> {
    let payload = tcp.payload();
    if payload.is_empty() {
        return None;
    }

    // Try to parse as HTTP
    if let Ok(http_str) = std::str::from_utf8(payload) {
        if let Some(request) = parse_http_request(http_str) {
            return Some(CapturedRequest {
                id: generate_id(),
                timestamp,
                source_ip: source_ip.to_string(),
                destination_ip: dest_ip.to_string(),
                source_port: tcp.get_source(),
                destination_port: tcp.get_destination(),
                protocol: "TCP".to_string(),
                method: request.method,
                url: request.url,
                host: request.host,
                user_agent: request.user_agent,
                content_type: request.content_type,
                content_length: request.content_length,
                headers: request.headers,
                payload_size: payload.len(),
            });
        }
    }

    // Return basic TCP info if HTTP parsing fails
    Some(CapturedRequest {
        id: generate_id(),
        timestamp,
        source_ip: source_ip.to_string(),
        destination_ip: dest_ip.to_string(),
        source_port: tcp.get_source(),
        destination_port: tcp.get_destination(),
        protocol: "TCP".to_string(),
        method: None,
        url: None,
        host: None,
        user_agent: None,
        content_type: None,
        content_length: None,
        headers: HashMap::new(),
        payload_size: payload.len(),
    })
}

#[cfg(feature = "capture")]
/// Parses a UDP packet
fn parse_udp_packet(
    source_ip: IpAddr,
    dest_ip: IpAddr,
    udp: &UdpPacket,
    timestamp: u64,
) -> Option<CapturedRequest> {
    let payload = udp.payload();
    
    Some(CapturedRequest {
        id: generate_id(),
        timestamp,
        source_ip: source_ip.to_string(),
        destination_ip: dest_ip.to_string(),
        source_port: udp.get_source(),
        destination_port: udp.get_destination(),
        protocol: "UDP".to_string(),
        method: None,
        url: None,
        host: None,
        user_agent: None,
        content_type: None,
        content_length: None,
        headers: HashMap::new(),
        payload_size: payload.len(),
    })
}

#[cfg(feature = "capture")]
/// Parses an HTTP request string
fn parse_http_request(http_str: &str) -> Option<HttpRequest> {
    let lines: Vec<&str> = http_str.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Parse request line
    let request_line: Vec<&str> = lines[0].split_whitespace().collect();
    if request_line.len() < 2 {
        return None;
    }

    let method = request_line[0].to_string();
    let url = request_line[1].to_string();

    // Parse headers
    let mut headers = HashMap::new();
    let mut host = None;
    let mut user_agent = None;
    let mut content_type = None;
    let mut content_length = None;

    for line in lines.iter().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            
            match key.as_str() {
                "host" => host = Some(value.clone()),
                "user-agent" => user_agent = Some(value.clone()),
                "content-type" => content_type = Some(value.clone()),
                "content-length" => content_length = value.parse().ok(),
                _ => {}
            }
            
            headers.insert(key, value);
        }
    }

    Some(HttpRequest {
        method: Some(method),
        url: Some(url),
        host,
        user_agent,
        content_type,
        content_length,
        headers,
    })
}

#[cfg(feature = "capture")]
/// Generates a unique ID for a request
fn generate_id() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

#[cfg(all(test, feature = "capture"))]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_request() {
        let http = "GET /video/stream.m3u8 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: Test\r\n\r\n";
        let parsed = parse_http_request(http);
        
        assert!(parsed.is_some());
        let req = parsed.unwrap();
        assert_eq!(req.method, Some("GET".to_string()));
        assert_eq!(req.url, Some("/video/stream.m3u8".to_string()));
        assert_eq!(req.host, Some("example.com".to_string()));
    }
}
