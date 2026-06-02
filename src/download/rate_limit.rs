//! Token-bucket rate limiters shared across workers (accurate per-download + global caps).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Per-run throttle configuration shared across workers.
#[derive(Clone, Default)]
pub struct RunLimits {
    pub global: Option<Arc<RateLimiter>>,
    pub local: Option<Arc<RateLimiter>>,
}

/// Async-friendly token bucket — one instance per download + optional global governor.
pub struct RateLimiter {
    bytes_per_sec: u64,
    state: Mutex<BucketState>,
}

struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec: bytes_per_sec.max(1),
            state: Mutex::new(BucketState {
                tokens: bytes_per_sec as f64,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Block until `bytes` may be transferred.
    pub async fn acquire(&self, bytes: u64) {
        loop {
            let wait = {
                let mut st = self.state.lock();
                let now = Instant::now();
                let elapsed = now.duration_since(st.last_refill).as_secs_f64();
                st.tokens = (st.tokens + elapsed * self.bytes_per_sec as f64)
                    .min(self.bytes_per_sec as f64 * 2.0);
                st.last_refill = now;

                if st.tokens >= bytes as f64 {
                    st.tokens -= bytes as f64;
                    None
                } else {
                    let deficit = bytes as f64 - st.tokens;
                    Some(Duration::from_secs_f64(deficit / self.bytes_per_sec as f64))
                }
            };

            match wait {
                None => return,
                Some(d) if d > Duration::from_millis(5) => tokio::time::sleep(d).await,
                Some(_) => tokio::task::yield_now().await,
            }
        }
    }
}

/// Compose per-download and global limiters — slowest wins.
pub async fn acquire_composed(
    global: Option<&Arc<RateLimiter>>,
    local: Option<&Arc<RateLimiter>>,
    bytes: u64,
) {
    if let Some(g) = global {
        g.acquire(bytes).await;
    }
    if let Some(l) = local {
        l.acquire(bytes).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn test_rate_limiter_high_cap_is_fast() {
        let limiter = RateLimiter::new(10_000_000);
        let start = Instant::now();
        limiter.acquire(4096).await;
        limiter.acquire(4096).await;
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn test_rate_limiter_throttles_large_transfer() {
        let limiter = RateLimiter::new(4096);
        limiter.acquire(4096).await;
        let start = Instant::now();
        limiter.acquire(8192).await;
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[tokio::test]
    async fn test_acquire_composed_applies_both() {
        let global = Arc::new(RateLimiter::new(2048));
        let local = Arc::new(RateLimiter::new(2048));
        global.acquire(2048).await;
        let start = Instant::now();
        acquire_composed(Some(&global), Some(&local), 2048).await;
        assert!(start.elapsed() >= Duration::from_millis(200));
    }
}
