use rpi_multiprovider::pins;
use serde_json::{json, Value};

#[test]
fn pinned_journal_and_inheritance_parsing() {
    let reference: Value = serde_json::from_str(include_str!("scheduler-reference.json")).unwrap();
    for case in reference["pinCases"].as_array().unwrap() {
        assert_eq!(
            json!(pins::from_entries(case["entries"].as_array().unwrap())),
            case["journal"]
        );
        assert_eq!(
            json!(pins::inherited_from_value(&case["input"])),
            case["inherited"]
        );
        assert_eq!(
            pins::serialize_inherited(&case["input"]),
            case["serialized"]
        );
    }
    for case in reference["envCases"].as_array().unwrap() {
        assert_eq!(
            json!(pins::inherited_from_env(&case["env"])),
            case["result"]
        );
    }
}
