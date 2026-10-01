use overpass::{Area, Client, Coord, Error, Query, Tagged};
use std::time::Duration;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn server_with(status: u16, body: &str) -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;
    let client = Client::with_endpoint(server.uri()).unwrap().retries(0);
    (server, client)
}

#[tokio::test]
async fn fetch_ok_sends_form_encoded_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("data=%5Bout%3Ajson%5D"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"elements":[{"type":"node","id":1,"lat":1.0,"lon":2.0,"tags":{"name":"X"}}]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::with_endpoint(server.uri()).unwrap();

    let resp = client
        .fetch(&Query::new().tag("amenity", "cafe"))
        .await
        .unwrap();
    assert_eq!(resp.elements.len(), 1);
    assert_eq!(resp.elements[0].name(), Some("X"));
}

#[tokio::test]
async fn rate_limited() {
    let (_s, client) = server_with(429, "").await;
    assert!(matches!(
        client.raw("[out:json];").await,
        Err(Error::RateLimited)
    ));
}

#[tokio::test]
async fn gateway_timeout() {
    let (_s, client) = server_with(504, "").await;
    assert!(matches!(
        client.raw("[out:json];").await,
        Err(Error::Timeout)
    ));
}

#[tokio::test]
async fn other_status_keeps_body() {
    let (_s, client) = server_with(400, "parse error: line 1").await;
    match client.raw("garbage").await {
        Err(Error::Status { code: 400, body }) => assert!(body.contains("parse error")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn runtime_error_remark() {
    let (_s, client) = server_with(
        200,
        r#"{"elements":[],"remark":"runtime error: Query timed out in \"query\" at line 1"}"#,
    )
    .await;
    assert!(matches!(
        client.raw("[out:json];").await,
        Err(Error::Remark(_))
    ));
}

#[tokio::test]
async fn invalid_json() {
    let (_s, client) = server_with(200, "<osm/>").await;
    assert!(matches!(
        client.raw("[out:json];").await,
        Err(Error::Parse(_))
    ));
}

#[tokio::test]
async fn retries_busy_server_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(504))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"elements":[]}"#))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::with_endpoint(server.uri())
        .unwrap()
        .retry_base_delay(Duration::from_millis(1));

    assert!(client.raw("[out:json];").await.is_ok());
}

#[tokio::test]
async fn gives_up_after_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(3)
        .mount(&server)
        .await;
    let client = Client::with_endpoint(server.uri())
        .unwrap()
        .retries(2)
        .retry_base_delay(Duration::from_millis(1));

    assert!(matches!(
        client.raw("[out:json];").await,
        Err(Error::RateLimited)
    ));
}

#[tokio::test]
#[ignore = "hits the public overpass-api.de server"]
async fn live_cafes_in_kyiv() {
    let q = Query::new().tag("amenity", "cafe").within(Area::Around {
        center: Coord {
            lat: 50.45,
            lon: 30.52,
        },
        radius_m: 500.0,
    });
    let resp = Client::new().unwrap().fetch(&q).await.unwrap();
    assert!(!resp.elements.is_empty());
    assert!(resp.elements.iter().all(|e| e.has_tag("amenity", "cafe")));
}
