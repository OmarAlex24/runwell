#![cfg(unix)]
mod support;
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::{Request, Response, StatusCode, body::Frame};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::Duration,
};
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn echoed_rewrites_passthrough_permissions_and_json_cap() {
    let mut fixture = Fixture::new(echo).await;
    fixture.settings.max_json_bytes = 512;
    // M3 may have left the shared runtime root private before proxies existed.
    std::fs::create_dir(&fixture.settings.run_dir).unwrap();
    std::fs::set_permissions(
        &fixture.settings.run_dir,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fixture.start().await;
    assert_eq!(
        std::fs::metadata(&fixture.settings.run_dir).unwrap().mode() & 0o777,
        0o711
    );
    let metadata = std::fs::metadata(fixture.socket()).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o660);
    assert_eq!(
        (metadata.uid(), metadata.gid()),
        (fixture.spec.uid, fixture.spec.gid)
    );
    let body = br#"{ "Image": "busybox", "Labels": {"user":"yes"} }"#;
    for path in [
        "/containers/create",
        "/v1.47/containers/create",
        "/networks/create",
        "/v1.41/volumes/create",
    ] {
        let response = fixture.send("POST", path, full(body.as_slice())).await;
        let value: Value = serde_json::from_slice(&bytes(response).await).unwrap();
        assert_eq!(
            value["Labels"],
            json!({"user":"yes","io.runwell.job":"42","io.runwell.node":"node-a"})
        );
        if path.ends_with("/containers/create") {
            assert_eq!(value["HostConfig"]["CgroupParent"], "ci-rw-j42.slice");
        }
    }
    let original = Bytes::from_static(b"\0\xffnot-json\r\n\x01\x02");
    let request = Request::builder()
        .method("PUT")
        .uri("/v1.47/containers/a/archive?path=%2ftmp&x=a+b")
        .header("x-exact", "opaque header")
        .body(full(original.clone()))
        .unwrap();
    let response = send(&fixture.socket(), request).await;
    assert_eq!(
        response.headers()["x-uri"],
        "/v1.47/containers/a/archive?path=%2ftmp&x=a+b"
    );
    assert_eq!(response.headers()["x-exact"], "opaque header");
    assert_eq!(bytes(response).await, original);
    let response = fixture
        .send("POST", "/build?version=2&t=a%2fb", full(original.clone()))
        .await;
    assert!(
        response.headers()["x-uri"]
            .to_str()
            .unwrap()
            .starts_with("/build?version=2&t=a%2fb&cgroupparent=ci-rw-j42.slice&labels=")
    );
    assert_eq!(bytes(response).await, original);
    for chunked in [false, true] {
        let body = if chunked {
            StreamBody::new(stream::iter((0..10).map(|_| {
                Ok::<_, Infallible>(Frame::data(Bytes::from(vec![b' '; 128])))
            })))
            .map_err(BoxError::from)
            .boxed_unsync()
        } else {
            full(vec![b' '; 513])
        };
        let response = fixture.send("POST", "/containers/create", body).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            String::from_utf8(bytes(response).await.to_vec())
                .unwrap()
                .contains("512 bytes")
        );
    }
    fixture.stop().await;
    assert!(!fixture.socket().exists());
}
#[tokio::test]
async fn chunked_responses_deliver_before_eof_and_preserve_trailers() {
    let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let upstream = release.clone();
    let mut fixture = Fixture::new(move |_| {
        let release = upstream.clone();
        async move {
            let frames = stream::unfold((0, release), |(index, release)| async move {
                let frame = match index {
                    0 => Frame::data(Bytes::from_static(b"\0first\xff")),
                    1 => {
                        release.acquire().await.unwrap().forget();
                        Frame::data(Bytes::from_static(b"second\r\n"))
                    }
                    2 => {
                        let mut trailers = hyper::HeaderMap::new();
                        trailers.insert("x-checksum", "intact".parse().unwrap());
                        Frame::trailers(trailers)
                    }
                    _ => return None,
                };
                Some((Ok::<_, Infallible>(frame), (index + 1, release)))
            });
            Response::builder()
                .header("trailer", "x-checksum")
                .body(
                    StreamBody::new(frames)
                        .map_err(BoxError::from)
                        .boxed_unsync(),
                )
                .unwrap()
        }
    })
    .await;
    fixture.start().await;
    // Identical streaming forwarding covers logs follow, events, stats, pull and build output.
    for (method, path) in [
        ("GET", "/containers/a/logs?follow=1"),
        ("GET", "/events"),
        ("GET", "/containers/a/stats"),
        ("POST", "/images/create?fromImage=busybox"),
        ("POST", "/build"),
    ] {
        let response = fixture.send(method, path, full("")).await;
        let mut body = response.into_body();
        let first = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.into_data().unwrap(), b"\0first\xff".as_slice());
        release.add_permits(1);
        let collected = body.collect().await.unwrap();
        assert_eq!(collected.trailers().unwrap()["x-checksum"], "intact");
        assert_eq!(collected.to_bytes(), b"second\r\n".as_slice());
    }
    fixture.stop().await;
}
#[tokio::test]
async fn tcp_and_h2c_upgrades_preserve_stdin_tty_multiplexed_frames_and_half_close() {
    let mut fixture = Fixture::new(|mut request| async move {
        let protocol = request.headers()["upgrade"].clone();
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let mut io = TokioIo::new(upgrade.await.unwrap());
            io.write_all(b"greeting\0").await.unwrap();
            let (mut read, mut write) = tokio::io::split(io);
            tokio::io::copy(&mut read, &mut write).await.unwrap();
            // Verify the other direction remains writable after stdin EOF.
            write.write_all(b"after-eof").await.unwrap();
            write.shutdown().await.unwrap();
        });
        Response::builder()
            .status(101)
            .header("connection", "Upgrade")
            .header("upgrade", protocol)
            .body(full(""))
            .unwrap()
    })
    .await;
    fixture.start().await;
    for (protocol, path) in [
        ("tcp", "/v1.47/containers/a/attach"),
        ("tcp", "/exec/a/start"),
        ("h2c", "/session"),
        ("h2c", "/grpc"),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("connection", "Upgrade")
            .header("upgrade", protocol)
            .body(full("{}"))
            .unwrap();
        let mut response = send(&fixture.socket(), request).await;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(response.headers()["upgrade"], protocol);
        let mut io = TokioIo::new(hyper::upgrade::on(&mut response).await.unwrap());
        let mut greeting = [0; 9];
        io.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"greeting\0");
        let payload = b"\x01\0\0\0\0\0\0\x05stdin\r\n\0\xffPRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        io.write_all(payload).await.unwrap();
        io.shutdown().await.unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), io.read_to_end(&mut output))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output, [payload.as_slice(), b"after-eof"].concat());
    }
    fixture.stop().await;
}

#[tokio::test]
async fn duplicate_listener_and_non_socket_paths_are_never_replaced() {
    use runwell_dockerproxy::{CgroupDriver, Error, Proxy, Rewriter};
    let mut fixture = Fixture::new(echo).await;
    fixture.start().await;
    let policy = || {
        Rewriter::new(
            fixture.spec.clone(),
            fixture.settings.clone(),
            CgroupDriver::Systemd,
        )
        .unwrap()
    };
    assert!(matches!(Proxy::start(policy()).await, Err(Error::Config)));
    let response = fixture.send("GET", "/_ping", full("still serving")).await;
    assert_eq!(bytes(response).await, "still serving");
    fixture.stop().await;
    std::fs::write(fixture.socket(), "keep this file").unwrap();
    let policy = Rewriter::new(
        fixture.spec.clone(),
        fixture.settings.clone(),
        CgroupDriver::Systemd,
    )
    .unwrap();
    assert!(matches!(Proxy::start(policy).await, Err(Error::Config)));
    assert_eq!(
        std::fs::read_to_string(fixture.socket()).unwrap(),
        "keep this file"
    );
}
