/// Analyzes a request for specific signals (e.g., HLS streams)
pub fn analyze_signal(url: String, ct: String) {
    // Check for HLS (HTTP Live Streaming) indicators
    let is_hls = url.contains(".m3u8") 
        || url.contains(".m3u") 
        || ct.contains("application/vnd.apple.mpegurl")
        || ct.contains("application/x-mpegurl")
        || ct.contains("audio/mpegurl");

    if is_hls {
        println!("[SIGNAL] Potential HLS found: {} (Type: {})", url, ct);
    }

    // Check for other streaming formats
    let is_stream = url.contains(".mpd") 
        || url.contains(".f4m") 
        || ct.contains("application/dash+xml")
        || ct.contains("video/mp2t")
        || ct.contains("application/octet-stream");

    if is_stream && !is_hls {
        println!("[SIGNAL] Potential stream found: {} (Type: {})", url, ct);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_signal() {
        analyze_signal("http://example.com/video.m3u8".to_string(), "application/vnd.apple.mpegurl".to_string());
        analyze_signal("http://example.com/video.mp4".to_string(), "video/mp4".to_string());
    }
}
