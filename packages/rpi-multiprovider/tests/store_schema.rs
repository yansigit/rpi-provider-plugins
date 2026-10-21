use rpi_multiprovider::store::schema;
use serde_json::{json, Value};

#[test]
fn persisted_schema_and_virtual_normalization_match_pinned_store() {
    let fixture: Value = serde_json::from_str(include_str!("store-reference.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        if let Some(initial) = case.get("initial") {
            match schema::state(initial) {
                Ok(()) => assert!(
                    case["results"][0].get("error").is_none(),
                    "{}",
                    case["name"]
                ),
                Err(error) => assert_eq!(
                    json!({"name":error.name(),"message":error.to_string()}),
                    case["results"][0]["error"],
                    "{}",
                    case["name"]
                ),
            }
        }
        for (index, action) in case["actions"].as_array().unwrap().iter().enumerate() {
            if action["op"] == "saveVirtual" {
                match schema::normalize_virtual(&action["input"]) {
                    Ok(value) => {
                        if let Some(expected) = case["results"][index].get("result") {
                            assert_eq!(&value, expected, "{}", case["name"]);
                        }
                    }
                    Err(error) => assert_eq!(
                        json!({"name":error.name(),"message":error.to_string()}),
                        case["results"][index]["error"],
                        "{}",
                        case["name"]
                    ),
                }
            }
        }
    }
}
