//! HTTP front for [`Client`]: `POST /api/query` builds a [`Query`] from JSON, `GET /healthz` for probes.

use crate::Client;
use crate::element::{Coord, ElementKind, Response};
use crate::error::Error;
use crate::query::{Area, Bbox, DEFAULT_TIMEOUT, Query};
use axum::Router;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;

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

pub struct ApiError(StatusCode, String);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let status = match e {
            Error::RateLimited => StatusCode::SERVICE_UNAVAILABLE,
            Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self(status, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

pub fn router(client: Client) -> Router {
    Router::new()
        .route("/api/query", post(query))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(client)
}

async fn query(
    State(client): State<Client>,
    body: Result<Json<QueryRequest>, JsonRejection>,
) -> Result<Json<Response>, ApiError> {
    let Json(req) = body.map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.body_text()))?;
    let q = req
        .to_query()
        .map_err(|msg| ApiError(StatusCode::BAD_REQUEST, msg))?;
    Ok(Json(client.fetch(&q).await?))
}
