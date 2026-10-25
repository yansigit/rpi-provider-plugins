//! R02 Linux native loading: dlopen the three provider cdylibs, exercise
//! old-plugin/short-vtable compatibility, and execute the packaged Cursor
//! worker through a real registration.
//!
//! macOS/Windows execution is deliberately not attempted. `cargo check
//! --target x86_64-pc-windows-gnu` only compiles this file there.
//!
//! Library paths follow the host `provider_packages.rs` gating: an explicit
//! `RPI_TEST_*` path is validated, otherwise the artifact next to the test
//! binary is used, and a missing artifact skips that library with a SKIP line.
//! `RPI_TEST_PROVIDER_LIBRARY` points at `rpi-providers`;
//! `RPI_TEST_ANTIGRAVITY_LIBRARY` at `rpi-antigravity-auth`;
//! `RPI_TEST_CURSOR_LIBRARY` at `rpi-cursor-sdk`.

#[cfg(target_os = "linux")]
mod linux {
    use rpi_plugin_sdk::{
        AuthRequestControl, EventTag, FreeStringFn, PluginApi, StableToolSchema, StbString,
        StbStringRef, RPI_PLUGIN_ABI_VERSION_UNIFIED,
    };
    use serde_json::{json, Value};
    use std::{
        ffi::{c_void, CStr, CString},
        path::{Path, PathBuf},
        sync::Mutex,
    };

    /// Every registration observed by the fake host, in call order.
    static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The locations the worker bootstrap receives.
    static LOCATIONS: Mutex<Option<Value>> = Mutex::new(None);
    /// Lifecycle hooks registered by a loaded plugin.
    static LIFECYCLES: Mutex<Vec<(usize, usize, usize)>> = Mutex::new(Vec::new());

    fn record(event: String) {
        EVENTS.lock().unwrap().push(event);
    }

    fn events() -> Vec<String> {
        EVENTS.lock().unwrap().clone()
    }

    fn clear() {
        EVENTS.lock().unwrap().clear();
        LIFECYCLES.lock().unwrap().clear();
    }

    extern "C" fn declare(json: StbStringRef) -> i32 {
        record(format!("declare:{}", unsafe { json.as_str() }));
        0
    }
    extern "C" fn flag(name: StbStringRef, description: StbStringRef) -> i32 {
        record(format!(
            "flag:{}:{}",
            unsafe { name.as_str() },
            unsafe { description.as_str() }
        ));
        0
    }
    extern "C" fn command(
        name: StbStringRef,
        description: StbStringRef,
        _handler: rpi_plugin_sdk::CommandHandlerFn,
    ) -> i32 {
        record(format!(
            "command:{}:{}",
            unsafe { name.as_str() },
            unsafe { description.as_str() }
        ));
        0
    }
    extern "C" fn provider(
        id: StbStringRef,
        base_url: StbStringRef,
        api: StbStringRef,
        _request: rpi_plugin_sdk::ProviderRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!(
            "provider:{}:{}:{}",
            unsafe { id.as_str() },
            unsafe { base_url.as_str() },
            unsafe { api.as_str() }
        ));
        0
    }
    extern "C" fn streaming_provider(
        id: StbStringRef,
        base_url: StbStringRef,
        api: StbStringRef,
        _stream: rpi_plugin_sdk::ProviderStreamRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!(
            "streaming:{}:{}:{}",
            unsafe { id.as_str() },
            unsafe { base_url.as_str() },
            unsafe { api.as_str() }
        ));
        0
    }
    extern "C" fn lifecycle(
        begin: rpi_plugin_sdk::PluginBeginShutdownFn,
        finish: rpi_plugin_sdk::PluginFinishShutdownFn,
        user_data: *mut c_void,
    ) -> i32 {
        record("lifecycle".to_owned());
        LIFECYCLES.lock().unwrap().push((
            begin as usize,
            finish as usize,
            user_data as usize,
        ));
        0
    }
    extern "C" fn provider_auth(
        manifest: StbStringRef,
        _request: rpi_plugin_sdk::AuthRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!("auth:{}", unsafe { manifest.as_str() }));
        0
    }
    extern "C" fn provider_runtime(
        manifest: StbStringRef,
        _request: rpi_plugin_sdk::AuthRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!("runtime:{}", unsafe { manifest.as_str() }));
        0
    }
    extern "C" fn event_result(
        tag: EventTag,
        _handler: rpi_plugin_sdk::AuthRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!("event:{tag:?}"));
        0
    }
    extern "C" fn native_command(
        manifest: StbStringRef,
        _request: rpi_plugin_sdk::AuthRequestFn,
        _free: FreeStringFn,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!("native-command:{}", unsafe { manifest.as_str() }));
        0
    }
    extern "C" fn owned_tool(
        schema: *const StableToolSchema,
        _execute: rpi_plugin_sdk::ToolExecuteOwnedFn,
        _poll: rpi_plugin_sdk::ToolPollFn,
        _cancel: rpi_plugin_sdk::ToolCancelFn,
        _destroy: rpi_plugin_sdk::ToolDestroyFn,
        _free: FreeStringFn,
        _set_waker: rpi_plugin_sdk::ToolSetWakerFn,
        _context: *mut c_void,
    ) -> i32 {
        let schema = unsafe { &*schema };
        record(format!("tool:{}", unsafe { schema.name.to_string_lossy() }));
        0
    }
    extern "C" fn native_context(operation: u32, user_data: *mut c_void) -> *mut c_void {
        record(format!("context:{operation}"));
        if operation == 1 {
            std::ptr::null_mut()
        } else {
            // Opaque non-null lease; nothing owns it because the host only
            // checks presence in this registration-only check.
            user_data.wrapping_add(1)
        }
    }
    static CAPABILITIES: &[&str] = &[
        "assistant.durationMs",
        "json.exactNumbers",
        "auth.publicErrors",
        "auth.providerRequest",
        "auth.providerPolicy",
        "auth.configuredValues",
        "auth.dynamicMethods",
        "auth.credentialStoreLock",
        "providers.initialize",
        "providers.awaitedPublication",
        "providers.refreshIsolation",
        "providers.scopedCallbacks",
        "providers.childCatalogs",
        "context.inspect",
        "context.bootstrap",
        "context.snapshot",
        "context.flags",
        "context.abort",
        "context.scopeLifetime",
        "context.trustProvenance",
        "events.lifecycleReasons",
        "events.compactionDetails",
        "events.selectionDetails",
        "events.turnIndex",
        "requests.liveBinding",
        "events.promptContext",
        "tools.provenance",
        "tools.metadata",
        "tools.activation",
        "tools.exactActivation",
        "tools.registrationPolicy",
        "tools.selectionState",
        "tools.boundedProgress",
        "tools.hostBindings",
        "sessions.location",
        "sessions.view",
        "sessions.persistenceLease",
        "commands.owned",
        "ui.notifications",
        "ui.status",
    ];
    extern "C" fn native_request(
        request: StbStringRef,
        control: *const AuthRequestControl,
        out: *mut StbString,
        _user_data: *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return 2;
        }
        if !control.is_null() {
            let size = unsafe { (*control).struct_size } as usize;
            assert!(
                size >= std::mem::size_of::<AuthRequestControl>(),
                "native request control is shorter than this host's contract"
            );
        }
        let text = unsafe { request.as_str() }.to_owned();
        let value: Value = serde_json::from_str(&text).expect("native request must be JSON");
        assert_eq!(value["version"], 1);
        record(format!("native:{text}"));
        let response = match (value["port"].as_str(), value["action"].as_str()) {
            (Some("capabilities"), Some("inspect")) => json!({
                "version": 1,
                "result": CAPABILITIES,
            }),
            (Some("context"), Some("bootstrap")) => {
                let locations = LOCATIONS.lock().unwrap().clone().expect("locations");
                json!({"version": 1, "result": locations})
            }
            (port, action) => json!({
                "version": 1,
                "error": format!(
                    "unexpected native request {} {}",
                    port.unwrap_or("?"),
                    action.unwrap_or("?")
                ),
            }),
        };
        unsafe {
            *out = StbString::from_string(response.to_string());
        }
        if response.get("error").is_some() {
            1
        } else {
            0
        }
    }
    extern "C" fn runtime_action(
        action: u32,
        _args: StbStringRef,
        out: *mut StbString,
        _user_data: *mut c_void,
    ) -> i32 {
        record(format!("runtime-action:{action}"));
        if !out.is_null() {
            unsafe {
                *out = StbString::from_string("{}".into());
            }
        }
        0
    }

    fn api() -> PluginApi {
        PluginApi {
            abi_version: RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<PluginApi>() as u32,
            free_string: rpi_plugin_sdk::callbacks::free_string,
            register_tool: None,
            register_command: Some(command),
            register_shortcut: None,
            register_flag: Some(flag),
            register_provider: Some(provider),
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: None,
            register_resources_discover: None,
            runtime_action,
            dispatch_event: None,
            user_data: std::ptr::null_mut(),
            declare: Some(declare),
            register_before_agent_start: None,
            register_oauth_provider: None,
            register_streaming_provider: Some(streaming_provider),
            register_tool_with_wake: None,
            register_plugin_lifecycle: Some(lifecycle),
            register_provider_auth: Some(provider_auth),
            register_credential_store: None,
            register_provider_wrapper: None,
            delegate_provider_stream: None,
            provider_continuation: None,
            native_request: Some(native_request),
            register_provider_runtime: Some(provider_runtime),
            register_event_result_handler: Some(event_result),
            register_service: None,
            native_context: Some(native_context),
            register_native_command: Some(native_command),
            register_owned_tool_with_wake: Some(owned_tool),
        }
    }

    struct Library {
        handle: *mut c_void,
        path: PathBuf,
    }
    impl Library {
        fn open(path: &Path) -> Self {
            let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                panic!("dlopen {} failed: {}", path.display(), dlerror());
            }
            Self {
                handle,
                path: path.to_owned(),
            }
        }
        /// Call the entrypoint with the raw advertised prefix. Short-vtable
        /// callers pass a pointer to a smaller allocation, so no `&PluginApi`
        /// reference is ever formed for them.
        fn register_raw(&self, api: *const PluginApi) -> i32 {
            let symbol = CString::new("rpi_plugin_register").unwrap();
            let entry = unsafe { libc::dlsym(self.handle, symbol.as_ptr()) };
            assert!(
                !entry.is_null(),
                "{} has no rpi_plugin_register: {}",
                self.path.display(),
                dlerror()
            );
            let entry: unsafe extern "C" fn(*const PluginApi) -> i32 =
                unsafe { std::mem::transmute(entry) };
            unsafe { entry(api) }
        }
        fn register(&self, api: &PluginApi) -> i32 {
            self.register_raw(api)
        }
    }
    impl Drop for Library {
        fn drop(&mut self) {
            // The registered lifecycle already joined every worker.
            unsafe {
                libc::dlclose(self.handle);
            }
        }
    }
    fn dlerror() -> String {
        let error = unsafe { libc::dlerror() };
        if error.is_null() {
            "unknown dynamic loader error".into()
        } else {
            unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
        }
    }

    /// Provider-package gating: an explicit path runs the library, an unset
    /// variable skips it, and an explicit path that is not a file fails.
    fn library(name: &str, env: &str) -> Option<PathBuf> {
        let path = std::env::var_os(env)?;
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "{env} must name the built {name} provider library, got {}",
            path.display()
        );
        Some(path)
    }

    /// Small unique directory without adding a dev-dependency.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut random = [0u8; 12];
            getrandom::getrandom(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "rpi-r02-{tag}-{:x}",
                u128::from_ne_bytes([
                    random[0], random[1], random[2], random[3], random[4], random[5], random[6],
                    random[7], random[8], random[9], random[10], random[11], 0, 0, 0, 0,
                ])
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// Install the library beside its packaged `runtime/` assets, the layout
    /// `installed::worker_entry` resolves from the loaded image.
    fn stage(library: &Path) -> TempDir {
        let staged = TempDir::new("stage");
        std::fs::copy(library, staged.path().join(library.file_name().unwrap())).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime");
        let target = staged.path().join("runtime");
        std::fs::create_dir(&target).unwrap();
        for name in ["main.mjs", "package.json", "LICENSE"] {
            std::fs::copy(source.join(name), target.join(name)).unwrap();
        }
        for name in ["dist", "shared"] {
            // Build outputs, not tracked: a fresh worktree fails here otherwise
            // with a bare NotFound from copy_tree.
            assert!(
                source.join(name).is_dir(),
                "{} is missing; stage the worker first: `(cd worker && node scripts/build.mjs) && node tests/stage-runtime.mjs` in packages/rpi-cursor-sdk",
                source.join(name).display()
            );
            copy_tree(&source.join(name), &target.join(name));
        }
        let dependencies = source.join("node_modules");
        // A dangling link here used to fail as an opaque worker crash; the staged
        // runtime is only executable when its installed tree exists.
        assert!(
            dependencies.is_dir(),
            "{} is missing; run `npm ci --offline --ignore-scripts --no-audit --no-fund` in packages/rpi-cursor-sdk/runtime first (or packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh)",
            dependencies.display()
        );
        std::os::unix::fs::symlink(&dependencies, target.join("node_modules")).unwrap();
        staged
    }

    fn shutdown() {
        let lifecycles = std::mem::take(&mut *LIFECYCLES.lock().unwrap());
        assert!(!lifecycles.is_empty(), "a registered plugin must offer lifecycle");
        for (begin, finish, data) in lifecycles.into_iter().rev() {
            let begin: rpi_plugin_sdk::PluginBeginShutdownFn = unsafe { std::mem::transmute(begin) };
            let finish: rpi_plugin_sdk::PluginFinishShutdownFn =
                unsafe { std::mem::transmute(finish) };
            begin(data as *mut c_void);
            assert_eq!(finish(data as *mut c_void), 0, "plugin did not finish cleanly");
        }
    }

    fn json_events(prefix: &str) -> Vec<Value> {
        events()
            .iter()
            .filter_map(|event| event.strip_prefix(prefix))
            .map(|json| serde_json::from_str(json).unwrap())
            .collect()
    }

    #[test]
    fn linux_provider_libraries_load_old_plugins_short_vtables_and_the_packaged_worker() {
        let providers = library("rpi_providers", "RPI_TEST_PROVIDER_LIBRARY");
        let antigravity = library("rpi_antigravity_auth", "RPI_TEST_ANTIGRAVITY_LIBRARY");
        let cursor = library("rpi_cursor_sdk", "RPI_TEST_CURSOR_LIBRARY");
        for (name, path) in [
            ("rpi-providers", providers.as_ref()),
            ("rpi-antigravity-auth", antigravity.as_ref()),
            ("rpi-cursor-sdk", cursor.as_ref()),
        ] {
            if path.is_none() {
                eprintln!("SKIP: build {name} or set its RPI_TEST_* library path");
            }
        }
        if providers.is_none() && antigravity.is_none() && cursor.is_none() {
            eprintln!("SKIP: set the RPI_TEST_* provider library paths to run native loading");
            return;
        }

        // Old plugin compiled against an older, shorter SDK revision
        // (rpi-providers pins an older host commit than this package) loaded
        // through the current full vtable.
        if let Some(path) = &providers {
            clear();
            let library = Library::open(path);
            assert_eq!(library.register(&api()), 0, "rpi-providers registration failed");
            let events = events();
            for provider in ["deepseek", "openrouter", "llama-cpp", "proxy"] {
                assert!(
                    events.iter().any(|event| event.starts_with(&format!("declare:"))
                        && event.contains(&format!("\"id\":\"{provider}\""))),
                    "missing catalog declaration for {provider}: {events:?}"
                );
                assert!(
                    events.iter().any(|event| event.starts_with(&format!("streaming:{provider}:"))),
                    "missing streaming provider {provider}: {events:?}"
                );
            }
            let catalogs = json_events("declare:");
            assert_eq!(catalogs.len(), 4, "one catalog declaration per provider");
            for catalog in catalogs {
                let providers = catalog["providers"].as_array().unwrap();
                assert_eq!(providers.len(), 1);
                assert!(
                    !providers[0]["models"].as_array().unwrap().is_empty(),
                    "catalog must carry models"
                );
            }
        }

        // A host that predates the trailing vtable slots must still register
        // the old plugin through its one-shot provider fallback.
        if let Some(path) = &providers {
            clear();
            let library = Library::open(path);
            let full = api();
            let size = std::mem::offset_of!(PluginApi, declare);
            // Pointer-aligned storage of exactly the advertised length.
            let mut legacy = vec![0usize; size / std::mem::size_of::<usize>()];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&full as *const PluginApi).cast::<u8>(),
                    legacy.as_mut_ptr().cast::<u8>(),
                    size,
                );
                // The copy ends before `declare`; advertise exactly that size.
                legacy.as_mut_ptr().cast::<u32>().add(1).write(size as u32);
            }
            assert_eq!(
                library.register_raw(legacy.as_ptr().cast::<PluginApi>()),
                0,
                "short-vtable registration failed"
            );
            let events = events();
            assert!(
                !events.iter().any(|event| event.starts_with("declare:")),
                "a short vtable has no declaration slot: {events:?}"
            );
            assert!(
                !events.iter().any(|event| event.starts_with("streaming:")),
                "a short vtable has no streaming slot: {events:?}"
            );
            let one_shot: Vec<_> = events
                .iter()
                .filter(|event| event.starts_with("provider:"))
                .collect();
            assert_eq!(one_shot.len(), 4, "one-shot provider fallback: {events:?}");
        }

        // An old host vtable is refused by plugins that require the newer
        // slots, without reading past the advertised prefix.
        if let Some(path) = &antigravity {
            clear();
            let library = Library::open(path);
            let full = api();
            let size = std::mem::offset_of!(PluginApi, declare) as u32;
            let mut legacy = vec![0usize; size as usize / std::mem::size_of::<usize>()];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&full as *const PluginApi).cast::<u8>(),
                    legacy.as_mut_ptr().cast::<u8>(),
                    size as usize,
                );
                legacy.as_mut_ptr().cast::<u32>().add(1).write(size);
            }
            assert_ne!(
                library.register_raw(legacy.as_ptr().cast::<PluginApi>()),
                0,
                "Antigravity must refuse a host without its required slots"
            );
            assert!(
                !events().iter().any(|event| event.starts_with("lifecycle")),
                "refused registration must not publish a lifecycle"
            );
        }

        // A version future host is refused before any slot is read.
        if let Some(path) = &providers {
            clear();
            let library = Library::open(path);
            let mut future = api();
            future.abi_version = RPI_PLUGIN_ABI_VERSION_UNIFIED + 1;
            assert_eq!(library.register(&future), 1);
            assert!(events().is_empty(), "a refused vtable must not register");
        }

        // Newest host vtable: Antigravity registers its provider, catalogs,
        // commands and runtime ports.
        if let Some(path) = antigravity {
            clear();
            let library = Library::open(&path);
            assert_eq!(library.register(&api()), 0, "rpi-antigravity-auth registration failed");
            let events = events();
            assert!(
                events.iter().any(|event| event.starts_with("declare:") && event.contains("\"id\":\"google\"")),
                "missing Antigravity catalog: {events:?}"
            );
            let catalogs = json_events("declare:");
            assert_eq!(catalogs.len(), 1);
            assert_eq!(catalogs[0]["providers"][0]["id"], "google");
            assert_eq!(
                catalogs[0]["providers"][0]["models"].as_array().unwrap().len(),
                6
            );
            assert!(
                events.iter().any(|event| event.starts_with("streaming:google:")),
                "missing streaming provider: {events:?}"
            );
            assert!(events.iter().any(|event| event.starts_with("native-command:")));
            assert!(events.iter().any(|event| event.starts_with("event:")));
            assert!(events.iter().any(|event| event.starts_with("auth:")));
            assert!(events.iter().any(|event| event.starts_with("runtime:")));
            assert!(events.iter().any(|event| event == "lifecycle"));
            shutdown();
        }

        // Newest host vtable: Cursor registers through the packaged worker.
        if let Some(path) = cursor {
            clear();
            let temp = TempDir::new("cursor");
            let locations = json!({
                "cwd": temp.path().join("project"),
                "agentDir": temp.path().join("agent"),
                "sessionDir": temp.path().join("sessions"),
            });
            for directory in ["project", "agent", "sessions"] {
                std::fs::create_dir(temp.path().join(directory)).unwrap();
            }
            *LOCATIONS.lock().unwrap() = Some(locations);
            let staged = stage(&path);
            let library = Library::open(&staged.path().join(path.file_name().unwrap()));
            assert_eq!(library.register(&api()), 0, "rpi-cursor-sdk registration failed");
            let events = events();
            let catalogs = json_events("declare:");
            assert!(catalogs.iter().any(|catalog| catalog["providers"][0]["id"] == "cursor"));
            let cursor_catalog = catalogs
                .iter()
                .find(|catalog| catalog["providers"][0]["id"] == "cursor")
                .unwrap();
            assert!(
                !cursor_catalog["providers"][0]["models"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "packaged worker must publish Cursor models"
            );
            let commands: Vec<_> = events
                .iter()
                .filter(|event| event.starts_with("native-command:"))
                .collect();
            assert_eq!(commands.len(), 10, "packaged worker commands: {commands:?}");
            assert!(events.iter().any(|event| event.starts_with("flag:")));
            assert!(events.iter().any(|event| event.starts_with("tool:")));
            assert!(events.iter().any(|event| event.starts_with("event:")));
            assert!(events.iter().any(|event| event.starts_with("auth:")));
            assert!(events.iter().any(|event| event.starts_with("runtime:")));
            assert!(events
                .iter()
                .any(|event| event.starts_with("streaming:cursor:https://cursor.com:cursor-sdk")));
            assert!(
                events.iter().any(|event| event.starts_with("native:")
                    && event.contains("\"port\":\"capabilities\"")),
                "registration must negotiate host capabilities: {events:?}"
            );
            assert!(
                events.iter().any(|event| event.starts_with("native:")
                    && event.contains("\"action\":\"bootstrap\"")),
                "worker startup must resolve packaged locations: {events:?}"
            );
            shutdown();

            // The same image without its packaged runtime must refuse
            // registration instead of silently starting an unpackaged worker.
            let unstaged = TempDir::new("unstaged");
            std::fs::copy(&path, unstaged.path().join(path.file_name().unwrap())).unwrap();
            let plain = Library::open(&unstaged.path().join(path.file_name().unwrap()));
            assert_ne!(
                plain.register(&api()),
                0,
                "an image without runtime assets must not register"
            );
        }
    }
}
