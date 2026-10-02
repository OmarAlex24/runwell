use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

async fn responses(responses: Vec<ResponseTemplate>) -> MockServer {
    let server = MockServer::start().await;
    let calls = AtomicUsize::new(0);
    let count = responses.len() as u64;
    Mock::given(path("/resource"))
        .respond_with(move |_: &wiremock::Request| {
            responses[calls
                .fetch_add(1, Ordering::SeqCst)
                .min(responses.len() - 1)]
            .clone()
        })
        .expect(count)
        .mount(&server)
        .await;
    server
}

fn client(base: String) -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(base, "synthetic-token".into(), dir.path().into()).unwrap();
    (client, dir)
}

async fn fetch(client: &mut Client) -> (Result<String, Error>, Vec<Duration>) {
    let mut delays = Vec::new();
    let result = client
        .raw_with_sleep("/resource", false, |until| {
            delays.push(until.saturating_duration_since(Instant::now()));
            std::future::ready(())
        })
        .await;
    (result, delays)
}

fn assert_delay(delay: Duration, seconds: u64) {
    let expected = Duration::from_secs(seconds);
    assert!(delay <= expected && delay >= expected - Duration::from_millis(100));
}

#[tokio::test]
async fn transient_statuses_retry_then_cache_success() {
    for status in [500, 502, 503, 504] {
        let server = responses(vec![
            ResponseTemplate::new(status),
            ResponseTemplate::new(200).set_body_string("success"),
        ])
        .await;
        let (mut client, _dir) = client(server.uri());

        let (result, delays) = fetch(&mut client).await;

        assert_eq!(result.unwrap(), "success");
        assert_eq!(delays.len(), 1);
        assert!((1.4..=2.0).contains(&delays[0].as_secs_f64()));
        assert_eq!(client.raw("/resource", false).await.unwrap(), "success");
        server.verify().await;
    }
}

#[tokio::test]
async fn repeated_503_exhausts_six_attempts_and_preserves_http_error() {
    let server = responses(vec![ResponseTemplate::new(503); 6]).await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Http(503))));
    assert_eq!(delays.len(), 5);
    for (delay, ceiling) in delays.iter().zip([2.0, 4.0, 8.0, 16.0, 30.0]) {
        assert!((ceiling * 0.75 - 0.1..=ceiling).contains(&delay.as_secs_f64()));
    }
    server.verify().await;
}

#[tokio::test]
async fn final_error_is_the_last_status_in_a_shared_retry_budget() {
    let server = responses(
        [502, 429, 500, 503, 504, 500]
            .map(|status| ResponseTemplate::new(status).insert_header("retry-after", "0"))
            .to_vec(),
    )
    .await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Http(500))));
    assert_eq!(delays.len(), 5);
    server.verify().await;
}

#[tokio::test]
async fn other_statuses_fail_after_exactly_one_request() {
    for status in [401, 404, 422, 501] {
        let server = responses(vec![
            ResponseTemplate::new(status).insert_header("retry-after", "45"),
        ])
        .await;
        let (mut client, _dir) = client(server.uri());

        let (result, delays) = fetch(&mut client).await;

        assert!(matches!(result, Err(Error::Http(code)) if code == status));
        assert!(delays.is_empty());
        server.verify().await;
    }
}

#[tokio::test]
async fn server_retry_after_overrides_the_backoff_cap() {
    let server = responses(vec![
        ResponseTemplate::new(503).insert_header("retry-after", "45"),
        ResponseTemplate::new(200),
    ])
    .await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    result.unwrap();
    assert_eq!(delays.len(), 1);
    assert_delay(delays[0], 45);
    server.verify().await;
}

#[tokio::test]
async fn server_retry_after_accepts_http_dates() {
    let date = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(45));
    let server = responses(vec![
        ResponseTemplate::new(502).insert_header("retry-after", date.as_str()),
        ResponseTemplate::new(200),
    ])
    .await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    result.unwrap();
    assert_eq!(delays.len(), 1);
    assert!((43.0..=45.0).contains(&delays[0].as_secs_f64()));
    server.verify().await;
}

#[tokio::test]
async fn rate_limit_keeps_retry_after_and_default_exponential_delays() {
    let server = responses(vec![
        ResponseTemplate::new(429).insert_header("retry-after", "7"),
        ResponseTemplate::new(429),
        ResponseTemplate::new(403).set_body_string("secondary rate limit"),
        ResponseTemplate::new(200),
    ])
    .await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    result.unwrap();
    assert_eq!(delays.len(), 3);
    for (delay, seconds) in delays.into_iter().zip([7, 120, 240]) {
        assert_delay(delay, seconds);
    }
    server.verify().await;
}

#[tokio::test]
async fn repeated_rate_limits_keep_the_budget_and_final_cooldown() {
    let server = responses(vec![
        ResponseTemplate::new(429)
            .insert_header("retry-after", "7");
        6
    ])
    .await;
    let (mut client, _dir) = client(server.uri());

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Http(429))));
    assert_eq!(delays.len(), 5);
    assert!(client.blocked_until.is_some());
    for delay in delays {
        assert_delay(delay, 7);
    }
    server.verify().await;
}

#[tokio::test]
async fn cache_hit_skips_requests_and_pending_rate_limit_wait() {
    let server = responses(vec![
        ResponseTemplate::new(200)
            .set_body_string("cached")
            .insert_header("x-ratelimit-remaining", "0")
            .insert_header("retry-after", "60"),
    ])
    .await;
    let (mut client, _dir) = client(server.uri());
    assert_eq!(client.raw("/resource", false).await.unwrap(), "cached");

    let (result, delays) = fetch(&mut client).await;

    assert_eq!(result.unwrap(), "cached");
    assert!(delays.is_empty());
    assert!(client.blocked_until.is_some());
    server.verify().await;
}

#[test]
fn malformed_retry_after_falls_back_to_jittered_backoff() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", "invalid".parse().unwrap());
    assert_eq!(retry_after(&headers), None);
    for attempt in 0..6 {
        let delay = backoff(attempt);
        assert!(delay >= Duration::from_millis(1500) && delay <= Duration::from_secs(30));
    }
}

#[tokio::test]
async fn invalid_json_is_not_retried_as_a_body_decode_error() {
    let server = responses(vec![ResponseTemplate::new(200).set_body_string("not json")]).await;
    let (mut client, _dir) = client(server.uri());

    assert!(matches!(
        client.json("/resource", false).await,
        Err(Error::Json(_))
    ));
    server.verify().await;
}

mod transport;
