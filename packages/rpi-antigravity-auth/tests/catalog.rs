use rpi_antigravity_auth::catalog;
use serde_json::Value;

#[test]
fn catalog_and_routing_match_pinned_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/catalog.json")).unwrap();
    assert_eq!(fixture["provider"], catalog::PROVIDER);
    assert_eq!(fixture["api"], catalog::API);
    assert_eq!(fixture["baseUrl"], catalog::BASE_URL);
    assert_eq!(fixture["retiredMessage"], catalog::RETIRED_MESSAGE);
    assert_eq!(
        fixture["catalog"],
        serde_json::to_value(catalog::models()).unwrap()
    );
    for case in fixture["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        assert_eq!(case["retired"], catalog::is_retired(id), "retirement: {id}");
        let resolved = catalog::resolve_pi(
            id,
            case["reasoning"].as_bool().unwrap(),
            case["effort"].as_str(),
        );
        assert_eq!(
            case["resolved"],
            serde_json::to_value(resolved).unwrap(),
            "case: {case}"
        );
    }
}
