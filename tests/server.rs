use overpass::Client;
use overpass::history::History;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start the API against `upstream` on a random port; returns its base URL.
async fn spawn_api(upstream: &MockServer) -> String {
    spawn_api_with_history(upstream, None).await
}

async fn spawn_api_with_history(upstream: &MockServer, history: Option<&str>) -> String {
    let client = Client::with_endpoint(upstream.uri()).unwrap().retries(0);
    let history = history.map(|url| History::new(url));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, overpass::server::router(client, history)).into_future());
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
