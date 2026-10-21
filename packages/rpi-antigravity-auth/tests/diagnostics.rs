use rpi_antigravity_auth::{redaction, sse::DecoderState};
use serde_json::{json, Value};
#[test]
fn credential_redaction_and_sse_read_boundaries_match_source() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/diagnostics.json")).unwrap();
    for name in ["cases", "frames"] {
        for case in fixture[name].as_array().unwrap() {
            let credentials: Vec<String> =
                serde_json::from_value(case["credentials"].clone()).unwrap();
            let projects: Vec<String> = serde_json::from_value(case["projects"].clone()).unwrap();
            let actual = if name == "cases" {
                redaction::text(case["text"].as_str().unwrap(), &credentials, &projects)
            } else {
                redaction::frame(case["text"].as_str().unwrap(), &credentials, &projects)
            };
            assert_eq!(actual, case["result"].as_str().unwrap(), "{case}");
        }
    }
    for case in fixture["usage"].as_array().unwrap() {
        let mut output = case["before"].clone();
        rpi_antigravity_auth::stream::update_usage(
            &case["model"],
            &mut output,
            case.get("metadata"),
        );
        assert_eq!(output, case["result"], "{case}");
    }
    for case in fixture["sse"].as_array().unwrap() {
        let mut decoder = DecoderState::default();
        let mut events = Vec::new();
        let chunks = case["chunks"].as_array().unwrap();
        for (i, bytes) in chunks.iter().enumerate() {
            decoder.push(&serde_json::from_value::<Vec<u8>>(bytes.clone()).unwrap());
            while let Some(value) = decoder.next_chunk(false) {
                events.push(json!({"value":value,"read":i+1,"eof":false}));
            }
        }
        while let Some(value) = decoder.next_chunk(true) {
            events.push(json!({"value":value,"read":chunks.len(),"eof":true}));
        }
        assert_eq!(json!(events), case["events"], "{case}");
    }
}
