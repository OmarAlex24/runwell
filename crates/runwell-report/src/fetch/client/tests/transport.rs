use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn connection_failure_then_success_is_retried() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (mut client, _dir) = client(format!("http://{address}"));
    let mut server_task = None;
    let mut delays = Vec::new();

    let result = client
        .raw_with_sleep("/resource", false, |until| {
            delays.push(until.saturating_duration_since(Instant::now()));
            assert!(
                server_task.is_none(),
                "only the first connection should fail"
            );
            let listener = std::net::TcpListener::bind(address).unwrap();
            server_task = Some(tokio::spawn(async move {
                let server = MockServer::builder().listener(listener).start().await;
                Mock::given(path("/resource"))
                    .respond_with(ResponseTemplate::new(200).set_body_string("success"))
                    .expect(1)
                    .mount(&server)
                    .await;
                server
            }));
            std::future::ready(())
        })
        .await;

    assert_eq!(result.unwrap(), "success");
    assert_eq!(delays.len(), 1);
    server_task.unwrap().await.unwrap().verify().await;
}

#[tokio::test]
async fn timeout_then_success_retries_without_sleeping_for_backoff() {
    let server = responses(vec![
        ResponseTemplate::new(200).set_delay(Duration::from_secs(10)),
        ResponseTemplate::new(200).set_body_string("success"),
    ])
    .await;
    let (mut client, _dir) = client(server.uri());
    client.http = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let (result, delays) = fetch(&mut client).await;

    assert_eq!(result.unwrap(), "success");
    assert_eq!(delays.len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn repeated_timeouts_return_the_last_request_error() {
    let server = responses(vec![
        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(10));
        6
    ])
    .await;
    let (mut client, _dir) = client(server.uri());
    client.http = reqwest::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Request(error)) if error.is_timeout()));
    assert_eq!(delays.len(), 5);
    server.verify().await;
}

// Wiremock always sends complete bodies, so use raw HTTP for truncated reads.
async fn truncated_then_success(status: u16) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for (status, length) in [(status, 10), (200, 2)] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Response\r\nContent-Length: {length}\r\nConnection: close\r\n\r\nok"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    (base, task)
}

#[tokio::test]
async fn truncated_success_body_is_retried_and_only_complete_body_is_cached() {
    let (base, task) = truncated_then_success(200).await;
    let (mut client, _dir) = client(base);

    let (result, delays) = fetch(&mut client).await;

    assert_eq!(result.unwrap(), "ok");
    assert_eq!(delays.len(), 1);
    task.await.unwrap();
    assert_eq!(client.raw("/resource", false).await.unwrap(), "ok");
}

#[tokio::test]
async fn unreadable_404_body_does_not_cause_a_retry() {
    let (base, task) = truncated_then_success(404).await;
    let (mut client, _dir) = client(base);

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Http(404))));
    assert!(delays.is_empty());
    task.abort();
}

#[tokio::test]
async fn non_transport_request_errors_fail_fast() {
    let (mut client, _dir) = client("invalid-url".into());

    let (result, delays) = fetch(&mut client).await;

    assert!(matches!(result, Err(Error::Request(error)) if error.is_builder()));
    assert!(delays.is_empty());
}
