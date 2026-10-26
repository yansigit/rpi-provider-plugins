//! Loaded-library OAuth/project diagnostics: malformed-JSON messages, expiry
//! units, cache eviction and error redaction compared to the unchanged upstream
//! Pi 1.3.0 reference (tests/fixtures/ag1-oauth.json).
use rpi_antigravity_auth::{
    auth::{self, Admission, ClientSource},
    project::{self, ProjectStore},
    redaction,
};
use serde_json::{json, Value};
use std::sync::Arc;
mod common;

/// The placeholder pair the fixture was scrubbed to; injected so the result does not
/// depend on the build or runtime OAuth environment.
const FIXTURE_CLIENT: ClientSource = ClientSource::configured(
    "antigravity-oauth-client-id.invalid",
    "antigravity-oauth-client-secret-placeholder",
);

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/ag1-oauth.json")).unwrap()
}

/// A gate that never refuses and never cancels, matching the reference's
/// no-`admit` invocation.
struct Open;
impl Admission for Open {
    async fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn cancelled(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn malformed_oauth_bodies_and_expiry_units_match_original() {
    let f = fixture();
    for case in f["exchange"].as_array().unwrap() {
        let port = common::script(f["clock"].as_f64().unwrap(), case);
        let result = auth::exchange(
            &port,
            &Open,
            FIXTURE_CLIENT,
            case["code"].as_str().unwrap(),
            case["state"].as_str().unwrap(),
        )
        .await;
        assert_eq!(result, case["result"], "exchange result: {case}");
        assert_eq!(
            json!(*port.trace.lock().unwrap()),
            case["trace"],
            "exchange trace: {case}"
        );
        assert_eq!(
            port.queue.lock().unwrap().len() as u64,
            case["remaining"].as_u64().unwrap(),
            "exchange remaining: {case}"
        );
    }
    for case in f["refresh"].as_array().unwrap() {
        let port = common::script(f["clock"].as_f64().unwrap(), case);
        let packed = case["packed"].as_str().unwrap();
        let result = match auth::refresh(&port, FIXTURE_CLIENT, packed).await {
            Ok(value) => json!({"value": value}),
            Err(error) => json!({"error": error}),
        };
        assert_eq!(result, case["result"], "refresh result: {case}");
        assert_eq!(
            json!(*port.trace.lock().unwrap()),
            case["trace"],
            "refresh trace: {case}"
        );
        assert_eq!(
            port.queue.lock().unwrap().len() as u64,
            case["remaining"].as_u64().unwrap(),
            "refresh remaining: {case}"
        );
    }
    for case in f["expiries"].as_array().unwrap() {
        assert_eq!(
            auth::expires_at(f["clock"].as_f64().unwrap(), &case["seconds"]),
            case["result"].as_f64().unwrap(),
            "expiry: {case}"
        );
    }
}

#[tokio::test]
async fn malformed_project_bodies_cache_eviction_and_redaction_match_original() {
    let f = fixture();
    for case in f["project"].as_array().unwrap() {
        let port = Arc::new(common::script(f["clock"].as_f64().unwrap(), case));
        let store = Arc::new(ProjectStore::default());
        let mut results = Vec::new();
        for op in case["operations"].as_array().unwrap() {
            let result = match op["op"].as_str().unwrap() {
                "ensure" => store.ensure(port.clone(), op["auth"].clone()).await,
                "onboard" => Ok(project::onboard(
                    port.as_ref(),
                    op["access"].as_str().unwrap(),
                    &op["tier"],
                    op["project"].as_str(),
                    op["attempts"].as_u64().unwrap() as u32,
                    op["delay"].as_u64().unwrap(),
                )
                .await
                .unwrap_or(Value::Null)),
                "advance" => {
                    *port.clock.lock().unwrap() += op["milliseconds"].as_f64().unwrap();
                    Ok(Value::Null)
                }
                "clearFailures" => {
                    store.clear_provision_failures();
                    Ok(Value::Null)
                }
                other => panic!("unknown operation: {other}"),
            };
            results.push(match result {
                Ok(value) => json!({"value": value}),
                Err(error) => json!({"error": error}),
            });
        }
        assert_eq!(
            json!(results),
            case["results"],
            "project result: {}",
            case["name"]
        );
        assert_eq!(
            json!(*port.trace.lock().unwrap()),
            case["trace"],
            "project trace: {}",
            case["name"]
        );
        assert_eq!(
            *port.clock.lock().unwrap(),
            case["clock"].as_f64().unwrap(),
            "project clock: {}",
            case["name"]
        );
        assert!(
            port.queue.lock().unwrap().is_empty(),
            "unused responses: {}",
            case["name"]
        );
    }
    let mut cache = auth::CredentialCache::default();
    for case in f["cache"]["trace"].as_array().unwrap() {
        cache.remember(
            case["access"].as_str().unwrap(),
            case["refresh"].as_str().unwrap(),
        );
        for (key, value) in case["values"].as_object().unwrap() {
            assert_eq!(json!(cache.get(key)), *value, "cache: {case}");
        }
    }
    for case in f["redaction"].as_array().unwrap() {
        let credentials: Vec<String> = serde_json::from_value(case["credentials"].clone()).unwrap();
        let projects: Vec<String> = serde_json::from_value(case["projects"].clone()).unwrap();
        assert_eq!(
            redaction::text(case["input"].as_str().unwrap(), &credentials, &projects),
            case["result"].as_str().unwrap(),
            "redaction: {case}"
        );
    }
}
