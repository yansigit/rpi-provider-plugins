use rpi_antigravity_auth::transport::{
    self,
    network::{Client, Options},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};
use url::Url;

fn tls() -> (Client, TlsAcceptor) {
    let certificate = CertificateDer::from(include_bytes!("fixtures/tls/server-cert.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("fixtures/tls/server-key.der").to_vec(),
    ));
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(crypto.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = rustls::ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    (
        Client::with_tls(client),
        TlsAcceptor::from(Arc::new(server)),
    )
}
fn options() -> Options {
    Options {
        timeout_ms: 2_000,
        idle_timeout_ms: 2_000,
        environment: Default::default(),
        ..Default::default()
    }
}
async fn head(socket: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await.unwrap());
    }
    bytes
}

#[tokio::test]
async fn original_response_fixtures_through_real_tls_and_proxy() {
    let (client, acceptor) = tls();
    let fixture: Value = serde_json::from_str(include_str!("fixtures/transport.json")).unwrap();
    for case in fixture["responses"].as_array().unwrap() {
        for proxy in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let mut reply = format!("{}\r\n\r\n", case["head"].as_str().unwrap()).into_bytes();
            reply.extend(serde_json::from_value::<Vec<u8>>(case["wireBody"].clone()).unwrap());
            let acceptor = acceptor.clone();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = acceptor.accept(socket).await.unwrap();
                let mut request = head(&mut socket).await;
                request.push(socket.read_u8().await.unwrap());
                for bytes in reply.chunks(7) {
                    socket.write_all(bytes).await.unwrap();
                }
                socket.shutdown().await.unwrap();
                request
            });
            let mut opts = options();
            let proxy_task = if proxy {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                opts.environment.insert(
                    "HTTPS_PROXY".into(),
                    format!("http://user:p%40ss@{}", listener.local_addr().unwrap()),
                );
                Some(tokio::spawn(async move {
                    let (mut incoming, _) = listener.accept().await.unwrap();
                    let request = head(&mut incoming).await;
                    let mut outgoing = TcpStream::connect(address).await.unwrap();
                    incoming
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                    request
                }))
            } else {
                None
            };
            let url = format!("https://localhost:{}/json", address.port());
            let mut response = client.fetch(&url, None, &[], b"x", opts).await.unwrap();
            assert_eq!(serde_json::json!(response.head.headers), case["headers"]);
            let mut body = Vec::new();
            while let Some(bytes) = response.body.next().await.unwrap() {
                assert!(bytes.len() <= 16 * 1024);
                body.extend(bytes);
            }
            assert_eq!(body, case["body"].as_str().unwrap().as_bytes(), "{case}");
            assert_eq!(
                server.await.unwrap(),
                transport::serialize_request(&Url::parse(&url).unwrap(), None, &[], b"x").unwrap()
            );
            drop(response);
            if let Some(task) = proxy_task {
                assert_eq!(task.await.unwrap(), format!("CONNECT localhost:{} HTTP/1.1\r\nHost: localhost:{}\r\nProxy-Authorization: Basic dXNlcjpwQHNz\r\n\r\n", address.port(), address.port()).into_bytes());
            }
        }
    }
}

#[tokio::test]
async fn original_constructor_and_body_errors_cross_real_tls() {
    let (client, acceptor) = tls();
    let fixture: Value = serde_json::from_str(include_str!("fixtures/transport.json")).unwrap();
    for case in fixture["responseErrors"].as_array().unwrap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = acceptor.clone();
        let mut reply = format!("{}\r\n\r\n", case["head"].as_str().unwrap()).into_bytes();
        reply.extend(serde_json::from_value::<Vec<u8>>(case["wireBody"].clone()).unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            head(&mut socket).await;
            assert_eq!(socket.read_u8().await.unwrap(), b'x');
            socket.write_all(&reply).await.unwrap();
            let _ = socket.shutdown().await;
        });
        let (phase, error) = match client
            .fetch(
                &format!("https://localhost:{}/", address.port()),
                None,
                &[],
                b"x",
                options(),
            )
            .await
        {
            Err(error) => ("headers", error),
            Ok(mut response) => loop {
                match response.body.next().await {
                    Err(error) => break ("body", error),
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("missing expected failure: {case}"),
                }
            },
        };
        assert_eq!(phase, case["phase"].as_str().unwrap(), "{case}");
        assert_eq!(error, case["error"].as_str().unwrap(), "{case}");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_during_tls_and_proxy_stages_closes_socket() {
    let (client, _) = tls();
    for stage in ["direct TLS", "proxy CONNECT", "proxy TLS"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready, accepted) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if stage.starts_with("proxy") {
                head(&mut socket).await;
                if stage == "proxy TLS" {
                    socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                }
            }
            let _ = ready.send(());
            let mut received = Vec::new();
            let _ = socket.read_to_end(&mut received).await;
        });
        let mut opts = options();
        if stage.starts_with("proxy") {
            opts.environment
                .insert("HTTPS_PROXY".into(), format!("http://{address}"));
        }
        let signal = opts.signal.clone();
        let client = client.clone();
        let request = tokio::spawn(async move {
            client
                .fetch(
                    &format!("https://localhost:{}/", address.port()),
                    None,
                    &[],
                    b"x",
                    opts,
                )
                .await
        });
        accepted.await.unwrap();
        signal.cancel();
        assert_eq!(
            request.await.unwrap().err().unwrap(),
            "The operation was aborted"
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn timeouts_retain_the_original_stage_messages() {
    let (client, _) = tls();
    for (stage, message) in [
        ("direct", "connecting"),
        ("CONNECT", "waiting for response headers"),
        ("tunnel", "during proxy TLS handshake"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if stage != "direct" {
                head(&mut socket).await;
                if stage == "tunnel" {
                    socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                }
            }
            let _ = socket.read_to_end(&mut Vec::new()).await;
        });
        let mut opts = options();
        opts.timeout_ms = 100;
        if stage != "direct" {
            opts.environment
                .insert("HTTPS_PROXY".into(), format!("http://{address}"));
        }
        assert_eq!(
            client
                .fetch(
                    &format!("https://localhost:{}/", address.port()),
                    None,
                    &[],
                    b"x",
                    opts
                )
                .await
                .err()
                .unwrap(),
            format!("Antigravity request timed out {message} after 100ms")
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn body_cancellation_idle_and_stopped_consumer_release_the_socket() {
    let (client, acceptor) = tls();
    for scenario in ["cancel", "idle", "stopped", "drop"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = acceptor.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            head(&mut socket).await;
            assert_eq!(socket.read_u8().await.unwrap(), b'x');
            socket
                .write_all(b"HTTP/1.1 200 OK\r\n\r\nfirst")
                .await
                .unwrap();
            if scenario == "stopped" {
                socket.write_all(&vec![b'x'; 4 * 16 * 1024]).await.unwrap();
            }
            let _ = socket.read_to_end(&mut Vec::new()).await;
        });
        let mut opts = options();
        opts.idle_timeout_ms = if scenario == "cancel" { 0 } else { 100 };
        let signal = opts.signal.clone();
        let mut response = client
            .fetch(
                &format!("https://localhost:{}/", address.port()),
                None,
                &[],
                b"x",
                opts,
            )
            .await
            .unwrap();
        if scenario != "stopped" {
            assert_eq!(response.body.next().await.unwrap().unwrap(), b"first");
        }
        if scenario == "cancel" {
            signal.cancel();
        }
        if scenario == "drop" {
            drop(response);
        } else {
            if scenario == "stopped" {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            assert_eq!(
                response.body.next().await.err().unwrap(),
                if scenario == "cancel" {
                    "The operation was aborted"
                } else {
                    "Antigravity response stalled: no data for 100ms"
                }
            );
            assert!(response.body.next().await.unwrap().is_none());
        }
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[test]
fn proxy_auth_uri_decoding_matches_javascript_errors() {
    for credential in ["user:%", "user:%ff", "%C3%28:pass"] {
        assert_eq!(
            transport::network::proxy_connect_request(
                &Url::parse(&format!("http://{credential}@proxy.test")).unwrap(),
                &Url::parse("https://example.test").unwrap()
            )
            .err()
            .unwrap(),
            "URI malformed"
        );
    }
}

#[tokio::test]
async fn malformed_nonprogressing_chunk_yields_to_timeout_after_eof() {
    let (client, acceptor) = tls();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = acceptor.accept(socket).await.unwrap();
        head(&mut socket).await;
        socket.read_u8().await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-6\r\n")
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });
    let mut opts = options();
    opts.idle_timeout_ms = 100;
    let mut response = client
        .fetch(
            &format!("https://localhost:{}/", address.port()),
            None,
            &[],
            b"x",
            opts,
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), response.body.next())
            .await
            .unwrap()
            .err()
            .unwrap(),
        "Antigravity response stalled: no data for 100ms"
    );
    server.await.unwrap();
}
