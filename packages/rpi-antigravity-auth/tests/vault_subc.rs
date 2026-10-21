use rpi_antigravity_auth::vault::{
    client::Operation,
    connection::{Enrolled, SubcConnector},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use subc_protocol::{BindIdentity, Flags, Frame, FrameType, Priority};
use subc_transport::{authenticate_server, read_frame, write_frame};
use tokio::io::AsyncWriteExt;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn scratch() -> Scratch {
    // The transport deliberately refuses insecure ancestors; use the private test root.
    let parent = std::env::var_os("RPI_PRIVATE_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&parent).unwrap();
    let path = parent.join(format!("vault-subc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    Scratch(path)
}

#[tokio::test]
async fn authenticated_frames_binary_replies_and_transport_errors_match_original() {
    tokio::time::timeout(Duration::from_secs(30),async {
        let fixture: Value=serde_json::from_str(include_str!("fixtures/vault-subc.json")).unwrap();
        let scratch=scratch();
        for case in fixture["cases"].as_array().unwrap() {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let info=subc_transport::connection_file::ConnectionInfo {
                schema:1,wire_version:Some(subc_protocol::PROTOCOL_VERSION),
                endpoints:vec![subc_transport::connection_file::Endpoint {host:"127.0.0.1".into(),port:listener.local_addr().unwrap().port()}],
                key:vec![7;32],daemon_id:[9;16],pid:std::process::id(),daemon_ver:"fixture".into(),
            };
            let connection_file=scratch.0.join("connection.json");
            subc_transport::connection_file::write_atomic(&connection_file,&info).unwrap();
            let connections=Arc::new(Mutex::new(Vec::<Vec<Value>>::new()));
            let trace=connections.clone();let spec=case["spec"].clone();
            let stop=tokio_util::sync::CancellationToken::new();let stopped=stop.clone();
            let server=tokio::spawn(async move {
                let mut clients=tokio::task::JoinSet::new();
                loop {
                    let (mut stream,_)=tokio::select! { _=stopped.cancelled()=>break, value=listener.accept()=>value.unwrap() };
                    let index={let mut trace=trace.lock().unwrap();let index=trace.len();trace.push(vec![]);index};
                    let trace=trace.clone();let spec=spec.clone();
                    clients.spawn(async move {
                        stream.set_nodelay(true).unwrap();
                        let auth=authenticate_server(&mut stream,&[7;32],&[9;16],"fixture",Duration::from_secs(10)).await.unwrap();
                        assert_eq!(auth.role,"client");
                        while let Some(request)=read_frame(&mut stream).await.unwrap() {
                            let h=request.header;
                            trace.lock().unwrap()[index].push(json!({"type":h.ty as u8,"flags":h.flags.0,"channel":h.channel,"epoch":h.epoch,"corr":h.corr.to_string(),"body":if request.body.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&request.body).unwrap()}}));
                            if h.ty==FrameType::Goodbye {break;}
                            assert_eq!(h.ty,FrameType::Request);
                            let (ty,body,binary)=if h.channel==0 {
                                (FrameType::Response,serde_json::to_vec(&json!({"op":"route.open","route_channel":17,"route_epoch":3})).unwrap(),false)
                            } else {
                                let ty=spec["type"].as_u64().map(|v| if v==FrameType::StreamEnd as u64 {FrameType::StreamEnd}else if v==FrameType::Goodbye as u64 {FrameType::Goodbye}else{FrameType::Error}).unwrap_or(FrameType::Response);
                                (ty,serde_json::from_value::<Vec<u8>>(spec["body"].clone()).unwrap(),spec["binary"].as_bool().unwrap_or(false))
                            };
                            write_frame(&mut stream,&Frame::build(ty,Flags::new(binary,Priority::Interactive,false),h.channel,h.epoch,if ty==FrameType::Goodbye {0}else{h.corr},body).unwrap()).await.unwrap();
                            stream.flush().await.unwrap();
                        }
                    });
                }
                while let Some(result)=clients.join_next().await {result.unwrap();}
            });
            let client=Enrolled::connect(SubcConnector {connection_file,identity:BindIdentity::new("/fixture/project","opencode","store-fixture"),handshake_timeout:Duration::from_secs(10)}).await.unwrap();
            let result=match client.call(Operation::List,json!({"enrollmentToken":"fixture-token"})).await {
                Ok(value)=>json!({"value":value}),
                Err(e)=>json!({"error":{"code":e.code,"class":e.class,"action":e.action,"message":e.to_string()}}),
            };
            client.close().await;stop.cancel();server.await.unwrap();
            assert_eq!(result,case["result"],"{}",case["spec"]["name"]);
            assert_eq!(json!(*connections.lock().unwrap()),case["connections"],"{}",case["spec"]["name"]);
        }
    }).await.expect("framed acceptance timed out");
}
