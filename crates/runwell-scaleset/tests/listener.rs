mod common;
use common::*;
use futures_util::{FutureExt, StreamExt};
use runwell_scaleset::{Error, Listener, retry::Clock};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use wiremock::{Mock, Request, ResponseTemplate, matchers::*};

#[tokio::test]
async fn next_before_ack_fails_immediately_and_resumes_after_ack() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let mut second = fixture("message");
    second["messageId"] = json!(7);
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200).set_body_json(fixture("message")),
            ResponseTemplate::new(200).set_body_json(second),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/queue/0"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(0));
    for _ in 0..2 {
        assert!(matches!(
            listener.next().now_or_never(),
            Some(Some(Err(Error::AckRequired)))
        ));
    }
    assert_eq!(count(&server, "GET", "/queue").await, 1);
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
    listener.ack(0).await.unwrap();
    assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(7));
    listener.close().await.unwrap();
}

#[tokio::test]
async fn rapid_empty_polls_are_bounded_in_a_virtual_minute() {
    let (server, client, clock) = harness().await;
    sessions(&server).await;
    let start = clock.now();
    let window = Duration::from_secs(60);
    let polls = Arc::new(Mutex::new(Vec::new()));
    let poll_times = polls.clone();
    let service_clock = clock.clone();
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(move |_: &Request| {
            let elapsed = service_clock.now().duration_since(start).unwrap();
            let mut times = poll_times.lock().unwrap();
            times.push(elapsed);
            // Bound even a broken implementation so the regression fails instead of hanging.
            if elapsed >= window || times.len() == 20 {
                ResponseTemplate::new(200).set_body_json(fixture("message"))
            } else {
                ResponseTemplate::new(202)
            }
        })
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    listener.next().await.unwrap().unwrap();
    assert!(clock.now().duration_since(start).unwrap() >= window);
    // Minimum successive delays are 1, 1, 2, 4, 8, 15, 15, 15 seconds:
    // at most eight requests in the window, then one delivering the message.
    let times = polls.lock().unwrap().clone();
    assert!(
        times.iter().filter(|&&time| time < window).count() <= 8,
        "{times:?}"
    );
    assert!(times.len() <= 9, "{times:?}");
    assert_eq!(count(&server, "GET", "/queue").await, times.len());
    let sleeps = clock.sleeps.lock().unwrap().clone();
    assert_eq!(sleeps.len(), times.len() - 1);
    assert!(
        sleeps
            .iter()
            .all(|delay| (Duration::from_secs(1)..=Duration::from_secs(30)).contains(delay))
    );
    listener.close().await.unwrap();
}

#[tokio::test]
async fn normal_long_poll_resets_rapid_empty_poll_backoff() {
    let (server, client, clock) = harness().await;
    sessions(&server).await;
    let service_clock = clock.clone();
    let polls = AtomicUsize::new(0);
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(
            move |_: &Request| match polls.fetch_add(1, Ordering::SeqCst) {
                3 => {
                    service_clock.advance(Duration::from_secs(5));
                    ResponseTemplate::new(202)
                }
                5.. => ResponseTemplate::new(200).set_body_json(fixture("message")),
                _ => ResponseTemplate::new(202),
            },
        )
        .expect(6)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    listener.next().await.unwrap().unwrap();
    let sleeps = clock.sleeps.lock().unwrap().clone();
    assert_eq!(sleeps.len(), 4);
    assert_eq!(sleeps[0], Duration::from_secs(1));
    assert!((Duration::from_secs(1)..=Duration::from_secs(2)).contains(&sleeps[1]));
    assert!((Duration::from_secs(2)..=Duration::from_secs(4)).contains(&sleeps[2]));
    assert_eq!(sleeps[3], Duration::from_secs(1));
    listener.close().await.unwrap();
}
