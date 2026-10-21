//! Real native host, enrolled Subc transport and HTTP/1.1 transport together.
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use subc_protocol::{Flags, Frame, FrameType, Priority};
use subc_transport::{authenticate_server, read_frame, write_frame};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires built host/library and Python 3; no live credentials or producer"]
async fn loaded_vault_receipts_rotation_refusal_and_reports() {
    let root = Scratch(
        PathBuf::from(std::env::var_os("RPI_PRIVATE_TMP").expect("RPI_PRIVATE_TMP"))
            .join(format!("vault-native-send-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&root.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for case in [
        "success",
        "rotated",
        "same-version",
        "terminal-401",
        "identity",
        "project-missing",
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let info = subc_transport::connection_file::ConnectionInfo {
            schema: 1,
            wire_version: Some(subc_protocol::PROTOCOL_VERSION),
            endpoints: vec![subc_transport::connection_file::Endpoint {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
            }],
            key: vec![7; 32],
            daemon_id: [9; 16],
            pid: std::process::id(),
            daemon_ver: "fixture".into(),
        };
        let connection = root.0.join("connection.json");
        subc_transport::connection_file::write_atomic(&connection, &info).unwrap();
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let trace = calls.clone();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let _cancel = stop.clone().drop_guard();
        let server = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = tokio::select! {_=stopped.cancelled()=>break,accepted=listener.accept()=>accepted.unwrap()};
                let trace = trace.clone();
                clients.spawn(async move {
                    stream.set_nodelay(true).unwrap();
                    authenticate_server(&mut stream,&[7;32],&[9;16],"fixture",Duration::from_secs(10)).await.unwrap();
                    while let Some(frame)=read_frame(&mut stream).await.unwrap() {
                        let h=frame.header;if h.ty==FrameType::Goodbye {break;}assert_eq!(h.ty,FrameType::Request);
                        let body:Value=serde_json::from_slice(&frame.body).unwrap();
                        let reply=if h.channel==0 {json!({"op":"route.open","route_channel":17,"route_epoch":3})} else {
                            let method=body["method"].as_str().unwrap();
                            assert_eq!(body["params"]["enrollment_token"],"a".repeat(64));
                            let get_count={let mut calls=trace.lock().unwrap();calls.push(body.clone());calls.iter().filter(|call|call["method"]=="credential.get_scoped").count()};
                            let result=match method {
                                "credential.list_scoped"=>json!({"view":"fixture-view","grants":1,"credentials":[{"id":"fixture-private-route","type":"oauth","state":"active","record_version":1,"categories":["antigravity-native"],"serves":["antigravity-native"],"provider_ids":["google"],"operations":["read"],"refresh_adapter":"antigravity","auth_method":"antigravity","account_id":"fixture-account"}]}),
                                "credential.get_scoped"=>{
                                    assert_eq!(body["params"]["credential_id"],"fixture-private-route");
                                    let version=if case=="same-version"{1}else{get_count};
                                    let mut value=json!({"payload":format!("fixture-private-access-{get_count}").as_bytes(),"record_version":version,"expires_at_ms":4_102_444_800_000u64,"credential_id":"fixture-private-route","account_id":if case=="identity" {"wrong-account"}else{"fixture-account"},"project_id":format!("fixture-private-managed-{get_count}")});
                                    if case=="project-missing" {value.as_object_mut().unwrap().remove("project_id");}
                                    value
                                },
                                "credential.report_auth_failure"=>{
                                    assert_eq!(body["params"]["credential_id"],"fixture-private-route");
                                    assert_eq!(body["params"]["provider_status"],401);
                                    assert_eq!(body["params"]["record_version"],if case=="same-version" {1}else{2});
                                    json!({"accepted":true})
                                },
                                _=>panic!("unexpected producer method {method}"),
                            };
                            json!({"result":result})
                        };
                        write_frame(&mut stream,&Frame::build(FrameType::Response,Flags::new(false,Priority::Interactive,false),h.channel,h.epoch,h.corr,serde_json::to_vec(&reply).unwrap()).unwrap()).await.unwrap();stream.flush().await.unwrap();
                    }
                });
            }
            while let Some(client) = clients.join_next().await {
                client.unwrap();
            }
        });
        let output = tokio::task::spawn_blocking(move || {
            Command::new(
                std::env::var_os("RPI_REFERENCE_PYTHON").unwrap_or_else(|| "python3".into()),
            )
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/native-vault-inference.py"),
            )
            .env("SUBC_CONNECTION_FILE", connection)
            .env("RPI_VAULT_SEND_CASE", case)
            .output()
            .unwrap()
        })
        .await
        .unwrap();
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let methods: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["method"].as_str().unwrap().to_owned())
            .collect();
        let mut expected = vec!["credential.list_scoped", "credential.get_scoped"];
        if matches!(case, "rotated" | "same-version" | "terminal-401") {
            expected.push("credential.get_scoped");
        }
        if matches!(case, "same-version" | "terminal-401") {
            expected.push("credential.report_auth_failure");
        }
        assert_eq!(methods, expected, "{case}");
    }
}
