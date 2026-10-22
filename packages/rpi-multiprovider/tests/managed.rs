mod common;

use common::*;
use rpi_multiprovider::managed::{
    get_pool, has_accounts, resolve_account, AccountRequest, AuthResult, ManagedIntegration,
    ModelRef, NativeInput, NativeProvider, ProviderIntegration,
};
use rpi_multiprovider::types::Account;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const FIXTURE: &str = include_str!("managed-reference.json");

fn request_options(input: &Value) -> Map<String, Value> {
    input
        .get("requestOptions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

async fn account_for(input: &Value, integration: &ManagedIntegration) -> Result<Account, String> {
    if let Some(account) = input.get("account") {
        return Ok(serde_json::from_value(account.clone()).unwrap());
    }
    let id = input["accountId"].as_str().unwrap_or_default();
    integration
        .accounts()
        .await?
        .into_iter()
        .find(|candidate| candidate.id == id)
        .ok_or_else(|| format!("unknown account {id}"))
}

async fn run_action(
    input: &Value,
    provider: &Arc<FixtureProvider>,
    store: &Arc<FixtureStore>,
    integration: &ManagedIntegration,
) -> Result<Value, String> {
    match input["op"].as_str().unwrap() {
        "accounts" => Ok(Value::Array(
            integration
                .accounts()
                .await?
                .iter()
                .map(account_json)
                .collect(),
        )),
        "pool" => Ok(get_pool(&**store, &provider.id())
            .await?
            .unwrap_or(Value::Null)),
        "hasAccounts" => Ok(json!(has_accounts(&**store, &provider.id()).await?)),
        "shape" => {
            let auth = integration.auth();
            Ok(json!({
                "topLevel": auth.surface_keys(),
                "apiKey": auth.api_key.surface_keys(),
                "oauth": auth.oauth.as_ref().map(|oauth| oauth.surface_keys()),
                "integration": integration.surface_keys(),
                "hint": integration.management_hint(),
            }))
        }
        "merged" => {
            let auth = integration.auth();
            let credential = input.get("credential").cloned();
            let signal = signal_for(input);
            let native = NativeInput {
                credential: credential.clone(),
                signal: signal.clone(),
            };
            match input["name"].as_str().unwrap() {
                "apiKey.check" => {
                    Ok(serde_json::to_value(auth.api_key.check(native).await?).unwrap())
                }
                "apiKey.resolve" => {
                    Ok(serde_json::to_value(auth.api_key.resolve(native).await?).unwrap())
                }
                "oauth.toAuth" => {
                    let oauth = auth
                        .oauth
                        .as_ref()
                        .ok_or("Cannot read properties of undefined (reading 'toAuth')")?;
                    let credential = credential.unwrap_or(Value::Null);
                    Ok(serde_json::to_value(oauth.to_auth(&credential).await?).unwrap())
                }
                "oauth.refresh" => {
                    let oauth = auth
                        .oauth
                        .as_ref()
                        .ok_or("Cannot read properties of undefined (reading 'refresh')")?;
                    oauth
                        .refresh(credential.unwrap_or(Value::Null), signal)
                        .await
                }
                other => Err(format!("unknown merged {other}")),
            }
        }
        "excludeAccountIds" => Ok(json!(
            integration.exclude_account_ids(&request_options(input))
        )),
        "resolveAuth" => {
            let signal = signal_for(input);
            let account = account_for(input, integration).await?;
            let request = AccountRequest {
                provider: provider.clone(),
                model: ModelRef {
                    id: input["model"].as_str().unwrap_or("same-model").to_owned(),
                    base_url: None,
                },
                context: json!({ "messages": [] }),
                request_options: request_options(input),
                signal: signal.clone(),
            };
            let resolution = integration
                .resolve_auth(&account, &signal, &request)
                .await?;
            Ok(serde_json::to_value(resolution).unwrap())
        }
        "sanitize" => {
            let account = account_for(input, integration).await?;
            let resolution: AuthResult =
                serde_json::from_value(input["resolution"].clone()).unwrap();
            let options = integration.sanitize_request_options(
                &account,
                &resolution,
                &request_options(input),
            );
            Ok(Value::Object(options))
        }
        "resolveAccount" => {
            let signal = signal_for(input);
            let id = input["accountId"].as_str().unwrap_or_default();
            let resolution = resolve_account(&**store, &**provider, id, &signal).await?;
            Ok(serde_json::to_value(resolution).unwrap())
        }
        other => Err(format!("unknown action {other}")),
    }
}

async fn run_case(case: &Value) -> Value {
    let trace: Trace = Arc::default();
    let now = case["now"].as_f64().unwrap_or(DEFAULT_NOW);
    let state = (!case["state"].is_null()).then(|| case["state"].clone());
    let store = Arc::new(FixtureStore::new(trace.clone(), state, now));
    let provider = fixture_provider(&case["provider"], &trace);
    let integration = ManagedIntegration::new(provider.clone(), store.clone());
    let mut results: Vec<Value> = Vec::new();
    for action in case["actions"].as_array().unwrap() {
        let input = resolve_refs(action, &results);
        let before = trace.lock().unwrap().len();
        let mut outcome = match run_action(&input, &provider, &store, &integration).await {
            Ok(result) => json!({ "op": action["op"], "result": result }),
            Err(error) => json!({ "op": action["op"], "error": error }),
        };
        outcome["trace"] = Value::Array(trace.lock().unwrap()[before..].to_vec());
        results.push(outcome);
    }
    assert!(!store.is_locked(), "{} left the lock held", case["name"]);
    if let Some(secrets) = case["secrets"].as_array() {
        for result in &results {
            if result["op"] == "accounts" || result["op"] == "pool" {
                for secret in secrets {
                    let secret = secret.as_str().unwrap();
                    assert!(!result.to_string().contains(secret), "exposed {secret}");
                }
            }
        }
    }
    json!({
        "name": case["name"],
        "results": results,
        "finalState": store.final_state(),
    })
}

#[tokio::test]
async fn managed_cases_match_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"]["managed"].as_array().unwrap();
    assert_eq!(cases.len(), 58);
    for case in cases {
        let actual = run_case(case).await;
        let expected = json!({
            "name": case["name"],
            "results": case["results"],
            "finalState": case["finalState"],
        });
        assert_eq!(
            normalize(&actual),
            normalize(&expected),
            "case {}",
            case["name"]
        );
    }
}

#[test]
fn reference_fixture_pins_the_upstream_commit_and_sources() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(
        fixture["reference"],
        "bd97d65a36f541935dc91e33ba15d7910a30f9d7"
    );
    assert_eq!(
        fixture["hashes"]["src/managed.ts"],
        "5a139118f805ed4825259df761dd5e2fbec515da19d3bc10b9719c9bf30bdeef"
    );
}
