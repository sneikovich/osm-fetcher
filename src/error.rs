use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("overpass returned {code}: {body}")]
    Status { code: u16, body: String },
    #[error("rate limited by overpass server (429)")]
    RateLimited,
    #[error("overpass server timed out (504)")]
    Timeout,
    #[error("overpass runtime error: {0}")]
    Remark(String),
    #[error("invalid response json: {0}")]
    Parse(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
