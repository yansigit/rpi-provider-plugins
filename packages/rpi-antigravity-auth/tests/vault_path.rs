use rpi_antigravity_auth::vault::path;
use serde_json::{json, Value};

#[test]
fn request_admission_and_account_lists_match_the_original_pi_port() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-path.json")).unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let result = path::require_custody(&case["read"]).and_then(|record| {
            path::select_route(
                case["routes"].as_array().unwrap(),
                record.get("routeId"),
                case["cause"].as_str().unwrap(),
            )
        });
        let value = match result {
            Ok(v) => json!({"value":v}),
            Err(e) => json!({"error":e}),
        };
        assert_eq!(value, case["result"], "selection {i}");
    }
    for (i, case) in fixture["choices"].as_array().unwrap().iter().enumerate() {
        let (references, choices) = path::account_choices(Some(&case["roster"]));
        assert_eq!(json!(choices), case["result"], "choices {i}");
        let selected: Vec<_> = choices
            .iter()
            .map(|choice| {
                references
                    .iter()
                    .rev()
                    .find(|reference| reference["routeId"] == choice["routeId"])
                    .unwrap()
            })
            .collect();
        assert_eq!(json!(selected), case["references"], "references {i}");
    }
    for case in fixture["guidance"].as_array().unwrap() {
        let cause = if case["reach"].is_null() {
            "unknown"
        } else if case["reach"]["grants"] == 0 {
            "no-grant"
        } else {
            "grant-reaches-nothing"
        };
        assert_eq!(json!(path::empty_guidance(cause)), case["result"]);
    }
}
