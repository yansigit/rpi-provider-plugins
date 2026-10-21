use rpi_antigravity_auth::vault::{enrollment, errors::Error, policy::ConsumerError};
use serde_json::{json, Value};
fn outcome<T: serde::Serialize>(result: Result<T, ConsumerError>) -> Value {
    match result {
        Ok(value) => json!({"value":value}),
        Err(error) => {
            json!({"error":{"name":"ClaustrumConsumerError","kind":error.kind,"message":error.message}})
        }
    }
}
#[test]
fn enrollment_schema_authority_names_and_refusals_match_original() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-enrollment.json")).unwrap();
    let name = fixture["proposedName"].as_str().unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let (token, state) = (case.get("token"), case.get("state"));
        assert_eq!(
            outcome(enrollment::status(token, state, name)),
            case["result"],
            "{case}"
        );
        assert_eq!(
            json!(enrollment::authority(token, state, name)),
            case["authority"],
            "{case}"
        );
    }
    for case in fixture["names"].as_array().unwrap() {
        assert_eq!(
            outcome(enrollment::name(
                case["plugin"].as_str().unwrap(),
                case["host"].as_str().unwrap()
            )),
            case["result"],
            "{case}"
        );
    }
    for case in fixture["failures"].as_array().unwrap() {
        let action = match case["action"].as_str().unwrap() {
            "retry" => "retry",
            "gone" => "gone",
            "reauth" => "reauth",
            _ => "reduce_and_retry",
        };
        let error = Error {
            code: case["code"].as_str().unwrap().into(),
            class: "transient",
            action,
            unknown_class: None,
        };
        assert_eq!(
            enrollment::retryable(&error),
            case["result"]["disposition"] == "retry",
            "{case}"
        );
    }
}
