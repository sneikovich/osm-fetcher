//! Fire-and-forget reporting of served queries to the `history` service.

use crate::element::ElementKind;
use crate::server::QueryRequest;
use serde::Serialize;
use std::time::Duration;

const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Body of `POST {history}/events`.
#[derive(Debug, Serialize)]
pub struct Event {
    pub tags: Vec<String>,
    pub area_type: &'static str,
    pub coords: Option<Vec<f64>>,
    pub kind: Option<ElementKind>,
    pub status: u16,
    pub element_count: Option<usize>,
    pub duration_ms: u128,
    pub user_agent: Option<String>,
}

impl Event {
    pub fn new(
        req: &QueryRequest,
        status: u16,
        element_count: Option<usize>,
        took: Duration,
        user_agent: Option<String>,
    ) -> Self {
        let (area_type, coords) = match (req.bbox, req.around) {
            (Some(b), _) => ("bbox", Some(b.to_vec())),
            (None, Some(a)) => ("around", Some(a.to_vec())),
            (None, None) => ("none", None),
        };
        Self {
            tags: req
                .tags
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect(),
            area_type,
            coords,
            kind: req.kind,
            status,
            element_count,
            duration_ms: took.as_millis(),
            user_agent,
        }
    }
}

#[derive(Debug, Clone)]
pub struct History {
    http: reqwest::Client,
    url: String,
}

impl History {
    /// `base` is the service root, e.g. `http://history:8081`.
    pub fn new(base: &str) -> reqwest::Result<Self> {
        let http = reqwest::Client::builder().timeout(SEND_TIMEOUT).build()?;
        Ok(Self {
            http,
            url: format!("{}/events", base.trim_end_matches('/')),
        })
    }

    /// Send in the background; failures are logged and never reach the caller.
    pub fn record(&self, event: Event) {
        let (http, url) = (self.http.clone(), self.url.clone());
        tokio::spawn(async move {
            let res = http.post(&url).json(&event).send().await;
            match res.and_then(|r| r.error_for_status()) {
                Ok(_) => {}
                Err(e) => eprintln!("history: {e}"),
            }
        });
    }
}
