use overpass::Client;
use overpass::history::History;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start the API against `upstream` on a random port; returns its base URL.
async fn spawn_api(upstream: &MockServer) -> String {
    spawn_api_with_history(upstream, None).await
}

async fn spawn_api_with_history(upstream: &MockServer, history: Option<&str>) -> String {
    let client = Client::with_endpoint(upstream.uri()).unwrap().retries(0);
    let history = history.map(|url| History::new(url).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, overpass::server::router(client, history)).into_future());
    format!("http://{addr}")
}

/// Wait for the background history POST to land.
async fn history_events(history: &MockServer, n: usize) -> Vec<Value> {
    for _ in 0..100 {
        let reqs = history.received_requests().await.unwrap();
        if reqs.len() >= n {
            return reqs.iter().map(|r| r.body_json().unwrap()).collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("expected {n} history events");
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
async fn reports_queries_to_history() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"elements":[{"type":"node","id":1,"lat":1.0,"lon":2.0},{"type":"node","id":2,"lat":1.0,"lon":2.0}]}"#,
        ))
        .mount(&upstream)
        .await;
    let history = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/events"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&history)
        .await;
    let base = spawn_api_with_history(&upstream, Some(&format!("{}/", history.uri()))).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/query"))
        .header(
            "user-agent",
            "Mozilla/5.0 (X11; Linux x86_64) Firefox/143.0",
        )
        .json(&json!({"tags": ["amenity=cafe", " "], "bbox": [1, 2, 3, 4], "kind": "way"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let (status, _) = post(&base, json!({"tags": []})).await;
    assert_eq!(status, 400);

    let events = history_events(&history, 2).await;
    let ok = events.iter().find(|e| e["status"] == 200).unwrap();
    assert_eq!(ok["tags"], json!(["amenity=cafe"]));
    assert_eq!(ok["area_type"], "bbox");
    assert_eq!(ok["coords"], json!([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(ok["kind"], "way");
    assert_eq!(ok["element_count"], 2);
    assert!(ok["duration_ms"].is_u64());
    assert_eq!(
        ok["user_agent"],
        "Mozilla/5.0 (X11; Linux x86_64) Firefox/143.0"
    );

    let bad = events.iter().find(|e| e["status"] == 400).unwrap();
    assert_eq!(bad["area_type"], "none");
    assert!(bad["coords"].is_null());
    assert!(bad["element_count"].is_null());
}

#[tokio::test]
async fn history_down_does_not_affect_response() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"elements":[]}"#))
        .mount(&upstream)
        .await;
    // Reserve a port, then free it so nothing listens there.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_url = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let base = spawn_api_with_history(&upstream, Some(&dead_url)).await;

    let (status, body) = post(&base, json!({"tags": ["a"]})).await;
    assert_eq!(status, 200);
    assert_eq!(body["elements"], json!([]));
}
