//! Native command registration binding `commands::Ui` to the dialogs and UI ports.
//!
//! The five slash commands Pi's multiprovider extension registers are exposed as
//! owned asynchronous commands (`PluginApi.register_native_command`). Each one
//! carries the upstream manifest name and description; the host invokes it with
//! `{version:1,command,args}` and reads `{version:1,result}` or
//! `{version:1,error}`.
//!
//! The callback captures its invocation context on the foreign thread, then
//! runs the ported flow on the registration's runtime. Dialog controls go to
//! the host's awaited `dialogs` port as `{port:"dialogs",action:"show",ui:...}`;
//! notifications go to `{port:"ui",action:"notify",...}`. `has_ui` is read once
//! per invocation from `context/snapshot` (`hasUI`) so the flows keep Pi's
//! non-interactive no-op behaviour instead of failing a dialog. The TUI gate
//! (`ctx.mode`) remains part of [`CommandHost`]'s `Port::mode`, which the host
//! implementation answers from the same snapshot.
use crate::commands::{
    accounts,
    login,
    logout,
    port::Port,
    switch,
    vprovider::{self, VproviderSources},
    Ui,
};
use rpi_plugin_sdk::{
    callbacks::free_string,
    provider::{native::Client, ProviderContext},
    AuthRequestControl, PluginApi, RegisterProviderAuthFn, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    future::Future,
    mem::ManuallyDrop,
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// One registered command: Pi's extension command name and description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// `pi.registerCommand` calls in `extensions/multiprovider.ts` (lines 1096,
/// 1288, 1334, 1347, 1473), in registration order.
pub const COMMANDS: [Command; 5] = [
    Command {
        name: "multilogin",
        description: "Manage a provider pool, Pi default auth, schedulers, and accounts",
    },
    Command {
        name: "multilogout",
        description: "Remove an account saved by /multilogin",
    },
    Command {
        name: "accounts",
        description: "Show multiprovider account pools and health",
    },
    Command {
        name: "switch-account",
        description: "Switch the pooled account used by the current model for this session",
    },
    Command {
        name: "vprovider",
        description: "Create virtual providers that map one model across multiple provider models",
    },
];

/// Host capabilities the command layer reads. The registration owner negotiates
/// them with [`Client::require_capabilities`] before constructing [`Commands`].
pub const CAPABILITIES: &[&str] = &["commands.owned", "ui.notifications", "context.snapshot"];

/// Everything the command flows read from Pi's session and the extension.
/// It is the store/service `Port` plus the model catalog `/vprovider` needs.
pub trait CommandHost: Port + VproviderSources + Send + Sync {}
impl<T> CommandHost for T where T: Port + VproviderSources + Send + Sync {}

/// The host state the flows read: whether an interactive UI is attached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub has_ui: bool,
}

/// `ctx.hasUI` from the host's authoritative `context/snapshot`.
pub async fn view(client: &Client, signal: &CancellationToken) -> Result<View, String> {
    let snapshot = client
        .request(
            json!({"port":"context","action":"snapshot"}),
            signal.clone(),
        )
        .await?;
    Ok(View {
        has_ui: snapshot["hasUI"] == true,
    })
}

/// Runs one named flow. `args` is the raw slash-command argument string.
pub async fn run<P: CommandHost, U: Ui>(
    port: &P,
    ui: &U,
    command: &str,
    args: &str,
) -> Result<(), String> {
    match command {
        "multilogin" => login::multilogin(ui, port, args).await,
        "multilogout" => logout::multilogout(ui, port, args).await,
        "accounts" => accounts::accounts(ui, port).await,
        "switch-account" => switch::switch_account(ui, port, args).await,
        "vprovider" => vprovider::vprovider(ui, port, args).await,
        _ => Err(format!("Unknown Multiprovider command \"{command}\"")),
    }
}

/// [`Ui`] over the host's awaited dialogs and ordered notifications. The
/// invocation signal is the command's own token, so closing the command or the
/// plugin cancels a pending dialog without affecting other owners.
struct Dialogs {
    client: Client,
    view: View,
    signal: CancellationToken,
}

impl Ui for Dialogs {
    fn has_ui(&self) -> bool {
        self.view.has_ui
    }
    async fn show(&self, control: Value) -> Result<Option<Value>, String> {
        let result = self
            .client
            .request(
                json!({"port":"dialogs","action":"show","ui":control}),
                self.signal.clone(),
            )
            .await?;
        if result.is_null() || result["type"] == "cancel" {
            return Ok(None);
        }
        if !result.is_object() {
            return Err("Invalid native dialog response".into());
        }
        Ok(Some(result))
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

/// Per-registration command state. No credentials live here: the Port owns the
/// account store and the client owns only private host leases.
struct Runtime<P> {
    context: ProviderContext,
    port: Arc<P>,
    client: Client,
    stopping: CancellationToken,
}

/// The registration handle. It owns the callback contexts the host receives and
/// the runtime every command runs on.
///
/// Keep it alive for as long as the host may invoke a command. Dropping it
/// cancels the shared token's work, releases every registered context and stops
/// the registration runtime, so the owning plugin's `finish_shutdown` hook is
/// the right place to drop it — after the host has stopped admitting commands,
/// as `commands.owned` requires.
pub struct Commands<P> {
    runtime: Arc<Runtime<P>>,
    register_command: RegisterProviderAuthFn,
    contexts: Vec<usize>,
}

impl<P: CommandHost + 'static> Commands<P> {
    /// Requires the owned-command slot and the native runtime port. Registration
    /// itself is separate so a loader can validate capabilities first.
    pub fn new(
        api: &PluginApi,
        port: Arc<P>,
        stopping: CancellationToken,
    ) -> Result<Self, String> {
        let register_command = api
            .register_native_command
            .ok_or("rpi-multiprovider requires owned native commands; upgrade rpi")?;
        Ok(Self {
            runtime: Arc::new(Runtime {
                context: ProviderContext::new(),
                port,
                client: Client::new(api)?,
                stopping,
            }),
            register_command,
            contexts: Vec::new(),
        })
    }

    /// Registers the five commands, in Pi's order. A refused registration aborts
    /// the whole plugin load; there is no partial command surface.
    pub fn register(&mut self) -> Result<(), String> {
        for command in COMMANDS {
            let manifest = json!({
                "version": 1,
                "name": command.name,
                "description": command.description,
            })
            .to_string();
            // One retained reference per registered command. The callback borrows
            // it through `ManuallyDrop`, and `Drop` releases it when the host has
            // stopped invoking commands.
            let context = Arc::into_raw(self.runtime.clone()) as *mut Runtime<P> as *mut c_void;
            self.contexts.push(context as usize);
            if (self.register_command)(
                StbStringRef::from_str(&manifest),
                command_request::<P>,
                free_string,
                context,
            ) != 0
            {
                return Err(format!(
                    "rpi-multiprovider could not register /{}",
                    command.name
                ));
            }
        }
        Ok(())
    }
}

impl<P> Drop for Commands<P> {
    fn drop(&mut self) {
        // Stop in-flight commands before their runtime goes away; the host is
        // responsible for having returned every callback first.
        self.runtime.context.begin_shutdown();
        for context in self.contexts.drain(..) {
            unsafe {
                drop(Arc::from_raw(context as *const Runtime<P>));
            }
        }
    }
}

extern "C" fn command_request<P: CommandHost + 'static>(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        // Borrow the retained registration state; never release it here.
        let state = ManuallyDrop::new(unsafe { Arc::from_raw(context.cast::<Runtime<P>>()) });
        let state: &Runtime<P> = &state;
        let result = (|| -> Result<(), String> {
            let value: Value = serde_json::from_str(unsafe { input.as_str() })
                .map_err(|_| "Invalid Multiprovider command request".to_owned())?;
            if value["version"] != 1 {
                return Err("Unsupported Multiprovider command version".into());
            }
            let command = value["command"]
                .as_str()
                .ok_or("Missing Multiprovider command name")?;
            if !COMMANDS.iter().any(|known| known.name == command) {
                return Err(format!("Unknown Multiprovider command \"{command}\""));
            }
            let args = match value.get("args") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(args)) => args.clone(),
                _ => return Err("Invalid Multiprovider command arguments".into()),
            };
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<AuthRequestControl>() as u32
            {
                return Err("Short Multiprovider cancellation control".into());
            }
            let client = state.client.scoped()?;
            let signal = state.stopping.child_token();
            if signal.is_cancelled() {
                return Err("This operation was aborted".into());
            }
            let work_signal = signal.clone();
            let port = state.port.clone();
            let command = command.to_owned();
            let cancelled = || {
                unsafe { control.as_ref() }
                    .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
            };
            execute(state, &signal, &cancelled, async move {
                let view = view(&client, &work_signal).await?;
                if work_signal.is_cancelled() {
                    return Err("This operation was aborted".into());
                }
                let ui = Dialogs {
                    client,
                    view,
                    signal: work_signal,
                };
                run(&*port, &ui, &command, &args).await
            })?;
            Ok(())
        })();
        output(
            out,
            match result {
                Ok(()) => json!({"version":1,"result":null}),
                Err(error) => json!({"version":1,"error":error}),
            },
        )
    })
}

/// Runs one command on the registration runtime, polling the host control. A
/// cancellation cancels the command token, so pending dialogs and notifications
/// stop; the callback returns as soon as the flow unwinds.
fn execute<P: CommandHost + 'static, T: Send + 'static>(
    state: &Runtime<P>,
    signal: &CancellationToken,
    cancelled: &dyn Fn() -> bool,
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    if cancelled() {
        signal.cancel();
    }
    state.context.run(signal, async {
        let task = tokio::spawn(work);
        poll_cancel(signal, cancelled, task)
            .await
            .map_err(|_| "Multiprovider operation stopped".to_owned())?
    })?
}

async fn poll_cancel<T>(
    signal: &CancellationToken,
    cancelled: &dyn Fn() -> bool,
    work: tokio::task::JoinHandle<T>,
) -> Result<T, String> {
    tokio::pin!(work);
    let mut timer = tokio::time::interval(Duration::from_millis(10));
    loop {
        if cancelled() {
            signal.cancel();
        }
        tokio::select! {biased; value=&mut work=>return value.map_err(|_| "Multiprovider command task stopped".to_owned()), _=timer.tick()=>{}}
    }
}

fn output(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 2;
    }
    unsafe {
        *out = StbString::from_string(value.to_string());
    }
    0
}
