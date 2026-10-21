//! Local credential admission and project acquisition, in the original Pi order.
use crate::{
    auth::{self, Admission, CredentialCache},
    catalog,
    driver::{self, Deadline, Response, Secrets},
    metadata::SessionStore,
    project::{ProjectPort, ProjectStore},
    request::{self, PreparedRequest},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub struct Shared {
    pub sessions: Mutex<SessionStore>,
    pub credentials: Mutex<CredentialCache>,
    pub projects: Arc<ProjectStore>,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(SessionStore::new(String::new())),
            credentials: Mutex::new(CredentialCache::default()),
            projects: Arc::new(ProjectStore::default()),
        }
    }
}
pub trait Port: ProjectPort {
    type StreamBody: driver::Body;
    fn next_id(&self) -> String;
    fn dispatch(
        &self,
        prepared: PreparedRequest,
        access: &str,
        project: &str,
        signal: CancellationToken,
    ) -> impl Future<Output = Result<Response<Self::StreamBody>, String>> + Send;
}
pub struct Local<'a, P, A> {
    pub shared: &'a Shared,
    pub port: Arc<P>,
    pub admission: &'a A,
}
impl<P: Port, A: Admission> driver::Source for Local<'_, P, A> {
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
        deadline.wait(self.admission.check()).await?;
        let access = options["apiKey"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or("Missing Antigravity OAuth access token")?;
        if catalog::is_retired(model["id"].as_str().unwrap_or_default()) {
            return Err(catalog::RETIRED_MESSAGE.into());
        }
        let packed = self
            .shared
            .credentials
            .lock()
            .unwrap()
            .get(access)
            .unwrap_or_default()
            .to_owned();
        let parts = auth::parse_refresh(&packed);
        for project in [parts.project_id, parts.managed_project_id]
            .into_iter()
            .flatten()
        {
            secrets.project(&project);
        }
        let project=deadline.wait(self.shared.projects.ensure(self.port.clone(),json!({"type":"oauth","refresh":packed,"access":access,"expires":self.port.now_ms()+60_000.0}))).await?;
        let project = project["effectiveProjectId"].as_str().unwrap_or_default();
        secrets.project(project);
        let prepared = request::prepare(
            model,
            context,
            options,
            &mut self.shared.sessions.lock().unwrap(),
            self.now_ms(),
            &mut || self.next_id(),
        )?;
        deadline.wait(self.admission.check()).await?;
        deadline
            .wait(
                self.port
                    .dispatch(prepared, access, project, deadline.signal.clone()),
            )
            .await
    }
}
