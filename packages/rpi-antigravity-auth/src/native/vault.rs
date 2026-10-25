//! Registration state follows committed slot transitions; reads never activate a source.
use crate::vault::{
    import_guard::{Admission, Custody, Slot as SlotState, Stash},
    runtime::Vault,
    slot::Slot,
    source,
};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub enum Mode {
    Local,
    Custody,
    Refusing(String),
}
pub struct State {
    vault: OnceLock<Arc<Vault>>,
    mode: Mutex<Mode>,
    serial: tokio::sync::Mutex<()>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            vault: OnceLock::new(),
            mode: Mutex::new(Mode::Refusing(
                "Antigravity has not initialized its account source.".into(),
            )),
            serial: tokio::sync::Mutex::new(()),
        }
    }
}
impl State {
    pub fn initialize(&self, agent_dir: &Path) -> Result<(), String> {
        let vault = Arc::new(Vault::new(&agent_dir.join("antigravity-auth-vault"))?);
        self.vault
            .set(vault)
            .map_err(|_| "Antigravity account source is already initialized".into())
    }
    pub fn vault(&self) -> Result<Arc<Vault>, String> {
        self.vault
            .get()
            .cloned()
            .ok_or_else(|| "Antigravity account source is not initialized".into())
    }
    pub fn mode(&self) -> Mode {
        self.mode.lock().unwrap().clone()
    }
    pub async fn synchronize(
        &self,
        client: &Client,
        signal: CancellationToken,
    ) -> Result<(), String> {
        let _serial = self.serial.lock().await;
        let result = async {
            let vault = self.vault()?;
            vault.authority.synchronize(|mode| client.request(json!({"port":"auth","action":"credentialStore","providerId":"google","input":{"mode":mode["mode"]}}), signal)).await
        }.await;
        *self.mode.lock().unwrap() = match &result {
            Ok(mode) if mode["mode"] == "local" => Mode::Local,
            Ok(_) => Mode::Custody,
            Err(error) => Mode::Refusing(error.clone()),
        };
        result.map(|_| ())
    }
    /// The host already holds its credential-store lock; never re-enter a host auth port here.
    pub fn transaction(&self, input: &Value) -> Result<Value, String> {
        if input["expiryUnit"] != "seconds" {
            return Err("Unsupported legacy credential expiry unit".into());
        }
        let path = Path::new(
            input["authPath"]
                .as_str()
                .ok_or("Missing host credential path")?,
        );
        if !path.is_absolute() {
            return Err("Invalid host credential path".into());
        }
        if input["input"]["operation"] == "inspect" {
            // Pi inspects the stored record, without resolving key commands or environment names.
            let record = std::fs::read(path)
                .ok()
                .and_then(|bytes| {
                    let text = String::from_utf8_lossy(&bytes);
                    serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(&text))
                        .ok()
                })
                .unwrap_or(Value::Null);
            return Ok(crate::vault::setup::classify_host_slot(&record["google"]).into());
        }
        let vault = self.vault()?;
        let mut files = vault
            .authority
            .store
            .authority_files()
            .map_err(|e| e.to_string())?;
        crate::vault::import_guard::guard_storage(&files)?;
        let stash_path = vault
            .authority
            .mode_file
            .with_file_name("google-login-stash.json");
        let slot = Slot::new(
            path,
            &vault
                .authority
                .mode_file
                .with_file_name("google-login-stash.json"),
            "google",
            source::MARKER,
        )
        .map_err(|e| e.to_string())?;
        match input["input"]["mode"].as_str() {
            Some("custody") => {
                // `enter` below moves Pi's own login into the custody stash, so the
                // admission decision is made against the custody that swap produces
                // before anything is written: a refusal must not migrate a login.
                let mut transition = files.clone();
                transition.mode = Some(json!({"version":1,"mode":"custody"}));
                if transition.stash.is_none() {
                    // The stash `enter` writes for a slot that has none.
                    transition.stash = Some(json!({"version":1,"provider":"google"}));
                }
                let produced = Custody {
                    slot: SlotState::Placeholder,
                    stash: Stash::Valid,
                };
                if let Admission::Refuse(refusal) = crate::vault::import_guard::admit(
                    &transition,
                    &produced,
                ) {
                    return Err(refusal.notice.into());
                }
                slot.enter().map_err(|e| e.to_string())?;
                // Validate the real post-swap slot, not a fabricated placeholder.
                files.mode = Some(json!({"version":1,"mode":"custody"}));
                files.stash = vault
                    .authority
                    .store
                    .authority_files()
                    .map_err(|e| e.to_string())?
                    .stash;
                let custody = crate::vault::import_guard::inspect(
                    path,
                    &stash_path,
                    "google",
                    source::MARKER,
                )
                .map_err(|e| e.to_string())?;
                if let Admission::Refuse(refusal) =
                    crate::vault::import_guard::admit(&files, &custody)
                {
                    // Only concurrent drift can disagree with the custody above;
                    // undo the swap so the refusal leaves no migrated login behind.
                    return match rollback(&slot) {
                        Ok(()) => Err(refusal.notice.into()),
                        Err(error) => Err(format!("{} ({error})", refusal.notice)),
                    };
                }
                Ok(Value::Null)
            }
            Some("local") => slot.exit().map(Value::from).map_err(|e| e.to_string()),
            _ => Err("Invalid Antigravity credential transition".into()),
        }
    }
    pub async fn assert_local(&self) -> Result<(), String> {
        self.vault()?.authority.assert_local().await
    }
    pub async fn close(&self) {
        if let Some(vault) = self.vault.get() {
            vault.close().await;
        }
    }
}

/// Undo a custody swap whose post-transition admission refused.
fn rollback(slot: &Slot) -> Result<(), String> {
    slot.exit().map(|_| ()).map_err(|error| error.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::vault::import_guard::{FRESH_ENROLLMENT_NOTICE, RESTORE_NOTICE};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    const AUTH: &str = r#"{"google":{"access":"access","refresh":"refresh","expires":1729000000000,"type":"oauth"}}"#;
    const MODE: &str = r#"{"version":1,"mode":"local"}"#;
    const STATE: &str = r#"{"version":1,"phase":"approved","proposedName":"antigravity-auth-pi","approvedName":"antigravity-auth-pi","tokenGeneration":2,"updatedAt":1729000000000}"#;
    const TOKEN: &str = r#"{"token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","token_generation":2}"#;
    const STASH: &str = "google-login-stash.json";

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn private(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn write(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        private(path, 0o600);
    }
    fn read(path: &Path) -> Option<(String, u32)> {
        let bytes = fs::read(path).ok()?;
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        Some((String::from_utf8(bytes).unwrap(), mode))
    }
    /// Every file in the vault directory with its bytes and mode: a refusal must
    /// not create, rewrite or remove anything, transient staging included.
    fn tree(directory: &Path) -> Vec<(String, String, u32)> {
        let mut files = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| {
                let (text, mode) = read(&path).unwrap();
                (path.to_string_lossy().into_owned(), text, mode)
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }
    struct Vault {
        _scratch: Scratch,
        auth: PathBuf,
        directory: PathBuf,
        state: State,
    }
    fn vault(name: &str) -> Vault {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("antigravity-vault-{name}-{}", uuid::Uuid::new_v4())),
        );
        fs::create_dir(&scratch.0).unwrap();
        private(&scratch.0, 0o700);
        let agent = scratch.0.join("agent");
        let directory = agent.join("antigravity-auth-vault");
        fs::create_dir_all(&directory).unwrap();
        private(&agent, 0o700);
        private(&directory, 0o700);
        let auth = scratch.0.join("auth.json");
        write(&auth, AUTH);
        let state = State::default();
        state.initialize(&agent).unwrap();
        Vault {
            _scratch: scratch,
            auth,
            directory,
            state,
        }
    }
    impl Vault {
        fn record(&self, name: &str, contents: &str) {
            write(&self.directory.join(name), contents);
        }
        fn enrolled(&self) {
            self.record("pi-mode.json", MODE);
            self.record("pi-enrollment-state.json", STATE);
            self.record("pi-enrollment.json", TOKEN);
        }
        fn custody(&self) -> Result<Value, String> {
            let input = json!({"expiryUnit":"seconds","authPath":self.auth.to_string_lossy(),"input":{"mode":"custody"}});
            self.state.transaction(&input)
        }
        fn local(&self) -> Result<Value, String> {
            let input = json!({"expiryUnit":"seconds","authPath":self.auth.to_string_lossy(),"input":{"mode":"local"}});
            self.state.transaction(&input)
        }
        /// The host login is the real one, byte for byte, and the vault directory
        /// (stash included) is exactly what it was before the refusal.
        fn assert_untouched(&self, before: &[(String, String, u32)]) {
            assert_eq!(read(&self.auth), Some((AUTH.to_string(), 0o600)));
            assert_eq!(&tree(&self.directory), before);
        }
    }

    #[test]
    fn custody_refusals_never_migrate_the_host_login() {
        let token = r#"{"token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","token_generation":0}"#;
        for (name, record, contents, notice) in [
            ("missing-enrollment", None, None, FRESH_ENROLLMENT_NOTICE),
            (
                "invalid-enrollment",
                Some("pi-enrollment.json"),
                Some(r#"{"token":"short","token_generation":2}"#),
                FRESH_ENROLLMENT_NOTICE,
            ),
            (
                "obsolete-enrollment",
                Some("pi-enrollment.json"),
                Some(token),
                FRESH_ENROLLMENT_NOTICE,
            ),
            // Imported custody records are refused by the record itself, before
            // `Slot::enter` can stash the login.
            (
                "mismatched-custody",
                Some(STASH),
                Some(r#"{"version":1,"provider":"openai"}"#),
                RESTORE_NOTICE,
            ),
            (
                "obsolete-custody-record",
                Some(STASH),
                Some(r#"{"version":2,"provider":"google"}"#),
                FRESH_ENROLLMENT_NOTICE,
            ),
        ] {
            let vault = vault(name);
            vault.record("pi-mode.json", r#"{"version":1,"mode":"custody"}"#);
            vault.record("pi-enrollment-state.json", STATE);
            if let (Some(record), Some(contents)) = (record, contents) {
                vault.record(record, contents);
            }
            let before = tree(&vault.directory);
            assert_eq!(vault.custody().unwrap_err(), notice, "{name}");
            vault.assert_untouched(&before);
        }
    }

    #[test]
    fn custody_transition_swaps_and_restores_the_host_login() {
        let vault = vault("transition");
        vault.enrolled();
        assert_eq!(vault.custody().unwrap(), Value::Null);
        let swapped = read(&vault.auth).unwrap().0;
        assert_ne!(swapped, AUTH);
        assert!(read(&vault.directory.join(STASH)).is_some());
        assert_eq!(vault.local().unwrap(), json!("restored"));
        assert_eq!(read(&vault.auth), Some((AUTH.to_string(), 0o600)));
        assert_eq!(read(&vault.directory.join(STASH)), None);
    }

    #[test]
    fn rollback_restores_the_login_and_drops_the_stash() {
        let vault = vault("rollback");
        let stash = vault.directory.join(STASH);
        let slot = Slot::new(&vault.auth, &stash, "google", source::MARKER).unwrap();
        slot.enter().unwrap();
        assert_ne!(read(&vault.auth).unwrap().0, AUTH);
        assert!(read(&stash).is_some());
        rollback(&slot).unwrap();
        assert_eq!(read(&vault.auth), Some((AUTH.to_string(), 0o600)));
        assert_eq!(read(&stash), None);
    }
}
