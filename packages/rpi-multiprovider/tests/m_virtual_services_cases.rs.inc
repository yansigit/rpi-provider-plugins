fn wait_fabric(fake: &Fake, method: &str) {
    let calls = fake.fabric_calls.lock().unwrap();
    let (calls, timeout) = fake
        .fabric_wake
        .wait_timeout_while(calls, std::time::Duration::from_secs(5), |calls| {
            !calls.iter().any(|call| call["method"] == method)
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "native Fabric never reached {method}: {calls:?}"
    );
}
#[test]
fn virtual_native_capable_service_compacts_and_shutdown_drains() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    fake.fabric_capable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    fake.fabric_idle
        .store(true, std::sync::atomic::Ordering::SeqCst);
    emit_event(&registered, EventTag::SessionStart);
    fake.fail_first_virtual_attempt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(
        message["stopReason"], "error",
        "capable Fabric surfaces failure instead of inline failover"
    );
    assert_eq!(fake.delegates.lock().unwrap().len(), 1);
    wait_fabric(fake, "compact");
    emit_event(&registered, EventTag::SessionShutdown);
    let calls = fake.fabric_calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["method"] == "compact")
            .count(),
        1
    );
    assert!(calls
        .iter()
        .all(|call| call["service"] == "pi-fabric" && call["serviceVersion"] == 1));
    assert!(fake
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["message"]
            == "multiprovider: compacted session context before account failover"));
    assert_eq!(released_targets(fake).len(), 1);
    assert_eq!(released_receipts(fake).len(), 1);
}
#[test]
fn virtual_native_delayed_compaction_is_cancelled_and_drained_on_shutdown() {
    let temp = tempfile_dir();
    let registered = virtual_setup(&temp);
    let fake = unsafe { &*registered.fake };
    fake.fabric_capable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    emit_event(&registered, EventTag::SessionStart);
    fake.fail_first_virtual_attempt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (message, _) = request(registered.fake, &registered, "virtual-native");
    assert_eq!(message["stopReason"], "error");
    wait_fabric(fake, "isIdle");
    emit_event(&registered, EventTag::SessionShutdown);
    assert!(!fake
        .fabric_calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["method"] == "compact"));
    assert!(!fake
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["message"]
            == "multiprovider: compacted session context before account failover"));
}
