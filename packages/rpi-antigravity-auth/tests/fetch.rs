use rpi_antigravity_auth::{
    auth::{network::Network, Body, Port, Request},
    transport::network::Options,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
#[path = "common/tls.rs"]
mod tls_fixture;

#[tokio::test]
async fn oauth_fetch_bodies_headers_and_deadlines_match_node() {
    let (agy, acceptor) = tls_fixture::tls();
    let http = Network::http_builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(include_bytes!("fixtures/tls/server-cert.pem")).unwrap(),
        )
        .build()
        .unwrap();
    let port = Network::with_clients(
        http,
        agy,
        Options {
            environment: Default::default(),
            ..Options::default()
        },
    );
    let fixture: Value = serde_json::from_str(include_str!("fixtures/fetch.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = acceptor.clone();
        let input = case.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            let mut headers = serde_json::Map::new();
            for line in std::str::from_utf8(&head).unwrap().split("\r\n").skip(1) {
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.to_ascii_lowercase(), value.trim().into());
                }
            }
            headers.remove("host");
            let mut body = vec![
                0;
                headers["content-length"]
                    .as_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
            ];
            socket.read_exact(&mut body).await.unwrap();
            assert_eq!(body, b"grant_type=fixture");
            let body: Vec<u8> = serde_json::from_value(input["wireBody"].clone()).unwrap();
            let mut head = format!(
                "{}\r\n",
                input["statusLine"].as_str().unwrap_or("HTTP/1.1 200 OK")
            );
            if let Some(headers) = input["headers"].as_array() {
                for header in headers {
                    head.push_str(&format!(
                        "{}: {}\r\n",
                        header[0].as_str().unwrap(),
                        header[1].as_str().unwrap()
                    ));
                }
            }
            if let Some(coding) = input["coding"].as_str() {
                head.push_str(&format!("Content-Encoding: {coding}\r\n"));
            }
            head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            socket.write_all(head.as_bytes()).await.unwrap();
            if let Some(delay) = input["delay"].as_u64() {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
            headers
        });
        let result=async {
            let reply=port.send(Request {url:format!("https://localhost:{}/token",address.port()),method:"POST".into(),headers:vec![("Content-Type".into(),"application/x-www-form-urlencoded".into())],body:Some("grant_type=fixture".into()),agy:false,timeout_ms:case["timeout"].as_u64().unwrap_or(1000)}).await?;
            Ok::<_,String>(json!({"status":reply.status,"statusText":reply.status_text,"text":reply.body.text().await?}))
        }.await.unwrap_or_else(|error|json!({"error":error}));
        assert_eq!(result, case["result"], "{}", case["name"]);
        assert_eq!(
            json!(server.await.unwrap()),
            case["requestHeaders"],
            "headers: {}",
            case["name"]
        );
    }
}
