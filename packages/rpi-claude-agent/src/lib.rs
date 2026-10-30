//! Claude provider for the published rpi plugin SDK (0.12), on the official
//! Claude Agent SDK.
//!
//! It registers one streaming provider and drives its own small Node worker
//! (`worker/main.mjs`), which runs the Agent SDK with Claude Code's tools off and
//! rpi's tools bridged in. The worker transport is shared with `rpi-cursor-sdk`
//! through `#[path]`, never copied.
// An inline module keeps the shared files' own nested paths (worker/wire.rs).
// Their formatting and test-only lints belong to rpi-cursor-sdk.
#[rustfmt::skip]
#[allow(
    clippy::sliced_string_as_bytes,
    clippy::err_expect,
    clippy::unnecessary_map_or
)]
#[path = "../../rpi-cursor-sdk/src"]
mod shared {
    pub mod installed;
    pub mod runtime;
    pub mod worker;
}
pub use shared::{installed, runtime, worker};

/// Replaces rpi-cursor-sdk's native router: this worker makes no host callbacks.
pub mod native_host {
    use crate::worker::{HostCall, HostHandler};
    use std::sync::Arc;

    pub fn handler() -> HostHandler {
        Arc::new(|call: HostCall| {
            Box::pin(async move {
                Err(format!(
                    "host callback {} is unavailable to the Claude provider",
                    call.method
                ))
            })
        })
    }

    pub struct Router;
    impl Router {
        pub fn standing(self: &Arc<Self>) -> HostHandler {
            handler()
        }
        pub async fn close(&self) -> Result<(), String> {
            Ok(())
        }
    }
}

use rpi_plugin_sdk::{
    self as sdk, callbacks::Output, PluginApi, ProviderStreamCompletionFn, ProviderStreamControl,
    ProviderStreamEmitFn, StbString, StbStringRef,
};
use runtime::{drive, Owner, Session};
use serde_json::{json, Value};
use std::{ffi::c_void, path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use worker::WorkerConfig;

/// The static catalog, so `--list-models` never starts Node.
pub const MODELS: &str = include_str!("../models.json");

struct Plugin {
    tokio: tokio::runtime::Runtime,
    stopping: CancellationToken,
    owner: OnceCell<Arc<Owner>>,
}

/// Worker locations come from the process, not from a host bootstrap port.
pub fn locations() -> Result<(WorkerConfig, Value), String> {
    let cwd = std::env::current_dir().map_err(|_| "Claude needs a working directory")?;
    let agent_dir = match std::env::var_os("RPI_CODING_AGENT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("Claude needs HOME")?).join(".rpi"),
    };
    let agent_dir = cwd.join(agent_dir);
    let config = WorkerConfig {
        node: std::env::var_os("RPI_CLAUDE_AGENT_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| "node".into()),
        entry: installed::worker_entry()?,
        cwd: cwd.clone(),
    };
    let locations = json!({"agentDir":agent_dir,"cwd":cwd});
    Ok((config, locations))
}

pub fn register(api: &PluginApi) -> i32 {
    let (Some(lifecycle), Some(declare), Some(provider)) = (
        api.register_plugin_lifecycle,
        api.declare,
        api.register_streaming_provider,
    ) else {
        eprintln!("rpi-claude-agent: the host lacks lifecycle, declaration or streaming support");
        return 1;
    };
    let Ok(tokio) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return 1;
    };
    let plugin = Box::into_raw(Box::new(Plugin {
        tokio,
        stopping: CancellationToken::new(),
        owner: OnceCell::new(),
    }));
    // Same order as rpi_plugin_sdk::provider::register_with_context.
    if lifecycle(begin_shutdown, finish_shutdown, plugin.cast()) != 0 {
        drop(unsafe { Box::from_raw(plugin) });
        return 1;
    }
    let Ok(models) = serde_json::from_str::<Value>(MODELS) else {
        return 1;
    };
    let manifest = json!({"providers":[{"id":"claude-code","models":models,"apiKeyEnv":"CLAUDE_CODE_OAUTH_TOKEN","requiresAuth":true}]});
    let status = declare(StbStringRef::from_str(&manifest.to_string()));
    if status != 0 {
        return status;
    }
    provider(
        StbStringRef::from_str("claude-code"),
        StbStringRef::from_str("https://api.anthropic.com"),
        StbStringRef::from_str("claude-agent-sdk"),
        stream,
        sdk::callbacks::free_string,
        plugin.cast(),
    )
}

extern "C" fn begin_shutdown(data: *mut c_void) {
    sdk::guard_or((), || {
        unsafe { &*data.cast::<Plugin>() }.stopping.cancel();
    });
}

extern "C" fn finish_shutdown(data: *mut c_void) -> i32 {
    sdk::guard_or(1, || {
        let plugin = unsafe { Box::from_raw(data.cast::<Plugin>()) };
        plugin.stopping.cancel();
        if let Some(owner) = plugin.owner.get() {
            if let Err(error) = plugin.tokio.block_on(owner.close()) {
                eprintln!("rpi-claude-agent shutdown: {error}");
            }
        }
        // Dropping the runtime joins its worker threads; no plugin code runs after this.
        drop(plugin);
        0
    })
}

async fn open(stopping: &CancellationToken) -> Result<Arc<Owner>, String> {
    let (config, locations) = locations()?;
    let session = Session::open(config, locations, Arc::new(native_host::Router)).await?;
    Ok(Owner::new(session, stopping.child_token()))
}

#[allow(clippy::too_many_arguments)]
extern "C" fn stream(
    input: StbStringRef,
    emit: Option<ProviderStreamEmitFn>,
    data: *mut c_void,
    control: *const ProviderStreamControl,
    _: Option<ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let Ok(input) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        let plugin = unsafe { &*context.cast::<Plugin>() };
        let cancelled = || {
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data, 0) != 0)
        };
        let mut partial = None;
        let result = plugin.tokio.block_on(async {
            let owner = plugin
                .owner
                .get_or_try_init(|| open(&plugin.stopping))
                .await?;
            let invocation = owner.enter()?;
            let call = invocation
                .session()
                .call(
                    "provider.stream",
                    json!({
                        "entry": "streamSimple",
                        "authMode": "replace",
                        "model": input["model"],
                        "context": input["context"],
                        "options": input["options"],
                    }),
                    native_host::handler(),
                    &invocation.signal,
                    &cancelled,
                )
                .await?;
            drive(call, &invocation.signal, &cancelled, |event| {
                if let Some(message) = event
                    .get("partial")
                    .or_else(|| event.get("message"))
                    .or_else(|| event.get("error"))
                    .filter(|value| value.is_object())
                {
                    partial = Some(message.clone());
                }
                if emit
                    .is_some_and(|emit| emit(StbString::from_string(event.to_string()), data) != 0)
                {
                    Err("Claude stream cancelled by its consumer".into())
                } else {
                    Ok(())
                }
            })
            .await
        });
        let message = result.unwrap_or_else(|error| {
            let mut message = partial.unwrap_or_else(|| json!({"role":"assistant","api":"claude-agent-sdk","provider":"claude-code","model":input["model"]["id"],"content":[],"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"timestamp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64}));
            message["stopReason"] = if cancelled() { "aborted" } else { "error" }.into();
            message["errorMessage"] = error.into();
            message
        });
        unsafe { Output::from_raw(out) }.write(message.to_string());
        0
    })
}

mod export {
    // The host passes a valid vtable; register_entrypoint validates it before use.
    #![allow(clippy::not_unsafe_ptr_arg_deref)]
    rpi_plugin_sdk::export_plugin!(super::register);
}
