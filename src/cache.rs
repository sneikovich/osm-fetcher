//! Redis-backed response cache, per-IP rate limit and upstream circuit breaker.
//!
//! Everything fails open: if Redis is down or slow the fetcher behaves as if
//! this module was not configured, and the error only goes to stderr.

use crate::element::Response;
use crate::server::QueryRequest;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncTypedCommands, Client};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

const KEY_PREFIX: &str = "ovp:v1:";
const BREAKER_KEY: &str = "ovp:breaker";
const LOCK_TTL_SECS: u64 = 30;
const RATE_WINDOW_SECS: u64 = 60;
const WAIT_STEP: Duration = Duration::from_millis(200);
const WAIT_MAX: Duration = Duration::from_secs(25);
const REDIS_TIMEOUT: Duration = Duration::from_millis(500);
/// Coordinates are snapped to this many decimals (4 ≈ 11 m of latitude).
const COORD_DECIMALS: i32 = 4;

#[derive(Debug, Clone)]
pub struct Config {
    /// How long a cached response stays valid.
    pub ttl_secs: u64,
    /// Upstream-bound requests per minute per IP; 0 disables the limit.
    pub rate_limit: u32,
    /// How long all requests are refused after Overpass reports 429/504.
    pub breaker_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ttl_secs: 600,
            rate_limit: 30,
            breaker_secs: 30,
        }
    }
}

#[derive(Clone)]
pub struct Cache {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    // Connected on first use so Redis may come up after the fetcher.
    conn: OnceCell<ConnectionManager>,
    cfg: Config,
}

impl Cache {
    /// `url` is e.g. `redis://redis:6379`. Does not connect yet.
    pub fn new(url: &str, cfg: Config) -> redis::RedisResult<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                client: Client::open(url)?,
                conn: OnceCell::new(),
                cfg,
            }),
        })
    }

    async fn conn(&self) -> Option<ConnectionManager> {
        let init = self.inner.conn.get_or_try_init(|| {
            let cfg = ConnectionManagerConfig::new()
                .set_number_of_retries(0)
                .set_connection_timeout(Some(REDIS_TIMEOUT))
                .set_response_timeout(Some(REDIS_TIMEOUT));
            ConnectionManager::new_with_config(self.inner.client.clone(), cfg)
        });
        match init.await {
            Ok(c) => Some(c.clone()),
            Err(e) => {
                eprintln!("cache: {e}");
                None
            }
        }
    }

    pub async fn get(&self, key: &str) -> Option<Response> {
        let mut c = self.conn().await?;
        match c.get(key).await {
            Ok(Some(raw)) => match serde_json::from_str(&raw) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("cache: bad entry {key}: {e}");
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                eprintln!("cache: {e}");
                None
            }
        }
    }

    /// Store a successful response. Responses carrying an Overpass `remark`
    /// (runtime error, possibly partial data) are not cached.
    pub async fn set(&self, key: &str, resp: &Response) {
        if resp.remark.is_some() {
            return;
        }
        let Some(mut c) = self.conn().await else {
            return;
        };
        let Ok(raw) = serde_json::to_string(resp) else {
            return;
        };
        if let Err(e) = c.set_ex(key, raw, self.inner.cfg.ttl_secs).await {
            eprintln!("cache: {e}");
        }
    }

    pub async fn breaker_open(&self) -> bool {
        let Some(mut c) = self.conn().await else {
            return false;
        };
        c.exists(BREAKER_KEY).await.unwrap_or_else(|e| {
            eprintln!("cache: {e}");
            false
        })
    }

    pub async fn trip_breaker(&self) {
        let Some(mut c) = self.conn().await else {
            return;
        };
        if let Err(e) = c.set_ex(BREAKER_KEY, 1, self.inner.cfg.breaker_secs).await {
            eprintln!("cache: {e}");
        }
    }

    /// Count one upstream-bound request from `ip`. `Some(retry_after_secs)` if over the limit.
    pub async fn rate_limited(&self, ip: &str) -> Option<u64> {
        let limit = self.inner.cfg.rate_limit;
        if limit == 0 {
            return None;
        }
        let mut c = self.conn().await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let key = format!("ovp:rl:{ip}:{}", now / RATE_WINDOW_SECS);
        let res: redis::RedisResult<(u64, bool)> = redis::pipe()
            .atomic()
            .incr(&key, 1)
            .expire(&key, RATE_WINDOW_SECS as i64)
            .query_async(&mut c)
            .await;
        match res {
            Ok((n, _)) if n > u64::from(limit) => Some(RATE_WINDOW_SECS - now % RATE_WINDOW_SECS),
            Ok(_) => None,
            Err(e) => {
                eprintln!("cache: {e}");
                None
            }
        }
    }

    /// Single-flight: `true` if the caller now owns the right to fetch `key`.
    pub async fn try_lock(&self, key: &str) -> bool {
        let Some(mut c) = self.conn().await else {
            return true;
        };
        let opts = redis::SetOptions::default()
            .conditional_set(redis::ExistenceCheck::NX)
            .with_expiration(redis::SetExpiry::EX(LOCK_TTL_SECS));
        match c.set_options(lock_key(key), 1, opts).await {
            Ok(v) => v.is_some(),
            Err(e) => {
                eprintln!("cache: {e}");
                true
            }
        }
    }

    pub async fn unlock(&self, key: &str) {
        if let Some(mut c) = self.conn().await {
            if let Err(e) = c.del(lock_key(key)).await {
                eprintln!("cache: {e}");
            }
        }
    }

    /// Wait for whoever holds the lock on `key` to fill the cache.
    pub async fn wait_for(&self, key: &str) -> Option<Response> {
        let deadline = tokio::time::Instant::now() + WAIT_MAX;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(WAIT_STEP).await;
            if let Some(r) = self.get(key).await {
                return Some(r);
            }
        }
        None
    }
}

fn lock_key(key: &str) -> String {
    format!("{key}:lock")
}

fn snap(v: f64) -> f64 {
    let m = 10f64.powi(COORD_DECIMALS);
    (v * m).round() / m
}

/// Cache key of a request: tags are trimmed, sorted and deduplicated, coordinates
/// snapped to ~11 m, the radius kept exact, `timeout` ignored.
pub fn key(req: &QueryRequest) -> String {
    let mut tags: Vec<&str> = req
        .tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    tags.sort_unstable();
    tags.dedup();

    let mut canon = String::new();
    for t in tags {
        let _ = writeln!(canon, "t={t}");
    }
    if let Some(k) = req.kind {
        let _ = writeln!(canon, "k={k:?}");
    }
    if let Some([s, w, n, e]) = req.bbox {
        let _ = writeln!(
            canon,
            "bbox={},{},{},{}",
            snap(s),
            snap(w),
            snap(n),
            snap(e)
        );
    }
    if let Some([lat, lon, r]) = req.around {
        let _ = writeln!(canon, "around={},{},{r}", snap(lat), snap(lon));
    }
    let digest = Sha256::digest(canon.as_bytes());
    let mut out = String::from(KEY_PREFIX);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The request as it is actually sent upstream: coordinates snapped like in [`key`],
/// so a cached answer is exactly the answer for its key.
pub fn snapped(req: &QueryRequest) -> QueryRequest {
    QueryRequest {
        tags: req.tags.clone(),
        bbox: req.bbox.map(|b| b.map(snap)),
        around: req.around.map(|[lat, lon, r]| [snap(lat), snap(lon), r]),
        kind: req.kind,
        timeout: req.timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;

    fn req(tags: &[&str], around: Option<[f64; 3]>) -> QueryRequest {
        QueryRequest {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            bbox: None,
            around,
            kind: None,
            timeout: None,
        }
    }

    #[test]
    fn tag_order_whitespace_and_timeout_do_not_matter() {
        let a = req(&["amenity=cafe", "name"], Some([50.4501, 30.5234, 300.0]));
        let mut b = req(
            &[" name ", "amenity=cafe", "name", ""],
            Some([50.4501, 30.5234, 300.0]),
        );
        b.timeout = Some(60);
        assert_eq!(key(&a), key(&b));
    }

    #[test]
    fn noise_below_the_grid_step_maps_to_one_key() {
        let a = req(&["a=b"], Some([50.45012, 30.52341, 300.0]));
        let b = req(&["a=b"], Some([50.45014, 30.52338, 300.0]));
        assert_eq!(key(&a), key(&b));
        assert_eq!(snapped(&a).around, snapped(&b).around);
    }

    #[test]
    fn different_queries_get_different_keys() {
        let base = req(&["a=b"], Some([50.4501, 30.5234, 300.0]));
        let mut kind = req(&["a=b"], Some([50.4501, 30.5234, 300.0]));
        kind.kind = Some(ElementKind::Way);
        assert_ne!(key(&base), key(&kind));
        assert_ne!(key(&base), key(&req(&["a=c"], base.around)));
        assert_ne!(
            key(&base),
            key(&req(&["a=b"], Some([50.4501, 30.5234, 301.0])))
        );
        assert_ne!(
            key(&base),
            key(&req(&["a=b"], Some([50.4503, 30.5234, 300.0])))
        );
    }
}
