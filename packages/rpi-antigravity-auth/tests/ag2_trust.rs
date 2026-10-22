//! AG-2: Node 24 trust-store switches and OpenSSL chain diagnostics.
//!
//! Each case runs in a fresh process on both sides because the switches are
//! captured before application code: Node's own reference emits the committed
//! fixture, and this binary's `ag2_trust_process` reproduces it against the
//! same loopback TLS peers.
use rpi_antigravity_auth::transport::network::{Client, Options};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command, sync::Arc};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};

const TRUST_VARS: [&str; 5] = [
    "NODE_USE_SYSTEM_CA",
    "NODE_OPTIONS",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls")
}

fn resolve(value: &str) -> String {
    let dir = fixtures();
    value
        .replace("@cert@", &dir.join("server-cert.pem").to_string_lossy())
        .replace("@ca@", &dir.join("ca-cert.pem").to_string_lossy())
        .replace("@certdir@", &dir.to_string_lossy())
}

fn acceptor(certificate: &[u8], key: &[u8]) -> TlsAcceptor {
    let certificate = CertificateDer::from(certificate.to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    TlsAcceptor::from(Arc::new(server))
}

/// A loopback TLS peer that answers one request per accepted connection.
async fn serve(acceptor: TlsAcceptor) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut socket) = acceptor.accept(socket).await else {
                    return;
                };
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    match socket.read(&mut byte).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => request.push(byte[0]),
                    }
                }
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("https://localhost:{port}/json"), task)
}

/// Child mode: trust decisions are process-owned, so this runs in its own process.
#[tokio::test]
async fn ag2_trust_process() {
    let Ok(url) = std::env::var("AG2_TRUST_URL") else {
        return;
    };
    let client = Client::default();
    let result = match client
        .fetch(
            &url,
            None,
            &[],
            b"x",
            Options {
                timeout_ms: 3000,
                environment: Default::default(),
                ..Default::default()
            },
        )
        .await
    {
        Ok(mut response) => {
            let mut body = Vec::new();
            while let Some(bytes) = response.body.next().await.unwrap() {
                body.extend(bytes);
            }
            json!({"trusted": response.head.status == 200 && body == b"ok", "message": Value::Null})
        }
        Err(error) => json!({"trusted": false, "message": error}),
    };
    println!("AG2_TRUST_RESULT={result}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_trust_store_and_chain_diagnostics_match_node() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/ag2-trust.json")).unwrap();
    let (self_signed_url, self_signed) = serve(acceptor(
        include_bytes!("fixtures/tls/server-cert.der"),
        include_bytes!("fixtures/tls/server-key.der"),
    ))
    .await;
    let (leaf_url, leaf) = serve(acceptor(
        include_bytes!("fixtures/tls/leaf-cert.der"),
        include_bytes!("fixtures/tls/leaf-key.der"),
    ))
    .await;
    for case in fixture["cases"].as_array().unwrap() {
        let url = if case["server"] == "leaf" {
            &leaf_url
        } else {
            &self_signed_url
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "ag2_trust_process", "--nocapture"]);
        for name in TRUST_VARS {
            command.env_remove(name);
        }
        for (name, value) in case["set"].as_object().unwrap() {
            command.env(name, resolve(value.as_str().unwrap()));
        }
        command.env("AG2_TRUST_URL", url);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let actual: Value = serde_json::from_str(
            stdout
                .lines()
                .find_map(|line| line.strip_prefix("AG2_TRUST_RESULT="))
                .expect("child result"),
        )
        .unwrap();
        assert_eq!(actual, case["result"], "{}", case["name"]);
    }
    self_signed.abort();
    leaf.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24 and the pinned Antigravity reference"]
async fn node_reference_regenerates_the_committed_fixture() {
    let reference = Command::new(std::env::var_os("RPI_REFERENCE_NODE").expect("RPI_REFERENCE_NODE"))
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/ag2-trust-reference.mjs"))
        .env(
            "RPI_REFERENCE_DIR",
            std::env::var_os("RPI_REFERENCE_DIR").expect("RPI_REFERENCE_DIR"),
        )
        .output()
        .unwrap();
    assert!(
        reference.status.success(),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );
}
