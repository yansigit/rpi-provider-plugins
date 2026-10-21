use rpi_antigravity_auth::vault::client;
use serde_json::{json, Value};

#[test]
fn enrolled_requests_and_replies_match_pinned_client_and_producer() {
    let f: Value = serde_json::from_str(include_str!("fixtures/vault-wire.json")).unwrap();
    for case in f["cases"].as_array().unwrap() {
        let operation = serde_json::from_value(case["operation"].clone()).unwrap();
        let request = client::request(operation, &case["input"]);
        assert_eq!(case["trace"].as_array().unwrap().len(), 1);
        assert_eq!(case["trace"][0][0], "claustrum");
        assert_eq!(request["method"], case["trace"][0][1], "{case}");
        assert_eq!(request["params"], case["trace"][0][2], "{case}");
        let (result, logs) = match client::decode(operation, &case["response"]) {
            Ok(value) => (json!({"value":value}), json!([])),
            Err(error) => (
                json!({"error":{"name":"ClaustrumCredentialError","message":error.to_string(),"code":error.code,"class":error.class,"action":error.action}}),
                json!(error.unknown_class.into_iter().collect::<Vec<_>>()),
            ),
        };
        assert_eq!(result, case["result"], "{case}");
        assert_eq!(logs, case["logs"], "{case}");
    }
}
