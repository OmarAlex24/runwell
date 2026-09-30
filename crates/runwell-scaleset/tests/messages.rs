mod common;
use common::*;
use futures_util::{FutureExt, StreamExt};
use runwell_scaleset::{Error, Event, Listener};
use serde_json::json;
use wiremock::{Mock, ResponseTemplate, matchers::*};

#[tokio::test]
async fn bom_double_encoded_body_typed_events_and_ack_last() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let body = format!("\u{feff}{}", fixture("message"));
    Mock::given(method("GET"))
        .and(path("/queue"))
        .and(query_param("lastMessageId", "0"))
        .and(query_param("partition", "synthetic"))
        .and(header("X-ScaleSetMaxCapacity", "12"))
        .and(header("Authorization", "Bearer synthetic-queue-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/queue/0"))
        .and(query_param("partition", "synthetic"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 12);
    let initial = listener.next().await.unwrap().unwrap();
    assert!(initial.message_id.is_none());
    assert_eq!(initial.statistics.unwrap().total_assigned_jobs, 7);
    let message = listener.next().await.unwrap().unwrap();
    assert_eq!(message.message_id, Some(0));
    assert_eq!(message.events.len(), 4);
    let Event::JobAvailable(job) = &message.events[0] else {
        panic!("wrong event")
    };
    assert_eq!(job.job.runner_request_id, 987654321);
    assert!(job.job.queue_time.is_some());
    assert!(job.job.scale_set_assign_time.is_none());
    assert!(matches!(&message.events[1], Event::JobAssigned(_)));
    assert!(matches!(&message.events[2], Event::JobStarted(job) if job.runner_id == 1234));
    assert!(matches!(&message.events[3], Event::JobCompleted(job) if job.result == "succeeded"));
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
    assert!(matches!(
        listener.next().now_or_never(),
        Some(Some(Err(Error::AckRequired)))
    ));
    assert!(listener.ack(9).await.is_err());
    listener.ack(0).await.unwrap();
    listener.close().await.unwrap();
    assert_eq!(count(&server, "DELETE", SESSION).await, 1);
    versions(&server).await;
}

#[tokio::test]
async fn unknown_message_types_remain_ackable_and_do_not_wedge_queue() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let unknown = json!({"messageId":5,"messageType":"FutureEnvelope","body":"not even JSON"});
    let inner = json!({"messageId":6,"messageType":"RunnerScaleSetJobMessages",
        "body":json!([{"messageType":"FutureJob"},{"messageType":"JobAssigned","runnerRequestId":99}]).to_string()});
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(200).set_body_json(unknown),
            ResponseTemplate::new(200).set_body_json(inner),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex("^/queue/[56]$"))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    let message = listener.next().await.unwrap().unwrap();
    assert!(message.events.is_empty());
    assert_eq!(count(&server, "DELETE", "/queue/5").await, 0);
    listener.ack(5).await.unwrap();
    let message = listener.next().await.unwrap().unwrap();
    assert_eq!(message.events.len(), 1);
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().any(|r| {
        r.url.path() == "/queue"
            && r.url
                .query_pairs()
                .any(|(k, v)| k == "lastMessageId" && v == "5")
    }));
    listener.ack(6).await.unwrap();
    listener.close().await.unwrap();
}

#[tokio::test]
async fn empty_202_repolls_with_capacity_and_without_ack() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .and(header("X-ScaleSetMaxCapacity", "0"))
        .and(query_param("lastMessageId", "0"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(202),
            ResponseTemplate::new(200).set_body_json(fixture("message")),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 100);
    listener.next().await.unwrap().unwrap();
    listener.set_max_capacity(0);
    assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(0));
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
    listener.close().await.unwrap();
}

#[tokio::test]
async fn processing_failure_closes_without_ack_and_next_session_redelivers() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("message")))
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
        listener.next().await.unwrap().unwrap();
        assert_eq!(listener.next().await.unwrap().unwrap().message_id, Some(0));
        listener.close().await.unwrap();
    }
    assert_eq!(count(&server, "DELETE", "/queue/0").await, 0);
}

#[tokio::test]
async fn truncated_events_scale_from_absolute_statistics() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    let events = vec![json!({"messageType":"JobAssigned"}); 50];
    Mock::given(method("GET"))
        .and(path("/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"messageId":1,"messageType":"RunnerScaleSetJobMessages",
            "body":serde_json::to_string(&events).unwrap(),"statistics":{"totalAssignedJobs":150}}),
        ))
        .mount(&server)
        .await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 200);
    listener.next().await.unwrap().unwrap();
    let message = listener.next().await.unwrap().unwrap();
    assert_eq!(message.events.len(), 50);
    assert_eq!(
        runwell_scaleset::desired_runners(2, 200, message.statistics.unwrap().total_assigned_jobs),
        152
    );
    listener.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_before_assignment_is_typed_completion() {
    let (server, client, _) = harness().await;
    sessions(&server).await;
    Mock::given(method("GET")).and(path("/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messageId":1,"messageType":"RunnerScaleSetJobMessages",
            "body":json!([{"messageType":"JobCompleted","runnerRequestId":99,"result":"canceled"}]).to_string()
        }))).mount(&server).await;
    let mut listener = Listener::new(client.open_session(42, "test-owner").await.unwrap(), 1);
    listener.next().await.unwrap().unwrap();
    let message = listener.next().await.unwrap().unwrap();
    assert!(
        matches!(&message.events[0],Event::JobCompleted(job) if job.runner_id == 0 && job.runner_name.is_empty() && job.result == "canceled")
    );
    listener.close().await.unwrap();
}
