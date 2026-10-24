//! Optional native Fabric contract. A tool name is not authority: discovery
//! requires pi-fabric v1 and an explicit failover-compaction capability receipt.
//! Acquire narrow selected-scope authority before callback return. Workers retain
//! only that lease, never an invocation or registration client.
use crate::{fabric::*, lift::Failover, services::Service};
use async_trait::async_trait;
use rpi_plugin_sdk::provider::native::{Client, ScopeServiceLease};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub const CAPABILITY: &str = "multiprovider.failoverCompaction.v1";
#[derive(Default)]
pub(super) struct Tasks(Mutex<HashMap<Option<String>, Scope>>);
struct Scope {
    signal: CancellationToken,
    state: Arc<Mutex<CompactionState>>,
    tasks: Vec<JoinHandle<()>>,
}
impl Tasks {
    pub fn drain(&self, scope: Option<&Option<String>>) {
        let removed: Vec<_> = {
            let mut scopes = self.0.lock().unwrap();
            if let Some(id) = scope {
                scopes.remove(id).into_iter().collect()
            } else {
                scopes.drain().map(|(_, scope)| scope).collect()
            }
        };
        for scope in &removed {
            scope.signal.cancel();
        }
        for scope in removed {
            for task in scope.tasks {
                let _ = task.join();
            }
        }
    }
    pub async fn failover(
        &self,
        scope: Option<String>,
        client: &Client,
        service: &Service,
        stopping: &CancellationToken,
        signal: &CancellationToken,
        info: Failover<'_>,
    ) -> Result<bool, String> {
        let message = assistant_message(info.failure.assistant_message.as_ref());
        if !message
            .as_ref()
            .is_some_and(crate::retry::is_retryable_assistant_error)
        {
            return Ok(false);
        }
        let Ok(manifests) = client
            .request(json!({"port":"services","action":"list"}), signal.clone())
            .await
        else {
            return Ok(false);
        };
        if !manifests.as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["id"] == "pi-fabric"
                    && item["serviceVersion"] == 1
                    && ["capabilities", "isIdle", "compact"].iter().all(|method| {
                        item["methods"]
                            .as_array()
                            .is_some_and(|methods| methods.contains(&json!(method)))
                    })
            })
        }) {
            return Ok(false);
        }
        let Ok(capabilities) = call(client, "capabilities", json!({}), signal.clone()).await else {
            return Ok(false);
        };
        if !capabilities
            .as_array()
            .is_some_and(|items| items.contains(&json!(CAPABILITY)))
        {
            return Ok(false);
        }
        let Ok(lease) = client
            .scope_service_lease("pi-fabric", 1, &["isIdle", "compact"], signal.clone())
            .await
        else {
            // Old hosts must keep inline failover; ordinary service calls are
            // not safe after the provider callback returns.
            return Ok(false);
        };
        let accounts = service.snapshot().await.ok().and_then(|snapshot| {
            snapshot["providers"]
                .as_array()?
                .iter()
                .find(|pool| pool["id"] == info.provider_id)?["accounts"]
                .as_array()
                .map(|accounts| accounts.iter().filter(|a| a["enabled"] == true).count())
        });
        let mut scopes = self.0.lock().unwrap();
        let scope = scopes.entry(scope).or_insert_with(|| Scope {
            signal: stopping.child_token(),
            state: Arc::new(Mutex::new(CompactionState::default())),
            tasks: Vec::new(),
        });
        if scope.signal.is_cancelled() {
            return Ok(false);
        }
        // Reap completed workers before scheduling another scope-owned lease.
        let mut live = Vec::new();
        for task in scope.tasks.drain(..) {
            if task.is_finished() {
                let _ = task.join();
            } else {
                live.push(task);
            }
        }
        scope.tasks = live;
        let host = Binding {
            lease,
            accounts,
            signal: scope.signal.clone(),
            handle: tokio::runtime::Handle::current(),
        };
        let state = scope.state.clone();
        let provider = info.provider_id.to_owned();
        scope.tasks.push(std::thread::spawn(move || {
            let handle = host.handle.clone();
            let mut state = state.lock().unwrap();
            if !host.signal.is_cancelled() {
                handle.block_on(run_compaction(&host, &mut state, &provider));
            }
            let _ = handle.block_on(host.lease.release());
        }));
        Ok(true)
    }
}
async fn call(
    client: &Client,
    method: &str,
    input: Value,
    signal: CancellationToken,
) -> Result<Value, String> {
    client.request(json!({"port":"services","action":"call","service":"pi-fabric", "serviceVersion":1,"method":method,"input":input}), signal).await
}
struct Binding {
    lease: ScopeServiceLease,
    accounts: Option<usize>,
    signal: CancellationToken,
    handle: tokio::runtime::Handle,
}
#[async_trait]
impl FabricHost for Binding {
    fn tool_names(&self) -> Result<Vec<String>, String> {
        Ok(vec![FABRIC_EXEC_TOOL.into()])
    }
    fn has_context(&self) -> bool {
        !self.signal.is_cancelled()
    }
    fn now_ms(&self) -> i64 {
        if self.signal.is_cancelled() {
            return i64::MAX;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
    fn is_idle(&self) -> bool {
        if self.signal.is_cancelled() {
            return false;
        }
        tokio::task::block_in_place(|| {
            self.handle
                .block_on(self.lease.call("isIdle", json!({}), self.signal.clone()))
        })
        .is_ok_and(|value| value["isIdle"] == true)
    }
    async fn sleep_ms(&self, ms: u64) {
        tokio::select! { _ = self.signal.cancelled() => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {} }
    }
    async fn enabled_accounts(&self, _: &str) -> Result<Option<usize>, String> {
        Ok(self.accounts)
    }
    fn compact(&self) -> CompactOutcome {
        if self.signal.is_cancelled() {
            return CompactOutcome::Error;
        }
        match tokio::task::block_in_place(|| {
            self.handle
                .block_on(self.lease.call("compact", json!({}), self.signal.clone()))
        }) {
            Ok(value) if value["outcome"] == "complete" => CompactOutcome::Complete,
            // IsIdle is not a reservation: a newer prompt may win admission.
            Err(error) if error == "selected execution scope is busy" => CompactOutcome::Skipped,
            Err(error) => {
                if !self.signal.is_cancelled() {
                    eprintln!("multiprovider: failover compaction failed: {error}");
                }
                CompactOutcome::Error
            }
            _ => CompactOutcome::Error,
        }
    }
    fn notify(&self, message: &str, _: &str) {
        // The narrow lease cannot send UI requests. Preserve the TS notice on
        // the diagnostic channel without retaining expired UI authority.
        eprintln!("{message}");
    }
}
