use rpi_antigravity_auth::vault::{
    policy::ConsumerError,
    roster::{self, Projection},
};
use serde_json::{json, Value};
fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/vault-roster.json")).unwrap()
}
fn outcome(result: Result<Value, ConsumerError>) -> Value {
    match result {
        Ok(value) => json!({"result":value}),
        Err(e) => {
            json!({"error":{"name":"ClaustrumConsumerError","kind":e.kind,"message":e.message}})
        }
    }
}
#[test]
fn roster_projection_matches_original_identity_and_incomplete_inventory_rules() {
    let fixtures = fixtures();
    for case in fixtures["cases"].as_array().unwrap() {
        let options = &case["options"];
        let mut projection = Projection::new(
            fixtures["locale"].as_str().unwrap(),
            options["now"].as_f64().unwrap(),
        )
        .unwrap();
        if let Some(prefix) = options["routePrefix"].as_str() {
            projection.prefix = prefix.into();
        }
        if let Some(reserved) = options["reservedRouteIds"].as_array() {
            projection.reserved = reserved
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
        }
        assert_eq!(
            roster::project(case.get("previous"), &case["inventory"], &projection),
            case["result"],
            "{}",
            case["name"]
        );
    }
}
#[test]
fn stored_roster_and_quota_validation_matches_original() {
    for case in fixtures()["decodes"].as_array().unwrap() {
        let result = if let Some(raw) = case["raw"].as_str() {
            serde_json::from_str::<Value>(raw)
                .map_err(|_| {
                    ConsumerError::new("invalid-state", "Claustrum roster file is not valid")
                })
                .and_then(|value| roster::decode(&value))
        } else {
            roster::decode(&case["input"])
        };
        let actual = outcome(result);
        assert_eq!(
            actual,
            if case.get("error").is_some() {
                json!({"error":case["error"]})
            } else {
                json!({"result":case["result"]})
            },
            "{}",
            case["name"]
        );
    }
}
#[test]
fn route_acceptance_and_decline_preserve_other_account_bindings() {
    for case in fixtures()["changes"].as_array().unwrap() {
        let input = roster::decode(&case["input"]).unwrap();
        assert_eq!(
            outcome(roster::change_route(
                &input,
                case["route"].as_str().unwrap(),
                case["accept"].as_bool().unwrap()
            )),
            if case.get("error").is_some() {
                json!({"error":case["error"]})
            } else {
                json!({"result":case["result"]})
            }
        );
    }
    for case in fixtures()["interlocks"].as_array().unwrap() {
        let entries = case["entries"].as_array().unwrap();
        let id = case["id"].as_str().unwrap();
        let identity = case.get("identity");
        assert_eq!(
            roster::is_declined(entries, id, identity),
            case["isDeclined"].as_bool().unwrap()
        );
        assert_eq!(
            json!(roster::decline_account(entries, id, identity)),
            case["decline"]
        );
        assert_eq!(
            json!(roster::accept_account(entries, &[id, "alias"], identity)),
            case["accept"]
        );
    }
}
#[test]
fn roster_locale_order_matches_node_reference() {
    for case in fixtures()["sorts"].as_array().unwrap() {
        let projection = Projection::new(case["locale"].as_str().unwrap(), 0.0).unwrap();
        let mut values: Vec<_> = case["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        values.sort_by(|a, b| projection.collator.compare(a, b));
        let expected: Vec<_> = case["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if values != expected {
            let differences: Vec<_> = values
                .iter()
                .zip(expected.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(10)
                .collect();
            panic!(
                "{} collation differences: {:?}",
                case["locale"], differences
            );
        }
    }
}

#[test]
fn collation_decomposition_matches_node_for_every_unicode_scalar_and_reordering() {
    use icu_normalizer::DecomposingNormalizer;
    use icu_provider_adapters::fallback::LocaleFallbackProvider;
    use icu_provider_blob::BlobDataProvider;
    use sha2::{Digest, Sha256};
    let blob = BlobDataProvider::try_new_from_static_blob(include_bytes!(
        "../data/icu78-collation.postcard"
    ))
    .unwrap();
    let provider = LocaleFallbackProvider::try_new_with_buffer_provider(blob).unwrap();
    let normalizer = DecomposingNormalizer::try_new_nfd_with_buffer_provider(&provider).unwrap();
    let mut digest = Sha256::new();
    let mut count = 0;
    for cp in 0..0x110000 {
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        count += 1;
        for value in [ch.to_string(), format!("a\u{315}{ch}\u{300}\u{327}")] {
            digest.update(format!("{cp:x}:{}\n", normalizer.normalize(&value)).as_bytes());
        }
    }
    let reference = &fixtures()["normalization"];
    assert_eq!(json!(count), reference["scalars"]);
    assert_eq!(
        format!("{:x}", digest.finalize()),
        reference["sha256"].as_str().unwrap()
    );
}

#[test]
fn roster_collation_matches_node_order_for_every_unicode_scalar() {
    use sha2::{Digest, Sha256};
    let projection = Projection::new("en-US", 0.0).unwrap();
    let mut values: Vec<_> = (0..0x110000).filter_map(char::from_u32).collect();
    values.sort_by(|a, b| {
        let mut left = [0; 4];
        let mut right = [0; 4];
        projection
            .collator
            .compare(a.encode_utf8(&mut left), b.encode_utf8(&mut right))
    });
    let reference = &fixtures()["collation"];
    assert_eq!(json!(values.len()), reference["scalars"]);
    let mut digest = Sha256::new();
    for value in values {
        digest.update(format!("{:x}\n", value as u32).as_bytes());
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        reference["sha256"].as_str().unwrap()
    );
}
