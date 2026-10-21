use base64::{engine::general_purpose::STANDARD, Engine};
use rpi_cursor_sdk::journal::port::JournalPort;
use rpi_cursor_sdk::worker::{HostHandler, Worker, WorkerConfig};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires the built pinned Cursor reference and Node 24+"]
async fn retained_node_journal_bytes_match_native_writes_and_torn_tail_rules() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let reference = std::env::var_os("RPI_CURSOR_REFERENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| package.join("../../../provider-references/cursor"));
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!(
        "rpi-journal-reference-{:x}",
        u128::from_ne_bytes(random)
    ));
    std::fs::create_dir(&root).unwrap();
    let prefixes: Vec<Option<&[u8]>> = vec![
        None,
        Some(b""),
        Some(b"{}\n"),
        Some(b"{\"torn\":"),
        Some(b"{}"),
        Some(b"broken\n"),
        Some(b"\n"),
        Some(b"{}\r\n"),
    ];
    let cases: Vec<Value> = ["usage", "cloud"].into_iter().flat_map(|kind| prefixes.iter().map(move |prefix| {
        json!({"kind":kind,"prefix":prefix.map(|bytes| STANDARD.encode(bytes)),"records":if kind == "cloud" {
            json!([{"agentId":"bc-00000000-0000-0000-0000-000000000001","runId":"run-é","branches":[]}])
        } else { json!([{"message":"é😀","nested":[true,null,1]}, {"next":true}]) }})
    })).collect();
    let mut child =
        Command::new(std::env::var_os("RPI_CURSOR_NODE").unwrap_or_else(|| "node".into()))
            .arg(package.join("tests/journal-reference.mjs"))
            .arg(reference)
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(results.len(), cases.len());
    let hash = format!("{:x}", Sha256::digest(b"fixture"));
    let mut paths = vec![];
    let mut ports = vec![];
    for (index, case) in cases.iter().enumerate() {
        let directory = root.join(format!("native-{index}"));
        std::fs::create_dir(&directory).unwrap();
        let prefix = if case["kind"] == "cloud" {
            ".cursor-cloud-lifecycle"
        } else {
            "cursor-usage"
        };
        let path = directory.join(format!("{prefix}-{}.journal", &hash[..32]));
        if let Some(prefix) = case["prefix"].as_str() {
            std::fs::write(&path, STANDARD.decode(prefix).unwrap()).unwrap();
        }
        ports.push(JournalPort::for_session(&directory.join("session.jsonl"), "fixture").unwrap());
        paths.push(path);
    }
    let ports = Arc::new(ports);
    let host: HostHandler = {
        let ports = Arc::clone(&ports);
        Arc::new(move |call| {
            let ports = Arc::clone(&ports);
            Box::pin(async move {
                assert_eq!(call.method, "fixture.journal");
                tokio::task::spawn_blocking(move || {
                    ports[call.input["index"].as_u64().unwrap() as usize]
                        .request(call.input["request"].clone())
                })
                .await
                .unwrap()
            })
        })
    };
    let worker = Worker::spawn(
        WorkerConfig {
            node: std::env::var_os("RPI_CURSOR_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: package.join("worker/test/rpi/fixture-worker.mjs"),
            cwd: root.clone(),
        },
        host,
    )
    .await
    .unwrap();
    for (index, (case, result)) in cases.iter().zip(&results).enumerate() {
        let expected = STANDARD.decode(result["bytes"].as_str().unwrap()).unwrap();
        let records = if case["kind"] == "cloud" {
            // The original converted cloud record isolates file IO from the separately retained adapter policy.
            let frame = expected
                .split_inclusive(|byte| *byte == b'\n')
                .last()
                .unwrap();
            json!([serde_json::from_slice::<Value>(frame).unwrap()])
        } else {
            case["records"].clone()
        };
        let outcomes = worker
            .call(
                "journalSequence",
                json!({"index":index,"kind":case["kind"],"records":records}),
            )
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(outcomes, result["outcomes"], "outcome case {index}");
        assert_eq!(
            std::fs::read(&paths[index]).unwrap_or_default(),
            expected,
            "bytes case {index}"
        );
    }
    for port in ports.iter() {
        port.close();
    }
    worker.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
