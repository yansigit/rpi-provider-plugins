//! The Antigravity OAuth client comes only from the build environment or the runtime
//! environment; the crate ships no identity of its own. Every source is injected here,
//! so these results are identical whether or not the variables were set at build time.
use rpi_antigravity_auth::auth::{
    self, Admission, BuildClient, ClientSource, CLIENT_ID_ENV, CLIENT_SECRET_ENV,
};
use serde_json::json;
use url::{form_urlencoded, Url};
mod common;
use common::script;

const UNCONFIGURED: &str = "Antigravity OAuth is not configured: set RPI_ANTIGRAVITY_OAUTH_CLIENT_ID and RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET when building rpi-antigravity-auth, or export both in the runtime environment.";
// Characters that form encoding must round-trip, so "verbatim" is checked exactly.
const ID: &str = "verbatim-id+1.invalid";
const SECRET: &str = "verbatim secret/2";

fn no_env(_name: &str) -> Option<String> {
    None
}
fn runtime_env(name: &str) -> Option<String> {
    match name {
        CLIENT_ID_ENV => Some("runtime-client.invalid".into()),
        CLIENT_SECRET_ENV => Some("runtime-secret".into()),
        _ => None,
    }
}
fn blank_env(_name: &str) -> Option<String> {
    Some(" \t".into())
}

fn unset(runtime: fn(&str) -> Option<String>) -> ClientSource {
    ClientSource {
        build: BuildClient::NONE,
        runtime,
    }
}

/// The `client_id` the login URL carries for `source`, or the configuration error.
fn login_client_id(source: ClientSource) -> Result<String, String> {
    let authorization = auth::authorize_with_entropy("", &[9; 32], source)?;
    let url = Url::parse(&authorization.url).unwrap();
    Ok(url
        .query_pairs()
        .find(|(key, _)| key == "client_id")
        .unwrap()
        .1
        .into_owned())
}

/// A real login URL for `project`, whose `state` the exchange tests feed back in.
fn state_for(project: &str) -> String {
    let authorization =
        auth::authorize_with_entropy(project, &[3; 32], ClientSource::configured(ID, SECRET))
            .unwrap();
    Url::parse(&authorization.url)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned()
}

/// Decodes a form body into its pairs, so each value is compared exactly.
fn form_pairs(body: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

struct Open;
impl Admission for Open {
    async fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn cancelled(&self) -> bool {
        false
    }
}

#[test]
fn configured_client_id_is_used_verbatim_in_the_login_url() {
    assert_eq!(
        login_client_id(ClientSource::configured(ID, SECRET)).unwrap(),
        ID
    );
}

#[tokio::test]
async fn configured_client_is_sent_verbatim_in_the_refresh_request() {
    let port = script(
        1_000.0,
        &json!({
            "trace": [],
            "responses": [{"status": 200, "statusText": "OK", "json": {
                "access_token": "access", "refresh_token": "rotated", "expires_in": 3600
            }}]
        }),
    );
    auth::refresh(&port, ClientSource::configured(ID, SECRET), "old-refresh")
        .await
        .unwrap();
    let trace = port.trace.lock().unwrap();
    let pairs = form_pairs(trace[0]["request"]["body"].as_str().unwrap());
    assert_eq!(
        pairs.len(),
        4,
        "grant_type, refresh_token, client_id, client_secret"
    );
    assert!(pairs.contains(&("client_id".into(), ID.into())));
    assert!(pairs.contains(&("client_secret".into(), SECRET.into())));
}

#[tokio::test]
async fn configured_client_is_sent_verbatim_in_the_exchange_request() {
    let port = script(
        1_000.0,
        &json!({
            "trace": [],
            "responses": [
                {"status": 200, "statusText": "OK", "json": {
                    "access_token": "access", "refresh_token": "refresh", "expires_in": 3600
                }},
                {"status": 200, "statusText": "OK", "json": {}}
            ]
        }),
    );
    let result = auth::exchange(
        &port,
        &Open,
        ClientSource::configured(ID, SECRET),
        "code",
        &state_for("project-one"),
    )
    .await;
    assert_eq!(result["type"], "success", "{result}");
    let trace = port.trace.lock().unwrap();
    let pairs = form_pairs(trace[0]["request"]["body"].as_str().unwrap());
    assert!(pairs.contains(&("client_id".into(), ID.into())));
    assert!(pairs.contains(&("client_secret".into(), SECRET.into())));
}

#[test]
fn unconfigured_login_fails_before_any_url_is_built() {
    assert_eq!(
        auth::authorize_with_entropy("", &[3; 32], unset(no_env))
            .err()
            .as_deref(),
        Some(UNCONFIGURED)
    );
    assert_eq!(
        auth::authorize("", unset(no_env)).err().as_deref(),
        Some(UNCONFIGURED)
    );
}

#[tokio::test]
async fn unconfigured_refresh_fails_without_sending_a_request() {
    let port = script(1_000.0, &json!({"trace": [], "responses": []}));
    let error = auth::refresh(&port, unset(no_env), "old-refresh")
        .await
        .unwrap_err();
    assert_eq!(error, UNCONFIGURED);
    assert!(port.trace.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unconfigured_exchange_reports_the_error_without_sending_a_request() {
    let port = script(1_000.0, &json!({"trace": [], "responses": []}));
    let result = auth::exchange(
        &port,
        &Open,
        unset(no_env),
        "code",
        &state_for("project-one"),
    )
    .await;
    assert_eq!(result, json!({"type": "failed", "error": UNCONFIGURED}));
    assert!(port.trace.lock().unwrap().is_empty());
}

#[test]
fn a_single_variable_is_not_a_configuration() {
    let only_id = ClientSource {
        build: BuildClient {
            id: Some(ID),
            secret: None,
        },
        runtime: no_env,
    };
    let only_secret = ClientSource {
        build: BuildClient {
            id: None,
            secret: Some(SECRET),
        },
        runtime: no_env,
    };
    assert_eq!(login_client_id(only_id), Err(UNCONFIGURED.into()));
    assert_eq!(login_client_id(only_secret), Err(UNCONFIGURED.into()));
}

#[test]
fn runtime_variables_apply_when_build_values_are_absent() {
    assert_eq!(
        login_client_id(unset(runtime_env)).unwrap(),
        "runtime-client.invalid"
    );
}

#[tokio::test]
async fn runtime_client_is_sent_in_the_refresh_request() {
    let port = script(
        1_000.0,
        &json!({
            "trace": [],
            "responses": [{"status": 200, "statusText": "OK", "json": {
                "access_token": "access", "refresh_token": "rotated", "expires_in": 3600
            }}]
        }),
    );
    auth::refresh(&port, unset(runtime_env), "old-refresh")
        .await
        .unwrap();
    let trace = port.trace.lock().unwrap();
    let pairs = form_pairs(trace[0]["request"]["body"].as_str().unwrap());
    assert!(pairs.contains(&("client_id".into(), "runtime-client.invalid".into())));
    assert!(pairs.contains(&("client_secret".into(), "runtime-secret".into())));
}

#[test]
fn build_values_take_precedence_over_runtime_variables() {
    let source = ClientSource {
        build: BuildClient {
            id: Some(ID),
            secret: Some(SECRET),
        },
        runtime: runtime_env,
    };
    assert_eq!(login_client_id(source).unwrap(), ID);
}

#[test]
fn empty_and_blank_values_count_as_unset() {
    let fallback = ClientSource {
        build: BuildClient {
            id: Some(""),
            secret: Some("   "),
        },
        runtime: runtime_env,
    };
    assert_eq!(login_client_id(fallback).unwrap(), "runtime-client.invalid");
    let blank = ClientSource {
        build: BuildClient {
            id: Some(""),
            secret: Some(SECRET),
        },
        runtime: blank_env,
    };
    assert_eq!(login_client_id(blank), Err(UNCONFIGURED.into()));
}
