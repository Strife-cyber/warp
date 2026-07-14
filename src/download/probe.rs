//! Server capability probing — Range support, size discovery, mirror selection.

use anyhow::{Context, Result, bail};
use reqwest::Client;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub effective_url: String,
    pub size: u64,
    pub supports_range: bool,
}

/// Probe primary URL then optional mirrors; picks the first host that responds.
pub async fn probe_url(client: &Client, primary: &str, mirrors: &[String]) -> Result<ProbeResult> {
    let mut candidates: Vec<String> = std::iter::once(primary.to_string())
        .chain(mirrors.iter().cloned())
        .collect();

    candidates.dedup();

    let mut last_err = None;
    for url in candidates {
        match probe_single(client, &url).await {
            Ok(result) => return Ok(result),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no URLs to probe")))
}

async fn probe_single(client: &Client, url: &str) -> Result<ProbeResult> {
    let head = client.head(url).send().await.context("HEAD failed")?;

    if !head.status().is_success() && head.status().as_u16() != 206 {
        bail!("HEAD status {} for {url}", head.status());
    }

    let accept_ranges = head
        .headers()
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);

    let head_size = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let range_result = verify_range(client, url, head_size).await?;

    if accept_ranges {
        if let Some(size) = range_result {
            return Ok(ProbeResult {
                effective_url: url.to_string(),
                size,
                supports_range: true,
            });
        }
        // range_result was None despite accept_ranges — fall through to
        // the non-range fallback below (no redundant HTTP call).
    }

    // Fallback: use the cached range_result (None when accept_ranges was
    // true, or the actual value when accept_ranges was false).
    if let Some(size) = range_result {
        return Ok(ProbeResult {
            effective_url: url.to_string(),
            size,
            supports_range: false,
        });
    }

    if let Some(size) = head_size {
        return Ok(ProbeResult {
            effective_url: url.to_string(),
            size,
            supports_range: false,
        });
    }

    // Last resort: stream until Content-Length appears on GET
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        bail!("GET status {} for {url}", resp.status());
    }
    let size = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .context("could not determine file size")?;

    Ok(ProbeResult {
        effective_url: url.to_string(),
        size,
        supports_range: false,
    })
}

/// Issue a tiny Range request; parse `Content-Range: bytes */total` when present.
async fn verify_range(client: &Client, url: &str, head_size: Option<u64>) -> Result<Option<u64>> {
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await?;

    if resp.status().as_u16() == 206 {
        if let Some(total) = parse_content_range_total(resp.headers()) {
            return Ok(Some(total));
        }
        if let Some(len) = head_size {
            return Ok(Some(len));
        }
        return Ok(resp.content_length());
    }

    if resp.status().is_success() {
        return Ok(head_size.or_else(|| resp.content_length()));
    }

    Ok(None)
}

pub(crate) fn parse_content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    // bytes 0-0/12345
    let slash = value.rsplit('/').next()?;
    if slash == "*" {
        return None;
    }
    slash.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE};

    #[test]
    fn test_parse_content_range_total() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-0/12345"));
        assert_eq!(parse_content_range_total(&headers), Some(12345));
    }

    #[test]
    fn test_parse_content_range_unknown_size() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 0-0/*"));
        assert_eq!(parse_content_range_total(&headers), None);
    }

    #[test]
    fn test_parse_content_range_missing_header() {
        assert_eq!(parse_content_range_total(&HeaderMap::new()), None);
    }
}
