//! Linux/macOS/Windows executable acceptance; never contacts a real account or vault.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path.as_ref(), serde_json::to_vec(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
struct Rpc {
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: tokio::sync::mpsc::UnboundedReceiver<Value>,
    stderr: Arc<Mutex<String>>,
    trace: Vec<Value>,
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Rpc {
    fn spawn(agent: &Path, connection: &Path, reference: bool) -> Self {
        let mut command = if reference {
            let mut c =
                Command::new(std::env::var_os("RPI_REFERENCE_NODE").expect("RPI_REFERENCE_NODE"));
            c.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/native-menu-reference.ts"));
            c
        } else {
            let mut c = Command::new(std::env::var_os("RPI_TEST_HOST").expect("RPI_TEST_HOST"));
            c.args([
                "--mode",
                "rpc",
                "--no-session",
                "--no-tools",
                "--provider",
                "google",
                "--model",
                "antigravity-gemini-3.8-flash",
                "--extension",
            ])
            .arg(std::env::var_os("RPI_TEST_PROVIDER_LIBRARY").expect("RPI_TEST_PROVIDER_LIBRARY"));
            c
        };
        for (key, _) in std::env::vars()
            .filter(|(key, _)| key.ends_with("_API_KEY") || key.ends_with("_AUTH_TOKEN"))
        {
            command.env_remove(key);
        }
        let mut child = command
            .current_dir(agent)
            .env("RPI_CODING_AGENT_DIR", agent)
            .env("RPI_OFFLINE", "1")
            .env("SUBC_CONNECTION_FILE", connection)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let errors = child.stderr.take().unwrap();
        let (send, incoming) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if send.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let captured = stderr.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(errors).lines() {
                captured.lock().unwrap().push_str(&line.unwrap());
            }
        });
        Self {
            child,
            stdin,
            incoming,
            stderr,
            trace: vec![],
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{value}").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }
    async fn next(&mut self) -> Value {
        let value = tokio::time::timeout(Duration::from_secs(20), self.incoming.recv())
            .await
            .expect("RPC timed out")
            .unwrap_or_else(|| panic!("RPC exited: {}", self.stderr.lock().unwrap()));
        assert_ne!(value["type"], "agent_start");
        if value["type"] == "ui_dialog_request" {
            let ui = &value["ui"]["nativeDialog"];
            self.trace.push(if ui["kind"] == "confirm" {
                json!(["confirm", ui["title"], ui["description"]])
            } else {
                json!([
                    "select",
                    ui["title"],
                    ui["choices"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v["label"].clone())
                        .collect::<Vec<_>>()
                ])
            });
        } else if value["type"] == "ui_notification" {
            self.trace
                .push(json!(["notify", value["text"], value["kind"]]));
        }
        value
    }
    async fn event(&mut self, kind: &str) -> Value {
        loop {
            let value = self.next().await;
            if value["type"] == kind {
                return value;
            }
        }
    }
    async fn dialog(&mut self) -> Value {
        self.event("ui_dialog_request").await
    }
    fn choose(&mut self, dialog: &Value, label: Option<&str>) {
        let answer = label.map(|label| {
            let choice = dialog["ui"]["nativeDialog"]["choices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["label"] == label)
                .unwrap_or_else(|| panic!("missing {label}: {dialog}"));
            json!({"type":"select","values":[choice["value"]]})
        });
        self.send(json!({"type":"ui_dialog_response","requestId":dialog["requestId"],"cancelled":answer.is_none(),"answer":answer}));
    }
    async fn root_vault(&mut self) {
        self.send(json!({"type":"extension_command","id":"menu","name":"antigravity"}));
        let root = self.dialog().await;
        let label = root["ui"]["nativeDialog"]["choices"][4]["label"]
            .as_str()
            .unwrap();
        self.choose(&root, Some(label));
    }
    async fn close_menu(&mut self, current: Value) {
        self.choose(&current, None);
        let root = self.dialog().await;
        self.choose(&root, None);
        loop {
            let response = self.next().await;
            if response["type"] == "response" && response["id"] == "menu" {
                assert_eq!(response["success"], true, "{response}");
                break;
            }
        }
    }
    async fn finish(&mut self) {
        self.stdin.take();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success(), "{}", self.stderr.lock().unwrap());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("worker did not exit");
    }
}

async fn scenario(base: &Path, reference: bool) -> (Vec<Value>, Vec<String>, Vec<Value>) {
    let agent = base.join(if reference { "reference" } else { "native" });
    std::fs::create_dir(&agent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let original = json!({"google":{"type":"oauth","refresh":"fixture-local-refresh","access":"fixture-local-access","expires":9_999_999_999u64},"untouched":{"key":"fixture-other-key"}});
    write(agent.join("auth.json"), &original);
    let vault = agent.join("antigravity-auth-vault");
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
    let connection = agent.join("connection.json");
    subc_transport::connection_file::write_atomic(&connection, &info).unwrap();
    let approved = Arc::new(AtomicBool::new(false));
    let decision = approved.clone();
    let methods = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests = methods.clone();
    let stopped = CancellationToken::new();
    let stop = stopped.clone();
    let files = agent.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut stream, _) = tokio::select! { _=stop.cancelled()=>break, value=listener.accept()=>value.unwrap() };
            let decision = decision.clone();
            let requests = requests.clone();
            let files = files.clone();
            clients.spawn(async move {
                stream.set_nodelay(true).unwrap();authenticate_server(&mut stream,&[7;32],&[9;16],"fixture",Duration::from_secs(10)).await.unwrap();
                while let Some(frame)=read_frame(&mut stream).await.unwrap() {
                    let h=frame.header;if h.ty==FrameType::Goodbye{break;}assert_eq!(h.ty,FrameType::Request);
                    let body: Value=serde_json::from_slice(&frame.body).unwrap();
                    let reply=if h.channel==0 {json!({"op":"route.open","route_channel":17,"route_epoch":3})} else {
                        let method=body["method"].as_str().unwrap();requests.lock().unwrap().push(method.into());
                        let state_path=files.join("antigravity-auth-vault/pi-enrollment-state.json");
                        let result=match method {
                            "auth.enroll_propose"=>{
                                let state=read(&state_path);let secret=state["requestSecret"].as_str().unwrap();
                                let bytes:Vec<u8>=(0..secret.len()).step_by(2).map(|i|u8::from_str_radix(&secret[i..i+2],16).unwrap()).collect();
                                assert_eq!(body["params"]["request_secret_hash"],format!("{:x}",Sha256::digest(bytes)));
                                assert_eq!(body["params"]["proposed_name"],"antigravity-auth-pi");
                                json!({"request_id":"fixture-request"})
                            },
                            "auth.enroll_poll"=>{
                                assert_eq!(read(&state_path)["requestSecret"],body["params"]["request_secret"]);
                                if decision.load(Ordering::Acquire) {json!({"status":"approved","name":"fixture-approved","token":"b".repeat(64),"token_generation":1})}else{json!({"status":"pending"})}
                            },
                            "credential.list_scoped"=>{
                                assert_eq!(read(files.join("auth.json"))["google"]["key"],"antigravity-vault-custody","custody request before marker");
                                assert!(files.join("antigravity-auth-vault/google-login-stash.json").exists(),"custody request before backup");
                                assert_eq!(body["params"]["enrollment_token"],"b".repeat(64));
                                json!({"view":"fixture-view","grants":1,"credentials":[{"id":"fixture-private-route","type":"oauth","state":"ready","record_version":1,"categories":["antigravity-native"],"serves":["antigravity-native"],"provider_ids":["google"],"operations":["read"],"refresh_adapter":"antigravity","auth_method":"antigravity","email":"private@example.test","account_id":"fixture-account"}]})
                            },
                            _=>panic!("unexpected operation: {method}"),
                        };json!({"result":result})
                    };
                    write_frame(&mut stream,&Frame::build(FrameType::Response,Flags::new(false,Priority::Interactive,false),h.channel,h.epoch,h.corr,serde_json::to_vec(&reply).unwrap()).unwrap()).await.unwrap();stream.flush().await.unwrap();
                }
            });
        }
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    });
    let mut rpc = Rpc::spawn(&agent, &connection, reference);
    rpc.event("ready").await;
    rpc.root_vault().await;
    let menu = rpc.dialog().await;
    rpc.choose(&menu, Some("Request vault access"));
    let notice = rpc.event("ui_notification").await;
    assert_eq!(notice["kind"], "info", "{notice}");
    let menu = rpc.dialog().await;
    rpc.close_menu(menu).await;
    // Close the command first; the watch must retain its own host context and avoid reproposing.
    tokio::time::timeout(Duration::from_secs(5), async {
        while methods
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.as_str() == "auth.enroll_poll")
            .count()
            < 2
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    approved.store(true, Ordering::Release);
    let notice = rpc.event("ui_notification").await;
    assert_eq!(notice["kind"], "info", "{notice}");
    assert!(notice["text"].as_str().unwrap().contains("approved"));
    assert_eq!(
        read(agent.join("auth.json"))["google"],
        json!({"type":"api_key","key":"antigravity-vault-custody"})
    );
    let mut snapshots = vec![read(vault.join("pi-mode.json"))];
    rpc.root_vault().await;
    let mut menu = rpc.dialog().await;
    for action in ["Use this account", "Turn off for Pi", "Turn back on for Pi"] {
        rpc.choose(&menu, Some("Vault account 1"));
        let actions = rpc.dialog().await;
        rpc.choose(&actions, Some(action));
        let notice = rpc.event("ui_notification").await;
        assert_eq!(notice["kind"], "info", "{notice}");
        snapshots.push(read(vault.join("pi-mode.json")));
        menu = rpc.dialog().await;
    }
    rpc.choose(&menu, Some("Disconnect from the vault"));
    let confirm = rpc.dialog().await;
    rpc.send(json!({"type":"ui_dialog_response","requestId":confirm["requestId"],"answer":{"type":"confirm","confirmed":false}}));
    let notice = rpc.event("ui_notification").await;
    assert_eq!(notice["text"], "Cancelled.");
    assert!(vault.join("pi-enrollment.json").exists());
    menu = rpc.dialog().await;
    rpc.choose(&menu, Some("Disconnect from the vault"));
    let confirm = rpc.dialog().await;
    rpc.send(json!({"type":"ui_dialog_response","requestId":confirm["requestId"],"answer":{"type":"confirm","confirmed":true}}));
    let notice = rpc.event("ui_notification").await;
    assert_eq!(notice["kind"], "info", "{notice}");
    assert_eq!(read(agent.join("auth.json")), original);
    assert!(!vault.join("pi-enrollment.json").exists());
    assert!(!vault.join("google-login-stash.json").exists());
    snapshots.push(read(vault.join("pi-mode.json")));
    menu = rpc.dialog().await;
    rpc.close_menu(menu).await;
    rpc.finish().await;
    stopped.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
    let text = serde_json::to_string(&rpc.trace).unwrap();
    for secret in [
        "fixture-local-refresh",
        "fixture-local-access",
        "fixture-private-route",
        "private@example.test",
        "fixture-other-key",
        &"b".repeat(64),
    ] {
        assert!(!text.contains(secret), "private data in UI");
    }
    let calls = methods.lock().unwrap().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|v| v.as_str() == "auth.enroll_propose")
            .count(),
        1
    );
    (rpc.trace.clone(), calls, snapshots)
}

#[tokio::test]
#[ignore = "requires built native library/host, Node 24 and pinned reference dependencies"]
async fn loaded_enrollment_watch_account_controls_and_restoration_match_pi() {
    let parent = PathBuf::from(std::env::var_os("RPI_PRIVATE_TMP").expect("RPI_PRIVATE_TMP"));
    let root = Scratch(parent.join(format!("native-menu-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&root.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(90), async {
        let reference = scenario(&root.0, true).await;
        let native = scenario(&root.0, false).await;
        assert_eq!(native, reference);
    })
    .await
    .expect("loaded menu acceptance timed out");
}
