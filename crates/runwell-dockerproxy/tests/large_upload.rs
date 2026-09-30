#![cfg(unix)]
mod support;
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::{Response, body::Frame};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;
const CHUNK: usize = 64 * 1024;
const TOTAL: usize = 200 * 1024 * 1024;
#[tokio::test]
async fn generated_200_mib_tar_build_and_archive_uploads_have_bounded_inflight_bytes() {
    let produced = Arc::new(AtomicUsize::new(0));
    let consumed = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let observed = consumed.clone();
    let mut fixture = Fixture::new(move |request| {
        let consumed = observed.clone();
        async move {
            let mut body = request.into_body();
            let mut count = 0;
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.unwrap().into_data() {
                    // 200 MiB of valid tar padding (zero-filled archive blocks).
                    assert!(data.iter().all(|b| *b == 0));
                    count += data.len();
                    consumed.store(count, Ordering::SeqCst);
                    if count <= CHUNK {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            assert_eq!(count, TOTAL);
            Response::new(full(count.to_string()))
        }
    })
    .await;
    fixture.start().await;
    for (method, path) in [
        ("POST", "/v1.47/build?version=2"),
        ("PUT", "/containers/a/archive?path=/tmp"),
    ] {
        produced.store(0, Ordering::SeqCst);
        consumed.store(0, Ordering::SeqCst);
        maximum.store(0, Ordering::SeqCst);
        let producer = produced.clone();
        let consumer = consumed.clone();
        let high_water = maximum.clone();
        let chunks = stream::unfold(0, move |count| {
            let producer = producer.clone();
            let consumer = consumer.clone();
            let high_water = high_water.clone();
            async move {
                if count == TOTAL {
                    return None;
                }
                let next = count + CHUNK;
                producer.store(next, Ordering::SeqCst);
                let inflight = next.saturating_sub(consumer.load(Ordering::SeqCst));
                high_water.fetch_max(inflight, Ordering::SeqCst);
                // A buffering proxy fails as soon as it consumes 8 MiB without
                // forwarding it. This bounds payload memory across all hops;
                // it does not depend on allocator behavior or process RSS noise.
                assert!(
                    inflight < 8 * 1024 * 1024,
                    "proxy lost upload backpressure: {inflight}"
                );
                Some((
                    Ok::<_, Infallible>(Frame::data(Bytes::from(vec![0; CHUNK]))),
                    next,
                ))
            }
        });
        let body = StreamBody::new(chunks)
            .map_err(BoxError::from)
            .boxed_unsync();
        let response =
            tokio::time::timeout(Duration::from_secs(60), fixture.send(method, path, body))
                .await
                .unwrap();
        assert_eq!(bytes(response).await, TOTAL.to_string());
        assert_eq!(produced.load(Ordering::SeqCst), TOTAL);
        assert_eq!(consumed.load(Ordering::SeqCst), TOTAL);
        assert!(maximum.load(Ordering::SeqCst) < 8 * 1024 * 1024);
    }
    fixture.stop().await;
}
