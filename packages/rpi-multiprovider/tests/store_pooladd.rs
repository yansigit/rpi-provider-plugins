//! Add-by-login through the private credential-store port: the record a
//! completed `/multilogin` produces, including the pool the pool manager had
//! only buffered ("pending pool"). Every step is compared to the upstream
//! `MultiAuthStore` reference fixture, and the persisted file must be
//! byte-identical to Pi's.
use rpi_multiprovider::store::port::{Port, STORE_ID};
use rpi_multiprovider::store::{Failure, Store};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn failure(error: Failure) -> Value {
    match error {
        Failure::Public(error) => json!({"error":{"name":error.name(),"message":error.to_string()}}),
        Failure::Syntax { line, column, .. } => panic!("unexpected syntax error at {line}:{column}"),
        Failure::Io {
            operation,
            path,
            error,
        } => panic!("{operation} {}: {error}", path.display()),
    }
}
async fn checked_port(port: &Port, request: Value) -> Value {
    match port.request(request, CancellationToken::new()).await {
        Ok(value) => value,
        Err(error) => panic!("unexpected add-by-login failure: {}", failure(error)),
    }
}
async fn checked_store(store: &Store, request: Value) -> Value {
    match store.request(request, CancellationToken::new()).await {
        Ok(value) => value,
        Err(error) => panic!("unexpected store failure: {}", failure(error)),
    }
}
fn scratch() -> Scratch {
    let root = Scratch(
        std::env::temp_dir().join(format!("rpi-multiprovider-pooladd-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&root.0).unwrap();
    root
}
fn sources(now: Arc<AtomicU64>) -> (Arc<dyn Fn() -> f64 + Send + Sync>, Arc<dyn Fn() -> String + Send + Sync>)
{
    let clock = now;
    let next = AtomicU64::new(0);
    (
        Arc::new(move || clock.load(Ordering::SeqCst) as f64),
        Arc::new(move || format!("fixture-{}", next.fetch_add(1, Ordering::SeqCst) + 1)),
    )
}

#[tokio::test]
async fn add_by_login_records_and_files_match_the_reference() {
    std::env::set_var("LC_ALL", "en_US.UTF-8");
    let reference: Value = serde_json::from_str(include_str!("store-reference.json")).unwrap();
    let cases: Vec<&Value> = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["name"].as_str().unwrap().starts_with("add by login"))
        .collect();
    assert_eq!(cases.len(), 6, "add-by-login scenarios");
    for case in cases {
        let root = scratch();
        let path = root.0.join("multiprovider-auth.json");
        let now = Arc::new(AtomicU64::new(1_700_000_000_000));
        let (clock, ids) = sources(now.clone());
        let store = Store::with_sources(path.clone(), clock, ids);
        let port = Port::new(store.clone());
        let mut last = "missing".to_string();
        for (index, action) in case["actions"].as_array().unwrap().iter().enumerate() {
            let provider = action["provider"].as_str().unwrap_or("example");
            let actual = if action["op"] == "add" {
                let request = json!({"version":1,"storeId":STORE_ID,"providerId":provider,
                    "action":"add","input":action["input"]});
                match port.request(request, CancellationToken::new()).await {
                    Ok(value) => {
                        assert!(
                            !value.to_string().contains("fixture-secret"),
                            "case {} operation {index}: credential leaked",
                            case["name"]
                        );
                        assert!(value["payload"].get("credential").is_none());
                        last = value["payload"]["id"].as_str().unwrap().to_string();
                        json!({"result":value["payload"]})
                    }
                    Err(error) => failure(error),
                }
            } else {
                let mut request = action.clone();
                if request.get("provider").is_none() {
                    request["provider"] = json!("example");
                }
                if request.get("account").is_none() {
                    request["account"] = json!(last);
                }
                match store.request(request, CancellationToken::new()).await {
                    Ok(value) => json!({"result":value}),
                    Err(error) => failure(error),
                }
            };
            let expected = case["results"][index].clone();
            let mut actual = actual;
            actual["persisted"] = json!(std::fs::read_to_string(&path).ok());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                actual["mode"] = json!(std::fs::metadata(&path)
                    .ok()
                    .map(|metadata| metadata.permissions().mode() & 0o777));
            }
            #[cfg(not(unix))]
            let expected = {
                let mut expected = expected;
                expected.as_object_mut().unwrap().remove("mode");
                expected
            };
            assert_eq!(
                actual, expected,
                "case {} operation {index}: {action}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn add_by_login_is_private_and_commits_before_release() {
    let root = scratch();
    let path = root.0.join("multiprovider-auth.json");
    let now = Arc::new(AtomicU64::new(1_700_000_000_000));
    let (clock, ids) = sources(now);
    let store = Store::with_sources(path.clone(), clock, ids);
    let port = Port::new(store.clone());
    let request = json!({"version":1,"storeId":STORE_ID,"providerId":"example","action":"add",
        "input":{"label":" Work ","credential":{"type":"api_key","key":"fixture-secret"},
        "pool":{"policy":"priority","affinity":false,"includeUpstream":false,"upstream":{"label":" Home ","weight":3}}}});
    let value = checked_port(&port, request).await;
    assert_eq!(value["kind"], "account");
    assert_eq!(value["payload"]["label"], "Work");
    assert_eq!(value["payload"]["id"], "fixture-1");
    assert_eq!(value["payload"]["authKind"], "api-key");
    assert_eq!(value["payload"]["enabled"], true);
    assert_eq!(value["payload"]["weight"], 1);
    assert_eq!(value["payload"]["priority"], 1);
    assert!(value["payload"].get("credential").is_none());
    assert!(!root.0.join("multiprovider-auth.json.lock").exists());
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["providers"]["example"]["policy"], "priority");
    assert_eq!(stored["providers"]["example"]["affinity"], false);
    assert_eq!(stored["providers"]["example"]["includeUpstream"], false);
    assert_eq!(
        stored["providers"]["example"]["upstream"],
        json!({"label":"Home","weight":3})
    );
    assert_eq!(
        stored["providers"]["example"]["accounts"][0]["credential"]["key"],
        "fixture-secret"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    // A login-created record is immediately addressable by the other actions.
    let read = checked_port(
        &port,
        json!({"version":1,"storeId":STORE_ID,"providerId":"example","accountId":"fixture-1","action":"read","input":null}),
    )
    .await;
    assert_eq!(read["kind"], "credential");
    assert_eq!(read["payload"]["key"], "fixture-secret");
}

#[tokio::test]
async fn add_by_login_rejects_invalid_input_without_writing() {
    let root = scratch();
    let path = root.0.join("multiprovider-auth.json");
    let port = Port::new(Store::new(path.clone()));
    let request = |input: Value| {
        json!({"version":1,"storeId":STORE_ID,"providerId":"example","action":"add","input":input})
    };
    for (input, message) in [
        (
            json!({"label":" ","credential":{"type":"api_key","key":"fixture-secret"}}),
            "multiprovider: account label is required",
        ),
        (
            json!({"label":"Work","credential":null}),
            "multiprovider: malformed stored credential",
        ),
        (
            json!({"label":"Work","credential":{"type":"custom"}}),
            "multiprovider: unsupported stored credential type",
        ),
    ] {
        let error = port
            .request(request(input), CancellationToken::new())
            .await
            .err()
            .expect("invalid add");
        assert_eq!(failure(error)["error"]["message"], message);
        assert!(!path.exists());
    }
    // Unsafe provider identity is refused before any record is created.
    let error = port
        .request(
            json!({"version":1,"storeId":STORE_ID,"providerId":"__proto__","action":"add",
                "input":{"label":"Work","credential":{"type":"api_key","key":"fixture-secret"}}}),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("unsafe provider");
    assert_eq!(
        failure(error)["error"]["message"],
        "multiprovider: invalid provider id"
    );
    assert!(!path.exists());
}

#[test]
#[ignore = "subprocess entry invoked by concurrent_logins_create_one_pool"]
fn pooladd_process_writer() {
    let path = PathBuf::from(std::env::var_os("RPI_MULTIPROVIDER_TEST_STORE").unwrap());
    let label = std::env::var("RPI_MULTIPROVIDER_TEST_LABEL").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let port = Port::new(Store::new(path));
        let request = json!({"version":1,"storeId":STORE_ID,"providerId":"example","action":"add",
            "input":{"label":label,"credential":{"type":"api_key","key":"fixture-secret"},
            "pool":{"policy":"priority","affinity":false,"includeUpstream":false,"upstream":{"label":"Home","weight":3}}}});
        let value = checked_port(&port, request).await;
        assert_eq!(value["kind"], "account");
    });
}

#[tokio::test]
async fn concurrent_logins_create_one_pool() {
    let root = scratch();
    let path = root.0.join("multiprovider-auth.json");
    let mut children = Vec::new();
    for index in 0..8 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "pooladd_process_writer", "--ignored", "--nocapture"])
                .env("RPI_MULTIPROVIDER_TEST_STORE", &path)
                .env("RPI_MULTIPROVIDER_TEST_LABEL", format!("Account {index}"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let store = Store::new(path.clone());
    let pool = checked_store(&store, json!({"op":"pool","provider":"example"})).await;
    assert_eq!(pool["policy"], "priority");
    assert_eq!(pool["affinity"], false);
    assert_eq!(pool["includeUpstream"], false);
    assert_eq!(pool["upstream"], json!({"label":"Home","weight":3}));
    let accounts = pool["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 8);
    for index in 0..8 {
        assert_eq!(
            accounts
                .iter()
                .filter(|account| account["label"] == format!("Account {index}"))
                .count(),
            1
        );
    }
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
