//! Differential test for the extension's session-pin resume layer.
//!
//! `tests/ext-diff-reference.mjs` drives the unchanged upstream extension
//! (`extensions/multiprovider.ts` + `src/session-pins.ts`, sha256-asserted)
//! through a scripted ExtensionAPI/ExtensionContext, records the genuine
//! journal it writes, and emits the complete observable effect of resuming a
//! session. This test replays the same inputs through the Rust port and
//! compares whole objects: the journal, the warnings, the follower
//! notifications and the active account per pool.
mod common;

use async_trait::async_trait;
use common::normalize;
use rpi_multiprovider::announcement::{
    AnnouncementDependencies, Listener, ServiceAnnouncement, ServiceContext,
};
use rpi_multiprovider::managed::{AccountRequest, AuthResult, ModelRef, ProviderIntegration};
use rpi_multiprovider::pins::{SESSION_PIN_ENTRY_TYPE, SESSION_PIN_ENV};
use rpi_multiprovider::resume;
use rpi_multiprovider::services::{Options, Registration, Service};
use rpi_multiprovider::types::{Account, PoolPatch, SelectionPolicy};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = include_str!("ext-diff-reference.json");

#[derive(Clone)]
struct Ctx;

impl ServiceContext for Ctx {
    fn model(&self) -> Option<ModelRef> {
        None
    }
}

/// The managed integration's account inventory: the store's pool, unchanged.
struct Integration {
    accounts: Vec<Account>,
}

#[async_trait]
impl ProviderIntegration for Integration {
    async fn accounts(&self) -> Result<Vec<Account>, String> {
        Ok(self.accounts.clone())
    }

    async fn resolve_auth(
        &self,
        _: &Account,
        _: &CancellationToken,
        _: &AccountRequest,
    ) -> Result<AuthResult, String> {
        Err("not used".into())
    }
}

fn account(value: &Value) -> Account {
    serde_json::from_value(json!({
        "id": value["id"],
        "label": value["label"],
        "authKind": value["authKind"],
        "credentialRef": value["id"],
        "enabled": value["enabled"],
        "weight": value["weight"],
        "priority": value["priority"],
    }))
    .expect("fixture account")
}

fn register(service: &Service, accounts: &[Account]) {
    let provided = accounts.to_vec();
    service
        .register(Registration {
            id: "example".into(),
            label: "Example".into(),
            accounts: Arc::new(move || {
                let provided = provided.clone();
                Box::pin(async move { Ok(provided) })
            }),
            classify_failure: None,
            management_hint: None,
            selection_bias: Default::default(),
        })
        .expect("register");
}

async fn configure(service: &Service, pool: &Value, accounts: &[Account]) {
    register(service, accounts);
    service
        .update_pool(
            pool["id"].as_str().unwrap(),
            PoolPatch {
                policy: Some(SelectionPolicy::RoundRobin),
                affinity: Some(pool["affinity"].as_bool().unwrap_or(true)),
                accounts: None,
            },
        )
        .await
        .expect("configure pool");
}

fn push_entry(entries: &mut Vec<Value>, data: &Value) {
    entries.push(json!({
        "type": "custom",
        "customType": SESSION_PIN_ENTRY_TYPE,
        "data": data,
        "id": format!("entry-{}", entries.len() + 1),
        "parentId": null,
        "timestamp": "1970-01-01T00:00:00.000Z",
    }));
}

fn journal(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|entry| {
            entry["type"] == "custom" && entry["customType"] == SESSION_PIN_ENTRY_TYPE
        })
        .map(|entry| entry["data"].clone())
        .collect()
}

/// Runs one fixture case through the port and returns the observable effect.
async fn run_case(case: &Value) -> Value {
    let input = &case["input"];
    let pools = input["pools"].as_array().unwrap();
    let pool = &pools[0];
    let accounts: Vec<Account> = pool["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(account)
        .collect();
    let session_id = input["sessionId"].as_str().unwrap().to_owned();
    let env = match input["env"].get(SESSION_PIN_ENV) {
        Some(raw) => json!({ SESSION_PIN_ENV: raw }),
        None => json!({}),
    };

    let service = Service::new(Options::default());
    let pending_first = input["pending"].as_bool().unwrap_or(false);
    if !pending_first {
        configure(&service, pool, &accounts).await;
    }

    let mut entries: Vec<Value> = input["entries"].as_array().unwrap().clone();
    let mut notifications: Vec<(String, String)> = Vec::new();
    let mut restored_pools: Vec<String> = Vec::new();

    let announce = {
        let integration: Arc<dyn ProviderIntegration> = Arc::new(Integration {
            accounts: accounts.clone(),
        });
        let session = session_id.clone();
        ServiceAnnouncement::<Ctx>::new(AnnouncementDependencies {
            scheduler: service.clone(),
            get_integration: Arc::new(move |_| Some(integration.clone())),
            get_base_provider: Arc::new(|_, _| None),
            affinity_key_for: Arc::new(move |_, _, _| Ok(session.clone())),
        })
    };
    let changes: std::sync::Arc<std::sync::Mutex<Vec<Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = changes.clone();
    let _listener: Arc<Listener<Ctx>> = Arc::new(move |event| {
        recorded.lock().unwrap().push(json!({
            "providerId": event.provider_id,
            "account": event.account.as_ref().map(|account| json!({
                "id": account.id,
                "label": account.label,
                "authKind": account.auth_kind,
            })),
        }));
        Ok(())
    });
    let _subscription = announce.on_active_account_changed("example", _listener);

    let affinity_key = {
        let session = session_id.clone();
        move |_: &str| session.clone()
    };

    let passes = if input["reload"].as_bool().unwrap_or(false) {
        2
    } else {
        1
    };
    for pass in 0..passes {
        let pending = resume::session_start_state(&entries, &env);
        let first = resume::apply_recorded_pins(&service, &entries, pending, &affinity_key).await;
        let applied = if pass == 0 && pending_first {
            // Before reconcile the base provider is absent, so the inherited pin
            // stays pending; the next reconcile registers the pool and applies it.
            assert_eq!(first.restored_pools, Vec::<String>::new());
            configure(&service, pool, &accounts).await;
            let pending = first.pending.clone();
            resume::apply_recorded_pins(&service, &entries, pending, &affinity_key).await
        } else {
            first
        };
        notifications.extend(applied.notifications.iter().cloned());
        restored_pools.extend(applied.restored_pools.iter().cloned());
        for entry in &applied.journal {
            push_entry(&mut entries, entry);
        }
    }

    for pool_id in &restored_pools {
        let account = announce.get_active_account(pool_id, &Ctx).await.unwrap();
        announce.notify_active_account_changed(pool_id, &Ctx, account);
    }

    let mut active = serde_json::Map::new();
    for pool in pools {
        let id = pool["id"].as_str().unwrap();
        let account = announce.get_active_account(id, &Ctx).await.unwrap();
        active.insert(
            id.into(),
            match account {
                Some(account) => json!({
                    "id": account.id,
                    "label": account.label,
                    "authKind": account.auth_kind,
                }),
                None => Value::Null,
            },
        );
    }

    json!({
        "journal": journal(&entries),
        "notifications": notifications
            .into_iter()
            .map(|(message, level)| json!([message, level]))
            .collect::<Vec<_>>(),
        "accountChanges": changes.lock().unwrap().clone(),
        "active": active,
    })
}

#[tokio::test]
async fn resume_cases_match_the_upstream_reference() {
    let reference: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = reference["resume"].as_array().unwrap();
    assert!(cases.len() >= 8, "the reference records the resume scenarios");
    for case in cases {
        let actual = run_case(case).await;
        assert_eq!(
            normalize(&actual),
            normalize(&case["expected"]),
            "{}",
            case["name"]
        );
    }
}
