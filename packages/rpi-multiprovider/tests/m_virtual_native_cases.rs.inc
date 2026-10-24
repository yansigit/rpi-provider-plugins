// Included in m_native: reuse the ABI harness, not a second retry implementation.
fn virtual_config() -> Value {
    json!({"id":"virtual-native","label":"Virtual native","models":[{"id":"probe-model","backends":[
        {"providerId":"example","modelId":"probe-model","template":{"api":"faux","baseUrl":"https://example.invalid","contextWindow":32000,"maxTokens":4096,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}},
        {"providerId":"plain","modelId":"probe-model","template":{"api":"faux","baseUrl":"https://plain.invalid","contextWindow":32000,"maxTokens":4096,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}}
    ]}]})
}
fn virtual_setup(agent: &Path) -> Registered {
    let registered = register(agent, vec![], Some(1.0));
    let fake = unsafe { &*registered.fake };
    fake.v2.store(true, std::sync::atomic::Ordering::SeqCst);
    *fake.catalog.lock().unwrap() = Some(json!({"revision":1,"providers":[
        {"id":"example","models":[{"id":"probe-model","provider":"example","name":"Real model","api":"faux","baseUrl":"https://example.invalid","contextWindow":64000,"maxTokens":8192,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]},
        {"id":"plain","models":[{"id":"probe-model","provider":"plain","name":"Plain model","api":"faux","baseUrl":"https://plain.invalid","contextWindow":32000,"maxTokens":4096,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]}
    ]}));
    virtual_store(agent, Some(virtual_config()));
    registered
}
fn virtual_store(agent: &Path, config: Option<Value>) {
    let path = agent.join("multiprovider-auth.json");
    let mut store: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    store["virtuals"] = config
        .map(|value| json!({"virtual-native":value}))
        .unwrap_or_else(|| json!({}));
    std::fs::write(path, store.to_string()).unwrap();
}
fn released_targets(fake: &Fake) -> Vec<String> {
    fake.continuations
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["action"] == "releaseTarget")
        .map(|request| request["target"].as_str().unwrap().to_owned())
        .collect()
}
fn released_receipts(fake: &Fake) -> Vec<String> {
    fake.continuations
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["action"] == "releaseAuth")
        .map(|request| request["receipt"].as_str().unwrap().to_owned())
        .collect()
}
#[test]
fn virtual_native_failover_releases_every_target_and_receipt() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    emit_event(&registered, EventTag::SessionStart);
    fake.fail_first_virtual_attempt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(message["stopReason"], "stop", "{message}");
    let delegates = fake.delegates.lock().unwrap();
    let trace: Vec<_> = delegates
        .iter()
        .map(|r| r["target"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(trace.len(), 2);
    let mut sorted = trace.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec!["target:example", "target:plain"],
        "both backends attempted exactly once"
    );
    for request in delegates.iter() {
        assert_eq!(request["version"], 2);
        assert!(request.get("authReceipt").is_some());
        assert!(request.get("model").is_none());
        assert!(request["options"].get("apiKey").is_none());
        assert!(request["options"].get("headers").is_none());
        assert!(request["options"].get("identity").is_none());
    }
    assert_eq!(released_targets(fake), trace);
    assert_eq!(
        released_receipts(fake),
        trace
            .iter()
            .map(|id| id.replace("target:", "receipt:"))
            .collect::<Vec<_>>()
    );
    drop(delegates);
    drop(registered);
}
#[test]
fn virtual_native_unsupported_v2_never_downgrades() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    fake.v2.store(false, std::sync::atomic::Ordering::SeqCst);
    emit_event(&registered, EventTag::SessionStart);
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(message["errorMessage"], "host lacks secure delegation v2");
    assert!(fake.delegates.lock().unwrap().is_empty());
    assert!(released_targets(fake).is_empty());
}
#[test]
fn virtual_native_missing_terminal_fails_and_releases() {
    virtual_missing_terminal(
        0,
        Value::Null,
        "Provider stream ended without a terminal event",
    );
}
#[test]
fn virtual_native_transport_failure_preserves_diagnostic_and_identity() {
    virtual_missing_terminal(
        1,
        json!("delegation transport refused"),
        "\"delegation transport refused\"",
    );
}
fn virtual_missing_terminal(status: i32, output: Value, diagnostic: &str) {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    emit_event(&registered, EventTag::SessionStart);
    for provider in ["example", "plain"] {
        fake.script(
            provider,
            vec![Attempt {
                frames: vec![],
                message: output.clone(),
                status,
            }],
        );
    }
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    let _: rpi_ai::AssistantMessage = serde_json::from_value(message.clone())
        .expect("fallback must be a real typed assistant message");
    assert_eq!(message["api"], "faux");
    assert_eq!(message["model"], "probe-model");
    assert!(matches!(
        message["provider"].as_str(),
        Some("example" | "plain")
    ));
    assert_eq!(message["errorMessage"], diagnostic);
    let attempts = fake.delegates.lock().unwrap();
    assert_eq!(attempts.len(), 1, "missing-terminal error is nonretryable");
    let target = attempts[0]["target"].as_str().unwrap();
    assert_eq!(released_targets(fake), vec![target]);
    assert_eq!(
        released_receipts(fake),
        vec![target.replace("target:", "receipt:")]
    );
}
#[test]
fn virtual_native_publication_ack_healing_delete_and_reregister() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    let mut config = virtual_config();
    config["models"][0]["backends"][0]
        .as_object_mut()
        .unwrap()
        .remove("template");
    virtual_store(&temp, Some(config));
    fake.fail_publication
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // The fixture event helper asserts errors absent; inspect the ABI reply here.
    let (_, handler, free, context) = registered
        .events
        .iter()
        .find(|(tag, ..)| *tag == EventTag::SessionStart as u64)
        .unwrap();
    let mut out = StbString::empty();
    handler(
        StbStringRef::from_str(
            &json!({"version":1,"tag":EventTag::SessionStart as u64}).to_string(),
        ),
        std::ptr::null(),
        &mut out,
        *context as *mut c_void,
    );
    let out = unsafe { OwnedString::from_raw(out, Some(*free)) };
    assert!(out.to_string_lossy().contains("publication refused"));
    fake.fail_publication
        .store(false, std::sync::atomic::Ordering::SeqCst);
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(
        fake.publications.lock().unwrap().len(),
        2,
        "failed publication must be retried"
    );
    let path = temp.join("multiprovider-auth.json");
    let store: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        store["virtuals"]["virtual-native"]["models"][0]["backends"][0]["template"]
            ["contextWindow"],
        64000
    );
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(
        fake.publications.lock().unwrap().len(),
        2,
        "acknowledged publication must be idempotent"
    );
    virtual_store(&temp, None);
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(
        fake.publications.lock().unwrap().last().unwrap()["action"],
        "unregister"
    );
    virtual_store(&temp, Some(virtual_config()));
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(
        fake.publications.lock().unwrap().last().unwrap()["action"],
        "register"
    );
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(message["stopReason"], "stop");
}

#[test]
fn virtual_native_recorded_pin_is_scope_local_across_reconcile_and_shutdown() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    let pin = |key: &str, backend: &str| {
        json!({
            "type":"custom", "customType":"pi-multiprovider:switch-account",
            "data":{"pool":"virtual-native::probe-model", "key":key,
                "accountId":format!("{backend}::probe-model")}
        })
    };
    emit_event(&registered, EventTag::SessionStart);
    let selected = || {
        fake.delegates.lock().unwrap().clear();
        let (message, _) = request(registered.fake, &registered, "virtual-native");
        assert_eq!(message["stopReason"], "stop", "{message}");
        let delegates = fake.delegates.lock().unwrap();
        assert_eq!(delegates.len(), 1);
        delegates[0]["target"].clone()
    };
    // Pin the opposite of the established automatic affinity, so a missing
    // restoration cannot pass just because random scheduling picked the pin.
    let backend = if selected() == "target:plain" {
        "example"
    } else {
        "plain"
    };
    let expected = format!("target:{backend}");
    fake.entries
        .lock()
        .unwrap()
        .push(pin("session-parent", backend));
    emit_event(&registered, EventTag::SessionStart);
    assert_eq!(selected(), expected);
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(selected(), expected);
    fake.inspect(Inspect {
        child: true,
        scope_id: Some("child-scope".into()),
        session_id: "session-child".into(),
    });
    let child_backend = if backend == "plain" {
        "example"
    } else {
        "plain"
    };
    *fake.entries.lock().unwrap() = vec![pin("session-child", child_backend)];
    emit_event(&registered, EventTag::SessionStart);
    assert_eq!(selected(), format!("target:{child_backend}"));
    emit_event(&registered, EventTag::SessionShutdown);
    fake.inspect(Inspect::default());
    emit_event(&registered, EventTag::BeforeAgentStart);
    assert_eq!(selected(), expected);
}

#[test]
fn virtual_native_missing_session_identity_fails_explicitly() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    fake.inspect(Inspect {
        session_id: String::new(),
        ..Inspect::default()
    });
    emit_event(&registered, EventTag::SessionStart);
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(message["errorMessage"], "multiprovider: session not ready");
    assert!(fake.delegates.lock().unwrap().is_empty());
}
