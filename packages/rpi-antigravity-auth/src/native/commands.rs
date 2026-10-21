//! The menu owns dialogs per invocation; setup and approval watching retain the registration.
use super::{vault::State, Runtime};
use crate::{
    commands::{self, render::Ui, Accounts, Menu},
    vault::{
        ceremony::Failure,
        connection::{Enrolled, SubcConnector},
        file_store::FileStore,
        runtime,
        setup::{self, Error, Setup},
    },
};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Watch(Mutex<Option<tokio::task::JoinHandle<()>>>);
impl Watch {
    pub async fn finish(&self) {
        let task = self.0.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
struct Host {
    state: Arc<State>,
    client: Client,
}
impl setup::Port for Host {
    type Client = Arc<Enrolled<SubcConnector>>;
    async fn connect(&self) -> Result<Self::Client, Failure> {
        Enrolled::connect(runtime::connector().map_err(Failure::Storage)?)
            .await
            .map_err(|_| Failure::Storage("Claustrum connection unavailable".into()))
    }
    async fn close(&self, client: Self::Client) {
        client.close().await;
    }
    async fn host_slot(&self) -> Result<String, Failure> {
        let value = self.client.request(
            json!({"port":"auth","action":"credentialStore","providerId":"google","input":{"operation":"inspect"}}),
            CancellationToken::new(),
        ).await.map_err(Failure::Storage)?;
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Failure::Storage("Invalid host credential classification".into()))
    }
    async fn changed(&self, _: Value) -> Result<(), Error> {
        // Closing the menu must not interrupt an admitted credential transition.
        self.state
            .synchronize(&self.client, CancellationToken::new())
            .await
            .map_err(setup_error)
    }
    fn now_ms(&self) -> u64 {
        runtime::now() as u64
    }
    fn mint_secret(&self) -> Result<String, Failure> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)
            .map_err(|_| Failure::Storage("Unable to generate enrollment secret".into()))?;
        Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
    }
}
fn setup_error(error: String) -> Error {
    use crate::vault::source::{CHANGED, LOCAL_EXCLUDED, SLOT_CONFLICT, UNAPPROVED, UNREADABLE};
    // Only the source controller's PiVaultSetupError messages are actionable refusals.
    [
        CHANGED,
        LOCAL_EXCLUDED,
        SLOT_CONFLICT,
        UNAPPROVED,
        UNREADABLE,
    ]
    .into_iter()
    .find(|known| *known == error)
    .map(Error::Refused)
    .unwrap_or_else(|| Failure::Storage(error).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synchronization_refusals_keep_the_original_error_class() {
        use crate::vault::source::{
            CHANGED, LOCAL_EXCLUDED, SLOT_CONFLICT, UNAPPROVED, UNREADABLE,
        };
        for message in [
            CHANGED,
            LOCAL_EXCLUDED,
            SLOT_CONFLICT,
            UNAPPROVED,
            UNREADABLE,
        ] {
            assert!(matches!(setup_error(message.into()), Error::Refused(text) if text == message));
        }
        assert!(matches!(setup_error("Pi slot: io".into()), Error::Other(_)));
    }
}
#[derive(Clone)]
struct Port {
    setup: Arc<Setup<FileStore, Host>>,
    watch: Arc<Watch>,
    stopping: CancellationToken,
    salt: [u8; 16],
}
impl commands::Port for Port {
    async fn status(&self) -> Result<Value, Error> {
        self.setup.status().await
    }
    async fn accounts(&self) -> Result<Option<Accounts>, String> {
        let vault = self.setup.port.state.vault()?;
        Ok(Some(Accounts {
            list: vault.list().await?,
            reach: vault.grant_reach(),
        }))
    }
    async fn use_vault(&self, route: Option<String>) -> Result<(), Error> {
        self.setup.use_vault(route.as_deref()).await.map(|_| ())
    }
    async fn use_local(&self) -> Result<(), Error> {
        self.setup.use_local().await.map(|_| ())
    }
    async fn disconnect(&self) -> Result<(), Error> {
        self.setup.disconnect().await
    }
    async fn change_route(&self, route: String, accept: bool) -> Result<(), Error> {
        self.setup
            .port
            .state
            .vault()
            .map_err(Failure::Storage)?
            .change_route(&route, accept)
            .await
            .map_err(|e| Failure::Storage(e).into())
    }
    async fn connect_step(&self) -> Result<Value, Error> {
        self.setup.connect_step().await
    }
    fn watch(&self) {
        let mut task = self.watch.0.lock().unwrap();
        if self.stopping.is_cancelled() || task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let port = self.clone();
        *task = Some(tokio::spawn(async move {
            let menu = Menu {
                salt: port.salt,
                port: Arc::new(port),
            };
            if let Some((text, kind)) = commands::watch::run(&menu, &menu.port.stopping).await {
                let _ = menu
                    .port
                    .setup
                    .port
                    .client
                    .request(
                        json!({"port":"ui","action":"notify","text":text,"kind":kind}),
                        menu.port.stopping.clone(),
                    )
                    .await;
            }
        }));
    }
}
impl commands::watch::Clock for Port {
    fn now_ms(&self) -> f64 {
        runtime::now()
    }
    async fn sleep(&self, milliseconds: u64) {
        tokio::time::sleep(Duration::from_millis(milliseconds)).await;
    }
    async fn source_status(&self) -> Result<Value, Error> {
        Ok(self.setup.source.status().await)
    }
}
struct Dialogs {
    client: Client,
    signal: CancellationToken,
}
impl Ui for Dialogs {
    async fn select(&self, title: String, labels: Vec<String>) -> Result<Option<usize>, String> {
        let choices: Vec<_> = labels
            .into_iter()
            .enumerate()
            .map(|(i, label)| json!({"value":i.to_string(),"label":label}))
            .collect();
        let result = self.client.request(
            json!({"port":"dialogs","action":"show","ui":{"kind":"select","title":title,"choices":choices}}),
            self.signal.clone(),
        ).await?;
        if result.is_null() || result["type"] == "cancel" {
            return Ok(None);
        }
        result["values"][0]
            .as_str()
            .and_then(|v| v.parse().ok())
            .map(Some)
            .ok_or_else(|| "Invalid menu selection".into())
    }
    async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
        let result = self.client.request(
            json!({"port":"dialogs","action":"show","ui":{"kind":"confirm","title":title,"description":message}}),
            self.signal.clone(),
        ).await?;
        Ok(result["confirmed"] == true)
    }
    async fn notify(&self, text: &str, kind: &str) -> Result<(), String> {
        self.client
            .request(
                json!({"port":"ui","action":"notify","text":text,"kind":kind}),
                self.signal.clone(),
            )
            .await
            .map(|_| ())
    }
}
pub async fn run(
    state: Arc<State>,
    owner: Client,
    ui: Client,
    watch: Arc<Watch>,
    stopping: CancellationToken,
    signal: CancellationToken,
) -> Result<(), String> {
    static SALT: OnceLock<Result<[u8; 16], String>> = OnceLock::new();
    let salt = SALT
        .get_or_init(|| {
            let mut salt = [0; 16];
            getrandom::getrandom(&mut salt)
                .map_err(|_| "Unable to generate menu identifiers".to_owned())?;
            Ok(salt)
        })
        .clone()?;
    let authority = state.vault()?.authority.clone();
    let port = Arc::new(Port {
        setup: Arc::new(Setup {
            source: authority,
            port: Arc::new(Host {
                state,
                client: owner,
            }),
        }),
        watch,
        stopping,
        salt,
    });
    commands::render::run(&Menu { port, salt }, &Dialogs { client: ui, signal }).await
}

pub(super) extern "C" fn request(
    input: rpi_plugin_sdk::StbStringRef,
    control: *const rpi_plugin_sdk::AuthRequestControl,
    out: *mut rpi_plugin_sdk::StbString,
    context: *mut std::ffi::c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let state = unsafe { &*context.cast::<Runtime>() };
        let result = (|| {
            let value: Value = serde_json::from_str(unsafe { input.as_str() })
                .map_err(|_| "Invalid Antigravity command request")?;
            if value["version"] != 1 || value["command"] != "antigravity" {
                return Err("Invalid Antigravity command request".into());
            }
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<rpi_plugin_sdk::AuthRequestControl>() as u32
            {
                return Err("Short Antigravity cancellation control".into());
            }
            let ui = state.client.scoped()?;
            let owner = state.client.clone();
            let vault = state.vault.clone();
            let watch = state.watch.clone();
            let stopping = state.stopping.clone();
            let signal = stopping.child_token();
            let work_signal = signal.clone();
            super::execute(
                state,
                &signal,
                || unsafe { control.as_ref() }.is_some_and(|v| (v.is_cancelled)(v.user_data) != 0),
                async move { run(vault, owner, ui, watch, stopping, work_signal).await },
            )
        })();
        super::output(
            out,
            match result {
                Ok(()) => json!({"version":1,"result":null}),
                Err(error) => json!({"version":1,"error":commands::seam::text(&error)}),
            },
        )
    })
}
