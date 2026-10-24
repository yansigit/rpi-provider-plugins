//! Test-only transparent loader: audit successful opaque-handle operations while
//! forwarding the unmodified host ABI into the actual multiprovider cdylib.
//! Never logs credential payloads, options, contexts, or descriptors.
use rpi_plugin_sdk::{self as sdk, PluginApi, StbString, StbStringRef};
use serde_json::{json, Value};
use std::{
    ffi::{c_void, CString},
    io::Write,
    sync::OnceLock,
};
static CONTINUATION: OnceLock<sdk::AuthRequestFn> = OnceLock::new();
#[link(name = "dl")]
extern "C" {
    fn dlopen(path: *const i8, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
}
extern "C" fn continuation(
    input: StbStringRef,
    control: *const sdk::AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
) -> i32 {
    let request: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
    let result = CONTINUATION.get().unwrap()(input, control, out, data);
    let response: Value = serde_json::from_str(&unsafe { (*out).to_string_lossy() }).unwrap();
    let mut event = json!({"action":request["action"],"status":result});
    if result != 0 {
        event["error"] = response["error"].clone();
    }
    for key in ["target", "receipt", "released"] {
        if let Some(value) = request.get(key).or_else(|| response.get(key)) {
            event[key] = value.clone();
        }
    }
    let path = std::env::var_os("VIRTUAL_AUDIT").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{event}").unwrap();
    result
}
sdk::export_plugin!(|api: &PluginApi| {
    CONTINUATION
        .set(api.provider_continuation.unwrap())
        .unwrap();
    let path = CString::new(std::env::var("VIRTUAL_PACKAGE_DLL").unwrap()).unwrap();
    unsafe {
        let library = dlopen(path.as_ptr(), 2); // kept alive until process exit, like host native libraries
        assert!(!library.is_null(), "cannot load real multiprovider DLL");
        let symbol = dlsym(library, b"rpi_plugin_register\0".as_ptr().cast());
        assert!(!symbol.is_null());
        let register: extern "C" fn(*const PluginApi) -> i32 = std::mem::transmute(symbol);
        let mut forwarded = *api;
        forwarded.provider_continuation = Some(continuation);
        register(&forwarded)
    }
});
