use std::sync::Arc;
use anyhow::Result;
use tokio::sync::RwLock;

use crate::interceptor::{
    types::{InterceptorConfig, CapturedRequest, RequestFilter},
    capture::capture_loop,
};

/// Main interceptor that captures and collects network requests
pub struct Interceptor {
    config: InterceptorConfig,
    captured_requests: Arc<RwLock<Vec<CapturedRequest>>>,
    filter: Arc<RwLock<RequestFilter>>,
    is_running: Arc<RwLock<bool>>,
}

impl Interceptor {
    /// Creates a new interceptor with the given configuration
    pub fn new(config: InterceptorConfig) -> Self {
        Self {
            config,
            captured_requests: Arc::new(RwLock::new(Vec::with_capacity(10000))),
            filter: Arc::new(RwLock::new(RequestFilter::default())),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Starts capturing network requests
    pub async fn start(&self) -> Result<()> {
        {
            let mut is_running = self.is_running.write().await;
            if *is_running {
                return Ok(());
            }
            *is_running = true;
        }

        let interface_name = self.config.interface_name.clone();
        let promiscuous = self.config.promiscuous;
        let snaplen = self.config.snaplen;
        let timeout = self.config.timeout;
        
        let captured_requests = Arc::clone(&self.captured_requests);
        let filter = Arc::clone(&self.filter);
        let is_running = Arc::clone(&self.is_running);

        tokio::spawn(async move {
            if let Err(e) = capture_loop(
                interface_name,
                promiscuous,
                snaplen,
                timeout,
                captured_requests,
                filter,
                is_running,
            ).await {
                eprintln!("Capture loop error: {}", e);
            }
        });

        Ok(())
    }

    /// Stops capturing network requests
    pub async fn stop(&self) -> Result<()> {
        let mut is_running = self.is_running.write().await;
        *is_running = false;
        Ok(())
    }

    /// Returns all captured requests
    pub async fn get_all_requests(&self) -> Vec<CapturedRequest> {
        self.captured_requests.read().await.clone()
    }

    /// Returns captured requests matching the filter
    pub async fn get_filtered_requests(&self, filter: RequestFilter) -> Vec<CapturedRequest> {
        let requests = self.captured_requests.read().await;
        requests.iter()
            .filter(|req| crate::interceptor::filter::matches_filter(req, &filter))
            .cloned()
            .collect()
    }

    /// Clears all captured requests
    pub async fn clear(&self) {
        self.captured_requests.write().await.clear();
    }

    /// Sets the current filter
    pub async fn set_filter(&self, filter: RequestFilter) {
        *self.filter.write().await = filter;
    }

    /// Returns the number of captured requests
    pub async fn count(&self) -> usize {
        self.captured_requests.read().await.len()
    }

    /// Returns true if the interceptor is currently running
    pub async fn is_running(&self) -> bool {
        *self.is_running.read().await
    }
}
