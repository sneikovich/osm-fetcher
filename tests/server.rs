use overpass::Client;
use overpass::cache::{Cache, Config as CacheConfig};
use overpass::history::History;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start the API against `upstream` on a random port; returns its base URL.
async fn spawn_api(upstream: &MockServer) -> String {
    spawn_api_with_history(upstream, None).await
}

async fn spawn_api_with_history(upstream: &MockServer, history: Option<&str>) -> String {
    spawn_api_full(upstream, history, None).await
}

async fn spawn_api_full(
    upstream: &MockServer,
    history: Option<&str>,
    cache: Option<Cache>,
) -> String {
    let client = Client::with_endpoint(upstream.uri()).unwrap().retries(0);
    let history = history.map(|url| History::new(url));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(listener, overpass::server::router(client, history, cache)).into_future(),
    );
    format!("http://{addr}")
}

async fn post(base: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/query"))
        .json(&body)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

#[tokio::test]
async fn query_ok_builds_ql_and_returns_elements() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        // node["amenity"="cafe"](around:300,50.45,30.52)
        .and(body_string_contains(
            "node%5B%22amenity%22%3D%22cafe%22%5D%28around%3A300%2C50.45%2C30.52%29",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"elements":[{"type":"node","id":1,"lat":50.45,"lon":30.52,"tags":{"name":"X"}}]}"#,
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    let base = spawn_api(&upstream).await;

    let (status, body) = post(
        &base,
        json!({"tags": ["amenity=cafe"], "around": [50.45, 30.52, 300], "kind": "node"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["elements"][0]["tags"]["name"], "X");
}

#[tokio::test]
async fn invalid_requests_are_400() {
    let upstream = MockServer::start().await;
    let base = spawn_api(&upstream).await;

    for body in [
        json!({"tags": []}),
        json!({"tags": ["  "]}),
        json!({"tags": ["a"], "bbox": [1, 2, 3, 4], "around": [1, 2, 3]}),
        json!({"tags": ["a"], "timeout": 0}),
        json!({"tags": ["a"], "timeout": 1000}),
        json!({"tags": ["a"], "kind": "area"}),
        json!({"tags": ["a"], "endpoint": "http://evil"}),
    ] {
        let (status, resp) = post(&base, body.clone()).await;
        assert_eq!(status, 400, "{body}");
        assert!(resp["error"].is_string(), "{body}");
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn upstream_errors_are_mapped() {
    for (upstream_status, expected) in [(429, 503), (504, 504), (400, 502)] {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(upstream_status))
            .mount(&upstream)
            .await;
        let base = spawn_api(&upstream).await;

        let (status, resp) = post(&base, json!({"tags": ["a"]})).await;
        assert_eq!(status, expected);
        assert!(resp["error"].is_string());
    }
}

#[tokio::test]
async fn healthz() {
    let upstream = MockServer::start().await;
    let base = spawn_api(&upstream).await;
    let resp = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn broker_down_does_not_affect_response() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"elements":[]}"#))
        .mount(&upstream)
        .await;
    // Reserve a port, then free it so nothing listens there.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_url = format!("amqp://app:app@{}/%2f", dead.local_addr().unwrap());
    drop(dead);
    let base = spawn_api_with_history(&upstream, Some(&dead_url)).await;

    let (status, body) = post(&base, json!({"tags": ["a"]})).await;
    assert_eq!(status, 200);
    assert_eq!(body["elements"], json!([]));
}

fn cafe_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string(r#"{"elements":[{"type":"node","id":1,"lat":1.0,"lon":2.0}]}"#)
}

async fn post_raw(base: &str, body: Value, ip: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/query"))
        .header("x-forwarded-for", ip)
        .json(&body)
        .send()
        .await
        .unwrap()
}

fn x_cache(resp: &reqwest::Response) -> Option<&str> {
    resp.headers().get("x-cache").and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn redis_down_does_not_affect_response() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cafe_ok())
        .mount(&upstream)
        .await;
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("redis://{}", dead.local_addr().unwrap());
    drop(dead);
    let cache = Cache::new(&url, CacheConfig::default()).unwrap();
    let base = spawn_api_full(&upstream, None, Some(cache)).await;

    let resp = post_raw(&base, json!({"tags": ["a"]}), "1.1.1.1").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(x_cache(&resp), Some("MISS"));
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
}

/// Needs `REDIS_URL`; each test uses its own key prefix via unique tags/IPs, but
/// the breaker key is global, so run with `--test-threads=1`.
fn redis_url() -> String {
    std::env::var("REDIS_URL").expect("REDIS_URL")
}

fn unique(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{n}")
}

async fn flush() {
    let c = redis::Client::open(redis_url()).unwrap();
    let mut conn = c.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();
}

#[tokio::test]
#[ignore = "needs REDIS_URL"]
async fn second_nearby_query_is_served_from_cache() {
    flush().await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cafe_ok())
        .mount(&upstream)
        .await;
    let cache = Cache::new(&redis_url(), CacheConfig::default()).unwrap();
    let base = spawn_api_full(&upstream, None, Some(cache)).await;

    let tag = unique("amenity=cafe");
    let a = post_raw(
        &base,
        json!({"tags": [tag], "around": [50.45012, 30.52341, 300]}),
        "1.1.1.1",
    )
    .await;
    assert_eq!(x_cache(&a), Some("MISS"));
    let b = post_raw(
        &base,
        json!({"tags": [tag], "around": [50.45014, 30.52338, 300]}),
        "1.1.1.1",
    )
    .await;
    assert_eq!(b.status(), 200);
    assert_eq!(x_cache(&b), Some("HIT"));
    let body: Value = b.json().await.unwrap();
    assert_eq!(body["elements"].as_array().unwrap().len(), 1);
    // different radius → different key
    let c = post_raw(
        &base,
        json!({"tags": [tag], "around": [50.45012, 30.52341, 301]}),
        "1.1.1.1",
    )
    .await;
    assert_eq!(x_cache(&c), Some("MISS"));
    assert_eq!(upstream.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "needs REDIS_URL"]
async fn over_the_limit_gets_429_and_hits_are_free() {
    flush().await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cafe_ok())
        .mount(&upstream)
        .await;
    let cfg = CacheConfig {
        rate_limit: 3,
        ..CacheConfig::default()
    };
    let base = spawn_api_full(
        &upstream,
        None,
        Some(Cache::new(&redis_url(), cfg).unwrap()),
    )
    .await;

    let ip = unique("9.9.9.9");
    for i in 0..3 {
        let r = post_raw(&base, json!({"tags": [format!("k{i}=v")]}), &ip).await;
        assert_eq!(r.status(), 200, "request {i}");
    }
    let limited = post_raw(&base, json!({"tags": ["k99=v"]}), &ip).await;
    assert_eq!(limited.status(), 429);
    assert!(limited.headers().contains_key("retry-after"));
    // a cached query still works for the limited client; another IP is unaffected
    let hit = post_raw(&base, json!({"tags": ["k0=v"]}), &ip).await;
    assert_eq!(x_cache(&hit), Some("HIT"));
    let other = post_raw(&base, json!({"tags": ["k99=v"]}), "8.8.8.8").await;
    assert_eq!(other.status(), 200);
    assert_eq!(upstream.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
#[ignore = "needs REDIS_URL"]
async fn upstream_overload_trips_the_breaker() {
    flush().await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(504))
        .mount(&upstream)
        .await;
    let base = spawn_api_full(
        &upstream,
        None,
        Some(Cache::new(&redis_url(), CacheConfig::default()).unwrap()),
    )
    .await;

    let first = post_raw(&base, json!({"tags": ["a=b"]}), "1.1.1.1").await;
    assert_eq!(first.status(), 504);
    let second = post_raw(&base, json!({"tags": ["c=d"]}), "1.1.1.1").await;
    assert_eq!(second.status(), 503);
    // the breaker answered without reaching upstream
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs REDIS_URL"]
async fn concurrent_identical_queries_hit_upstream_once() {
    flush().await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cafe_ok().set_delay(std::time::Duration::from_millis(500)))
        .mount(&upstream)
        .await;
    let base = spawn_api_full(
        &upstream,
        None,
        Some(Cache::new(&redis_url(), CacheConfig::default()).unwrap()),
    )
    .await;

    let tag = unique("shop=bakery");
    let body = json!({"tags": [tag]});
    let (a, b, c, d, e) = tokio::join!(
        post_raw(&base, body.clone(), "1.1.1.1"),
        post_raw(&base, body.clone(), "1.1.1.1"),
        post_raw(&base, body.clone(), "1.1.1.1"),
        post_raw(&base, body.clone(), "1.1.1.1"),
        post_raw(&base, body.clone(), "1.1.1.1"),
    );
    for r in [a, b, c, d, e] {
        assert_eq!(r.status(), 200);
    }
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
}
