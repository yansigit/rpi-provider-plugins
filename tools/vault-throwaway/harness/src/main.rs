//! Throwaway driver for the vault dry run (tools/vault-throwaway/dry-run.sh).
//!
//! It calls rpi's own Antigravity vault client (`rpi_antigravity_auth::vault`) against a
//! local CortexKit vault, through the same public entry points the menu and request
//! paths use:
//!
//!   enroll  -> `setup::Setup::connect_step`  (propose, then poll after approval)
//!   status  -> `setup::Setup::status`
//!   use     -> `runtime::Vault::list` + `setup::Setup::use_vault` ("Use this account")
//!   fetch   -> `runtime::Vault::send` with a capturing dispatcher (one custody admission)
//!
//! Fake data only. Nothing here prints a credential: `fetch` prints the SHA-256 of the
//! bearer rpi received and whether it equals the fixture token file it is given.
//!
//! SUBC_CONNECTION_FILE must name the throwaway daemon's connection file; rpi's own
//! discovery reads it from the environment.
use rpi_antigravity_auth::vault::{
    ceremony::Failure,
    connection::{Enrolled, SubcConnector},
    policy::ConsumerError,
    runtime::{connector, Vault},
    send::{Admission, Dispatch, Error as SendError},
    setup::{self, Setup},
    source::Source,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: vault-check <enroll|status|use|fetch> <vault-dir> [route-id | fixture-token-file]";

/// rpi's enrollment port for a host process: the real subc connector, OS randomness for
/// the request secret, and the wall clock.
struct Port;

impl setup::Port for Port {
    type Client = Arc<Enrolled<SubcConnector>>;

    async fn connect(&self) -> Result<Self::Client, Failure> {
        let connector = connector().map_err(Failure::Storage)?;
        Enrolled::connect(connector).await.map_err(|_| {
            Failure::Consumer(ConsumerError::new(
                "unreachable",
                "Claustrum is not reachable through the connection file",
            ))
        })
    }

    async fn close(&self, client: Self::Client) {
        client.close().await;
    }

    async fn host_slot(&self) -> Result<String, Failure> {
        Ok("empty".into())
    }

    async fn changed(&self, _mode: Value) -> Result<(), setup::Error> {
        Ok(())
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn mint_secret(&self) -> Result<String, Failure> {
        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
            .map_err(|e| Failure::Storage(format!("reading OS randomness: {e}")))?;
        Ok(hex(&bytes))
    }
}

/// Records the admission rpi built for one request, then answers 200 without any network.
#[derive(Default)]
struct Capture {
    token: Option<String>,
    metadata: Option<Value>,
}

impl Dispatch for Capture {
    type Response = u16;

    fn status(response: &u16) -> u16 {
        *response
    }

    async fn send(&mut self, admission: &Admission, _signal: CancellationToken) -> Result<u16, SendError> {
        self.token = Some(admission.access_token().to_owned());
        self.metadata = Some(admission.metadata().clone());
        Ok(200)
    }

    async fn cancel(&mut self, _response: u16) -> Result<(), SendError> {
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fail<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

async fn run() -> Result<Value, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, dir) = match args.as_slice() {
        [command, dir, ..] => (command.as_str(), PathBuf::from(dir)),
        _ => return Err(USAGE.into()),
    };
    let setup = Setup {
        source: Source::new(&dir).map_err(fail)?,
        port: Arc::new(Port),
    };
    match command {
        "enroll" => Ok(json!({"status": setup.connect_step().await.map_err(fail)?})),
        "status" => Ok(json!({"status": setup.status().await.map_err(fail)?})),
        "use" => {
            let vault = Vault::new(&dir)?;
            let choices = vault.list().await?;
            let route = match args.get(2) {
                Some(route) => route.clone(),
                None => match choices.as_slice() {
                    [only] => only["routeId"]
                        .as_str()
                        .ok_or("the listed account has no routeId")?
                        .to_owned(),
                    _ => return Err("several vault accounts are listed; pass a route id".into()),
                },
            };
            let mode = setup.use_vault(Some(&route)).await.map_err(fail)?;
            let listed: Vec<Value> = choices
                .iter()
                .map(|c| json!({"routeId": c["routeId"], "declined": c["declined"]}))
                .collect();
            Ok(json!({"selectedRouteId": route, "listed": listed, "mode": mode}))
        }
        "fetch" => {
            let fixture = args.get(2).ok_or("fetch needs the fixture token file")?;
            let expected = std::fs::read_to_string(fixture)
                .map_err(|e| format!("reading fixture token file: {e}"))?;
            let expected = expected.trim_end();
            let vault = Vault::new(&dir)?;
            let mut capture = Capture::default();
            let status = vault.send(&mut capture, &CancellationToken::new()).await?;
            let token = capture
                .token
                .take()
                .ok_or("the request never reached custody admission")?;
            let matches = token == expected;
            let digest = hex(&Sha256::digest(token.as_bytes()));
            let value = json!({
                "responseStatus": status,
                "bearerDelivered": true,
                "matchesFixture": matches,
                "bearerSha256": digest,
                "admission": capture.metadata,
            });
            if !matches {
                return Err(format!("custody delivered a different bearer: {value}"));
            }
            Ok(value)
        }
        other => Err(format!("unknown command {other}; {USAGE}")),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(value) => println!("{value}"),
        Err(message) => {
            eprintln!("vault-check: {message}");
            std::process::exit(1);
        }
    }
}
