use rpi_multiprovider::retry::{is_retryable_assistant_error, AssistantMessage};
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("retry-reference.json");

/// Every classifier case runs through Pi 1.3.0's real `isRetryableAssistantError`
/// in the reference script; the Rust port must agree on the complete case object.
#[test]
fn classifier_cases_match_the_upstream_reference() {
    let reference: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = reference["classifier"].as_array().unwrap();
    assert!(!cases.is_empty(), "the reference records classifier cases");
    for case in cases {
        let message: AssistantMessage = serde_json::from_value(case["message"].clone()).unwrap();
        let actual = json!({
            "name": case["name"],
            "message": serde_json::to_value(&message).unwrap(),
            "result": is_retryable_assistant_error(&message),
        });
        assert_eq!(actual, *case, "{}", case["name"]);
    }
}

/// The two upstream lists are ordered and overlapping; the non-retryable list
/// wins, and only an `error` stop reason with a non-empty message is considered.
#[test]
fn quota_precedence_and_stop_reason_guards_hold() {
    let message = |stop_reason: &str, error_message: Option<&str>| AssistantMessage {
        stop_reason: stop_reason.to_owned(),
        error_message: error_message.map(str::to_owned),
    };
    assert!(is_retryable_assistant_error(&message(
        "error",
        Some("HTTP 503: service unavailable")
    )));
    assert!(!is_retryable_assistant_error(&message(
        "error",
        Some("429 insufficient_quota")
    )));
    assert!(!is_retryable_assistant_error(&message("stop", Some("HTTP 503"))));
    assert!(!is_retryable_assistant_error(&message("error", None)));
    assert!(!is_retryable_assistant_error(&message("error", Some(""))));
}
