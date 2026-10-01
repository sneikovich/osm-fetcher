use crate::element::Response;
use crate::error::{Error, Result};
use crate::query::Query;
use reqwest::StatusCode;
use std::time::Duration;

pub const DEFAULT_ENDPOINT: &str = "https://overpass-api.de/api/interpreter";
const USER_AGENT: &str = concat!("overpass-rs/", env!("CARGO_PKG_VERSION"));
/// Overpass default server timeout is 180s; allow some slack on top.
const RAW_HTTP_TIMEOUT: Duration = Duration::from_secs(195);
const HTTP_TIMEOUT_SLACK: Duration = Duration::from_secs(15);
pub const DEFAULT_RETRIES: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Called before each retry with the error that triggered it and the delay before the next attempt.
pub type RetryHook = fn(&Error, Duration);

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    retries: u32,
    retry_base_delay: Duration,
    on_retry: Option<RetryHook>,
}

impl Client {
    pub fn new() -> Result<Self> {
        Self::with_endpoint(DEFAULT_ENDPOINT)
    }

    pub fn with_endpoint(endpoint: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self {
            http,
            endpoint: endpoint.into(),
            retries: DEFAULT_RETRIES,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            on_retry: None,
        })
    }

    /// How many times to retry on 429/504 (server busy). 0 disables retries.
    pub fn retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }

    /// Delay before the first retry; doubles on each subsequent one.
    pub fn retry_base_delay(mut self, delay: Duration) -> Self {
        self.retry_base_delay = delay;
        self
    }

    pub fn on_retry(mut self, hook: RetryHook) -> Self {
        self.on_retry = Some(hook);
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn fetch(&self, query: &Query) -> Result<Response> {
        let timeout = Duration::from_secs(query.timeout_secs().into()) + HTTP_TIMEOUT_SLACK;
        self.send(&query.to_ql(), timeout).await
    }

    /// Send a raw Overpass QL query. It must request `[out:json]`.
    pub async fn raw(&self, ql: &str) -> Result<Response> {
        self.send(ql, RAW_HTTP_TIMEOUT).await
    }

    async fn send(&self, ql: &str, timeout: Duration) -> Result<Response> {
        let mut attempt = 0;
        loop {
            match self.send_once(ql, timeout).await {
                Err(e @ (Error::RateLimited | Error::Timeout)) if attempt < self.retries => {
                    let delay = self.retry_base_delay * 2u32.pow(attempt);
                    if let Some(hook) = self.on_retry {
                        hook(&e, delay);
                    }
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                result => return result,
            }
        }
    }

    async fn send_once(&self, ql: &str, timeout: Duration) -> Result<Response> {
        let resp = self
            .http
            .post(&self.endpoint)
            .form(&[("data", ql)])
            .timeout(timeout)
            .send()
            .await?;

        match resp.status() {
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            StatusCode::GATEWAY_TIMEOUT => return Err(Error::Timeout),
            s if !s.is_success() => {
                return Err(Error::Status {
                    code: s.as_u16(),
                    body: resp.text().await.unwrap_or_default(),
                });
            }
            _ => {}
        }

        let body = resp.bytes().await?;
        let parsed: Response = serde_json::from_slice(&body)?;
        match parsed.remark {
            Some(remark) if remark.contains("error") => Err(Error::Remark(remark)),
            _ => Ok(parsed),
        }
    }
}
