use rpi_antigravity_auth::{
    auth::{network::Network, Body, Port, Request},
    transport::network::{Client, Options},
};
use serde_json::Value;
use std::{path::PathBuf, process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;
#[path = "common/tls.rs"]
mod tls_fixture;

#[tokio::test]
async fn trust_process() {
    let Ok(url) = std::env::var("RPI_TRUST_TEST_URL") else {
        return;
    };
    let client = Client::default();
    if std::env::var("RPI_TRUST_DELETE").as_deref() == Ok("1") {
        std::fs::remove_file(std::env::var_os("NODE_EXTRA_CA_CERTS").unwrap()).unwrap();
    }
    let first = match client
        .fetch(
            &url,
            None,
            &[],
            b"x",
            Options {
                timeout_ms: 2000,
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
            response.head.status == 200 && body == b"trusted"
        }
        Err(_) => false,
    };
    // Construct a new ordinary-fetch client after the file is gone: trust is process-owned.
    let port = Network::new(Default::default(), CancellationToken::new()).unwrap();
    let second = match port
        .send(Request {
            url,
            method: "POST".into(),
            headers: vec![],
            body: Some("x".into()),
            agy: false,
            timeout_ms: 2000,
        })
        .await
    {
        Ok(response) => response.status == 200 && response.body.text().await.unwrap() == "trusted",
        Err(_) => false,
    };
    println!("TRUST_RESULT={}", serde_json::json!([first, second]));
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn scenario(reference: bool, case: &str) -> Value {
    let scratch = Scratch(std::env::temp_dir().join(format!("rpi-trust-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let path = scratch.0.join("extra.pem");
    let cert = include_bytes!("fixtures/tls/server-cert.pem");
    let malformed = b"-----BEGIN CERTIFICATE-----\nYQ==\n-----END CERTIFICATE-----\n";
    let data = match case {
        "valid" | "cached" => cert.to_vec(),
        "partial" => [cert.as_slice(), malformed].concat(),
        "invalid" => malformed.to_vec(),
        _ => vec![],
    };
    if case != "missing" {
        std::fs::write(&path, data).unwrap();
    }
    let (_, acceptor) = tls_fixture::tls();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}/json",
        listener.local_addr().unwrap().port()
    );
    let server = tokio::spawn(async move {
        let mut requests = 0;
        for _ in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            if let Ok(mut socket) = acceptor.accept(socket).await {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(socket.read_u8().await.unwrap());
                }
                assert_eq!(socket.read_u8().await.unwrap(), b'x');
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\ntrusted",
                    )
                    .await
                    .unwrap();
                let _ = socket.shutdown().await;
                requests += 1;
            }
        }
        requests
    });
    let mut command = if reference {
        let mut c =
            Command::new(std::env::var_os("RPI_REFERENCE_NODE").expect("RPI_REFERENCE_NODE"));
        c.arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/trust-reference.mjs"));
        c
    } else {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "trust_process", "--nocapture"]);
        c
    };
    for name in [
        "NODE_OPTIONS",
        "NODE_USE_SYSTEM_CA",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(name);
    }
    command
        .env("NODE_EXTRA_CA_CERTS", &path)
        .env("RPI_TRUST_TEST_URL", url)
        .env("RPI_TRUST_DELETE", if case == "cached" { "1" } else { "0" });
    if case == "unset" {
        command.env_remove("NODE_EXTRA_CA_CERTS");
    }
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("TRUST_RESULT="))
            .expect("child result"),
    )
    .unwrap();
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let trusted = matches!(case, "valid" | "partial" | "cached");
    assert_eq!(result, serde_json::json!([trusted, trusted]), "{case}");
    assert_eq!(
        requests,
        if trusted { 2 } else { 0 },
        "untrusted TLS must send zero HTTP requests"
    );
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_clients_share_validated_process_trust() {
    for case in [
        "unset", "valid", "cached", "empty", "missing", "invalid", "partial",
    ] {
        scenario(false, case).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24 and the pinned Antigravity reference"]
async fn production_trust_outcomes_match_original_transports() {
    for case in [
        "unset", "valid", "cached", "empty", "missing", "invalid", "partial",
    ] {
        assert_eq!(
            scenario(false, case).await,
            scenario(true, case).await,
            "{case}"
        );
    }
}
