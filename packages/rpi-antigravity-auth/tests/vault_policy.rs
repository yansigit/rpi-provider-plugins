use rpi_antigravity_auth::vault::policy;
use serde_json::{json, Value};

#[test]
fn custody_validation_and_rotation_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-policy.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result = if case["kind"] == "inventory" {
            let value = policy::inventory(&case["input"]);
            let warnings: Vec<_> = value["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| json!(["skipped malformed vault record", v]))
                .collect();
            assert_eq!(json!(warnings), case["warnings"]);
            json!({"value":value})
        } else {
            match policy::validate_receipt(&case["identity"], &case["input"], 1000.0) {
                Ok(receipt) => {
                    assert!(!format!("{receipt:?}").contains("access_token"));
                    assert!(!receipt
                        .metadata
                        .as_object()
                        .unwrap()
                        .contains_key("accessToken"));
                    let boundary = receipt.metadata["expiresAtMs"].as_f64().unwrap() - 60000.0;
                    assert!(!receipt.usable_kept(boundary));
                    assert!(receipt.usable_kept(boundary - 1.0));
                    json!({"value":receipt.metadata,"accessToken":receipt.access_token()})
                }
                Err(error) => {
                    json!({"error":{"name":"ClaustrumConsumerError","kind":error.kind,"message":error.message}})
                }
            }
        };
        assert_eq!(result, case["result"], "{case}");
    }
    for case in fixture["failures"].as_array().unwrap() {
        assert_eq!(
            policy::unreachable(case["code"].as_str().unwrap()),
            case["value"] == "unreachable",
            "{case}"
        );
    }
    for case in fixture["rotations"].as_array().unwrap() {
        assert_eq!(
            policy::is_rotation(&case["served"], case.get("current")),
            case["value"].as_bool().unwrap(),
            "{case}"
        );
    }
}
