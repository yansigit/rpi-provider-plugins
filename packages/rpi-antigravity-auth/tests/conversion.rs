use rpi_antigravity_auth::{convert, schema};
use serde_json::{json, Value};

#[test]
fn request_and_schema_match_original_tests_with_pi_1_3() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/conversion.json")).unwrap();
    assert_eq!(
        fixture["reference"],
        "a97f9055017f3c1546afbfe61a19d50b428be3d8"
    );
    assert_eq!(fixture["host"], "1.3.0");
    let mut counts = (0, 0);
    for trace in fixture["traces"].as_array().unwrap() {
        let args = trace["args"].as_array().unwrap();
        let options = args
            .get(1)
            .and_then(|arg| arg.get("value"))
            .cloned()
            .unwrap_or(json!({}));
        let actual = match trace["function"].as_str().unwrap() {
            "toGeminiSchema" => {
                counts.0 += 1;
                match args[0].get("value") {
                    Some(value) => {
                        json!({"value": schema::to_gemini_schema(value, serde_json::from_value(options).unwrap())})
                    }
                    None => json!({"undefined": true}),
                }
            }
            "buildGeminiRequest" => {
                counts.1 += 1;
                json!({"value": convert::build_gemini_request(&args[0]["value"], serde_json::from_value(options).unwrap()).unwrap()})
            }
            other => panic!("Unknown fixture function: {other}"),
        };
        assert_eq!(
            actual, trace["result"],
            "{} {}: {}",
            trace["caller"], trace["scenario"], trace["args"]
        );
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&trace["result"]).unwrap(),
            "field order: {} {}",
            trace["caller"],
            trace["scenario"]
        );
    }
    assert_eq!(counts, (803, 52));
}
