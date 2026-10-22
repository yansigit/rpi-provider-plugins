//! Node/V8 `JSON.parse` diagnostics used by the OAuth and project ports.
//! Compares every corpus input to the message Node 24.22.0 raised.
use rpi_antigravity_auth::auth::json_error;
use serde_json::Value;

#[test]
fn json_parse_diagnostics_match_node() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/ag1-json-errors.json")).unwrap();
    let mut malformed = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let actual = json_error::parse_error(input);
        match case["message"].as_str() {
            Some(message) => {
                malformed += 1;
                assert_eq!(actual.as_deref(), Some(message), "input: {input:?}");
            }
            None => assert_eq!(actual, None, "expected valid JSON: {input:?}"),
        }
    }
    assert_eq!(
        malformed,
        fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| !case["message"].is_null())
            .count()
    );
}
