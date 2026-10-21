//! Vault credentials enter only the physical dispatch, after request preparation.
use super::{runtime::Vault, send, source::MARKER};
use crate::{
    driver::{self, Deadline, Response, Secrets},
    local::{Port, Shared},
    metadata::SessionStore,
    request::{self, PreparedRequest},
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub struct Stream<'a, P> {
    pub shared: &'a Shared,
    pub port: Arc<P>,
    pub vault: &'a Vault,
    pub refusal: Option<&'a str>,
}
struct Dispatch<'a, P> {
    port: &'a P,
    prepared: PreparedRequest,
    deadline: &'a Deadline,
    secrets: &'a mut Secrets,
}
impl<P: Port> send::Dispatch for Dispatch<'_, P> {
    type Response = Response<P::StreamBody>;
    fn status(response: &Self::Response) -> u16 {
        response.status
    }
    async fn send(
        &mut self,
        admission: &send::Admission,
        signal: CancellationToken,
    ) -> Result<Self::Response, send::Error> {
        let access = admission.access_token();
        let project = admission.metadata()["projectId"].as_str().unwrap();
        self.secrets.credential(access);
        self.secrets.project(project);
        if signal.is_cancelled() {
            self.deadline.signal.cancel();
        }
        self.deadline
            .wait(self.port.dispatch(
                self.prepared.clone(),
                access,
                project,
                self.deadline.signal.clone(),
            ))
            .await
            .map_err(send::Error::Dispatch)
    }
    async fn cancel(&mut self, response: Self::Response) -> Result<(), send::Error> {
        drop(response);
        Ok(())
    }
}
impl<P: Port> driver::Source for Stream<'_, P> {
    type Body = P::StreamBody;
    fn now_ms(&self) -> i64 {
        self.port.now_ms() as i64
    }
    fn next_id(&self) -> String {
        self.port.next_id()
    }
    fn sessions(&self) -> &Mutex<SessionStore> {
        &self.shared.sessions
    }
    async fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        deadline: &Deadline,
        secrets: &mut Secrets,
    ) -> Result<Response<Self::Body>, String> {
        if options["apiKey"] != MARKER {
            return Err("The native Google credential slot is not synchronized for vault mode. No request was sent.".into());
        }
        let prepared = request::prepare(
            model,
            context,
            options,
            &mut self.shared.sessions.lock().unwrap(),
            self.now_ms(),
            &mut || self.next_id(),
        )?;
        if let Some(reason) = self.refusal {
            return Err(format!("{reason} Resolve it from /antigravity; Antigravity requests are refused until then. No request was sent."));
        }
        let mut dispatch = Dispatch {
            port: self.port.as_ref(),
            prepared,
            deadline,
            secrets,
        };
        deadline
            .wait(self.vault.send(&mut dispatch, &deadline.signal))
            .await
    }
}
