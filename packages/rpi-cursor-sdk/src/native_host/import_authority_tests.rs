//! Exercise the production router, not just the guard's standalone parser.
use super::*;
fn call(method: &str, input: Value) -> HostCall {
    HostCall {
        parent: "standing".into(),
        method: method.into(),
        input,
        signal: CancellationToken::new(),
    }
}
fn scoped(origin: &str, action: &str) -> HostCall {
    call(
        "scoped",
        json!({"scope":origin,"method":"native","input":{"port":"sessions","origin":origin,"action":action}}),
    )
}

#[tokio::test]
async fn imported_management_never_reaches_native_worker_views_or_journals() {
    let directory = std::env::temp_dir().join(format!(
        "cursor-authority-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("sessions.sqlite");
    let notice = crate::import_guard::FRESH_AGENT_NOTICE;
    let resume = json!({"id":"resume-1","parentId":null,"type":"custom","customType":"cursor-sdk-agent-resume","data":{
        "version":2,"runtime":"local","agentId":"agent-native","scopeKey":"fixture","cwd":"/fixture",
        "poolKey":"fixture","branchPathHash":"fixture","createdAt":"now","compactionGeneration":0,
        "sendState":{"bootstrapped":true,"contextFingerprint":"fixture","incrementalSendCount":0},
        "storeIdentity":{"version":1,"stateRoot":"/fixture"},"sessionFile":path,"sessionId":"native"}});
    let cloud = json!({"id":"cloud-1","parentId":"resume-1","type":"custom","customType":"cursor-cloud-lifecycle",
        "data":{"runtime":"cloud","action":"record","agentId":"bc-00000000-0000-0000-0000-000000000001","timestamp":"now"}});
    let cleanup = json!({"id":"cleanup-1","type":"custom","customType":"cursor-sdk-agent-cleanup",
        "data":{"action":"delete","runtime":"local","phase":"result","timestamp":"now","candidateAgentIds":["agent-native"]}});
    for marker in [
        None,
        Some(json!({"importedFrom":"pi"})),
        Some(json!({"imported":false})),
        Some(json!({"imported":{"version":99}})),
    ] {
        let mut entries = vec![resume.clone(), cloud.clone(), cleanup.clone()];
        if let Some(marker) = &marker {
            for entry in &mut entries {
                entry["data"]
                    .as_object_mut()
                    .unwrap()
                    .extend(marker.as_object().unwrap().clone());
            }
        }
        // The host serialises `SessionSnapshot` (`crates/rpi-harness/src/session/types.rs`)
        // and wraps it as `{"snapshot":…,"leafId":…,"projection":…,"branchRevision":…}`
        // (`crates/rpi-cli/src/native_sessions.rs`); the guard admits only that shape.
        let view = json!({
            "snapshot": {
                "metadata": {"id":"native","createdAt":1729000000000_i64},
                "revision": 1,
                "branches": [{"lane":"main","revision":1,"leafId":null}],
                "entries": entries,
                "name": null,
                "labels": {}
            },
            "leafId": null,
            "branchRevision": 1,
            "projection": {"entries":entries.iter().map(|entry|json!({"sourceEntry":entry})).collect::<Vec<_>>()}
        });
        let trace = Arc::new(Mutex::new(Vec::new()));
        let request: Request = Arc::new({
            let path = path.clone();
            let trace = trace.clone();
            move |input, _| {
                let path = path.clone();
                let view = view.clone();
                let trace = trace.clone();
                Box::pin(async move {
                    trace.lock().unwrap().push(input.clone());
                    Ok(match input["action"].as_str() {
                        Some("capture") => {
                            json!({"origin":"origin","snapshot":{"metadata":{"id":"native","createdAt":1729000000000_i64},"revision":0,"branches":[],"entries":[],"name":null,"labels":{}}})
                        }
                        Some("view") => view,
                        Some("location") => json!({"path":path}),
                        _ => Value::Null,
                    })
                })
            }
        });
        let router = Arc::new(Router::default());
        let bound = router.bind(Some(Invocation {
            live: request.clone(),
            persistence: request,
        }));
        bound(call(
            "native",
            json!({"port":"sessions","action":"capture"}),
        ))
        .await
        .unwrap();
        let projected = router.standing()(scoped("origin", "view")).await.unwrap();
        for (original, entry) in [resume.clone(), cloud.clone(), cleanup.clone()]
            .iter()
            .zip(projected["snapshot"]["entries"].as_array().unwrap())
        {
            assert_eq!(
                entry["customType"] == original["customType"],
                marker.is_none(),
                "{entry}"
            );
            assert_eq!(
                entry["id"], original["id"],
                "history/tree continuity survives refusal"
            );
        }
        for (entry, projected_entry) in projected["snapshot"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .zip(projected["projection"]["entries"].as_array().unwrap())
        {
            assert_eq!(
                entry, &projected_entry["sourceEntry"],
                "branch projection must not retain stripped authority"
            );
        }
        assert_eq!(
            trace
                .lock()
                .unwrap()
                .iter()
                .any(|call| call["text"] == notice),
            marker.is_some()
        );
        let frame = json!({"version":1,"runtime":"cloud","action":"record","agentId":"bc-00000000-0000-0000-0000-000000000001","timestamp":"now",
            "sessionFile":path,"sessionId":"native","anchorEntryId":"cloud-1"});
        let mut frame = frame;
        if let Some(marker) = &marker {
            frame
                .as_object_mut()
                .unwrap()
                .extend(marker.as_object().unwrap().clone());
        }
        let hash = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(b"native"))
        };
        let ledger = directory.join(format!(".cursor-cloud-lifecycle-{}.journal", &hash[..32]));
        std::fs::write(&ledger, format!("{frame}\n")).unwrap();
        let opened = router.standing()(call(
            "scoped",
            json!({"scope":"origin","method":"journals","input":{
            "origin":"origin","action":"open","kind":"cloud"}}),
        ))
        .await;
        if marker.is_some() {
            assert_eq!(opened.unwrap_err(), notice);
        } else {
            assert!(opened.is_ok(), "{opened:?}");
        }
        router.close().await.unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}
