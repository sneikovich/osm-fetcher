//! HTTP front for [`Client`]: `POST /api/query` builds a [`Query`] from JSON, `GET /healthz` for probes.

use crate::Client;
use crate::cache::{self, Cache};
use crate::element::{Coord, ElementKind, Response};
use crate::error::Error;
use crate::history::{Event, History};
use crate::query::{Area, Bbox, DEFAULT_TIMEOUT, Query};
use axum::Router;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::header::{RETRY_AFTER, USER_AGENT};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;
use std::time::Instant;

/// Upper bound for `timeout`, matching Overpass' own default maximum.
pub const MAX_TIMEOUT: u32 = 180;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    /// `key=value` or bare `key`, same as the CLI's `--tag`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// south, west, north, east
    pub bbox: Option<[f64; 4]>,
    /// lat, lon, radius_m
    pub around: Option<[f64; 3]>,
    pub kind: Option<ElementKind>,
    pub timeout: Option<u32>,
}

impl QueryRequest {
    pub fn to_query(&self) -> Result<Query, String> {
        let tags: Vec<&str> = self
            .tags
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if tags.is_empty() {
            return Err("at least one tag is required".into());
        }
        if let Some(bad) = tags.iter().find(|t| t.starts_with('=')) {
            return Err(format!("tag {bad:?} has an empty key"));
        }
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        if !(1..=MAX_TIMEOUT).contains(&timeout) {
            return Err(format!("timeout must be within 1..={MAX_TIMEOUT}"));
        }

        let mut q = Query::new().timeout(timeout);
        if let Some(k) = self.kind {
            q = q.kind(k);
        }
        for t in tags {
            q = q.tag_expr(t);
        }
        match (self.bbox, self.around) {
            (Some(_), Some(_)) => return Err("bbox and around are mutually exclusive".into()),
            (Some([south, west, north, east]), None) => {
                q = q.within(Area::Bbox(Bbox {
                    south,
                    west,
                    north,
                    east,
                }))
            }
            (None, Some([lat, lon, radius_m])) => {
                if radius_m <= 0.0 {
                    return Err("around radius must be positive".into());
                }
                q = q.within(Area::Around {
                    center: Coord { lat, lon },
                    radius_m,
                })
            }
            (None, None) => {}
        }
        Ok(q)
    }
}

pub struct ApiError(StatusCode, String, Option<u64>);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let status = match e {
            Error::RateLimited => StatusCode::SERVICE_UNAVAILABLE,
            Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self(status, e.to_string(), None)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut resp = (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response();
        if let Some(secs) = self.2 {
            resp.headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}

#[derive(Clone)]
struct AppState {
    client: Client,
    history: Option<History>,
    cache: Option<Cache>,
}

/// `history`: optional query log; see [`crate::history`].
/// `cache`: optional Redis cache and limits; see [`crate::cache`].
pub fn router(client: Client, history: Option<History>, cache: Option<Cache>) -> Router {
    Router::new()
        .route("/api/query", post(query))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(AppState {
            client,
            history,
            cache,
        })
}

async fn query(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<QueryRequest>, JsonRejection>,
) -> Result<axum::response::Response, ApiError> {
    let Json(req) = body.map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.body_text(), None))?;
    let started = Instant::now();
    let result = run(&app, &headers, &req).await;

    if let Some(history) = &app.history {
        let (status, count) = match &result {
            Ok((resp, _)) => (StatusCode::OK, Some(resp.elements.len())),
            Err(e) => (e.0, None),
        };
        let ua = headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        history.record(Event::new(
            &req,
            status.as_u16(),
            count,
            started.elapsed(),
            ua,
        ));
    }
    result.map(|(resp, cache_status)| {
        let mut out = Json(resp).into_response();
        if let Some(status) = cache_status {
            out.headers_mut().insert(
                HeaderName::from_static("x-cache"),
                HeaderValue::from_static(status),
            );
        }
        out
    })
}

/// Client address as seen by nginx: the last `X-Forwarded-For` entry is the one nginx appended.
fn client_ip(headers: &HeaderMap) -> &str {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
}

/// Returns the response and, when a cache is configured, `HIT` or `MISS` for the `X-Cache` header.
async fn run(
    app: &AppState,
    headers: &HeaderMap,
    req: &QueryRequest,
) -> Result<(Response, Option<&'static str>), ApiError> {
    // Validate first: bad requests never touch Redis.
    req.to_query()
        .map_err(|msg| ApiError(StatusCode::BAD_REQUEST, msg, None))?;
    let Some(cache) = &app.cache else {
        let q = req.to_query().expect("validated above");
        return Ok((app.client.fetch(&q).await?, None));
    };

    let key = cache::key(req);
    if let Some(hit) = cache.get(&key).await {
        return Ok((hit, Some("HIT")));
    }
    if cache.breaker_open().await {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "overpass is busy, try again shortly".into(),
            None,
        ));
    }

    let locked = cache.try_lock(&key).await;
    if !locked && let Some(hit) = cache.wait_for(&key).await {
        return Ok((hit, Some("HIT")));
    }
    if let Some(retry_after) = cache.rate_limited(client_ip(headers)).await {
        if locked {
            cache.unlock(&key).await;
        }
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests, slow down".into(),
            Some(retry_after),
        ));
    }

    // The query goes upstream with snapped coordinates, so the cached answer matches its key.
    let q = cache::snapped(req).to_query().expect("validated above");
    let result = app.client.fetch(&q).await;
    match &result {
        Ok(resp) => cache.set(&key, resp).await,
        Err(Error::RateLimited | Error::Timeout) => cache.trip_breaker().await,
        Err(_) => {}
    }
    if locked {
        cache.unlock(&key).await;
    }
    Ok((result?, Some("MISS")))
}
