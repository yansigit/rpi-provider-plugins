#[path = "common/vault.rs"]
mod support;
use rpi_antigravity_auth::vault::{
    file_store::FileStore,
    roster_store::RosterStore,
    send::{self, Admission, Dispatch, Error, Source},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{flush, outcome, save, Data, Fixture, Runtime, Scratch};
use tokio_util::sync::CancellationToken;
fn result(value: Result<Value, Error>) -> Value {
    match value {
        Ok(value) => json!({"value":value}),
        Err(Error::Custody(e)) => outcome(Err(e)),
        Err(e) => {
            let error = match &e {
                Error::Source { kind, .. } => {
                    json!({"name":"AntigravityVaultSourceError","kind":kind,"message":e.to_string()})
                }
                Error::Cancelled => json!({"name":"AbortError","message":e.to_string(),"code":20}),
                _ => json!({"name":"Error","message":e.to_string()}),
            };
            json!({"error":error})
        }
    }
}
impl send::Port for Runtime {
    fn log(&self, message: &'static str, fields: Value) {
        self.0
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["log", "debug", message, fields]));
    }
    fn on_error(&self, error: &Error) {
        self.0
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["error", result(Err(error.clone()))["error"]]));
    }
}
struct Response {
    status: u16,
    id: usize,
    spec: Value,
}
struct Dispatcher {
    fixture: Arc<Fixture>,
    source: Arc<Source<Runtime>>,
    specs: Vec<Value>,
    index: usize,
    signal: CancellationToken,
}
impl Dispatch for Dispatcher {
    type Response = Response;
    fn status(response: &Response) -> u16 {
        response.status
    }
    async fn send(
        &mut self,
        admission: &Admission,
        _: CancellationToken,
    ) -> Result<Response, Error> {
        let spec = self
            .specs
            .get(self.index)
            .cloned()
            .unwrap_or(json!({"status":200}));
        self.index += 1;
        self.fixture.data.lock().unwrap().calls.push(json!([
            "dispatch",
            admission.metadata(),
            admission.access_token()
        ]));
        if spec["abort"] == true {
            self.signal.cancel();
        }
        if spec["close"] == true {
            self.source.close().await;
        }
        if let Some(error) = spec["error"].as_str() {
            return Err(Error::Dispatch(error.into()));
        }
        Ok(Response {
            status: spec["status"].as_u64().unwrap() as u16,
            id: self.index,
            spec,
        })
    }
    async fn cancel(&mut self, response: Response) -> Result<(), Error> {
        self.fixture
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["cancel-body", response.id]));
        if response.spec["cancelAbort"] == true {
            self.signal.cancel();
        }
        if let Some(error) = response.spec["cancelError"].as_str() {
            return Err(Error::Dispatch(error.into()));
        }
        Ok(())
    }
}
#[test]
fn source_admission_and_route_validation_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-send.json")).unwrap();
    for case in fixture["validations"].as_array().unwrap() {
        assert_eq!(
            result(send::admission_metadata(
                &fixture["referenceRoute"],
                &case["receipt"],
                case["receipt"]["accessToken"].as_str().unwrap_or("")
            )),
            case["result"],
            "{}",
            case["receipt"]
        );
    }
    for case in fixture["routeCases"].as_array().unwrap() {
        assert_eq!(
            json!(send::routes(Some(&case["roster"]))),
            case["result"],
            "{}",
            case["roster"]
        );
    }
    assert!(send::routes(None).is_empty());
}
#[tokio::test]
async fn physical_send_retry_and_reporting_traces_match_original() {
    tokio::time::timeout(Duration::from_secs(60),async{
  let fixture:Value=serde_json::from_str(include_str!("fixtures/vault-send.json")).unwrap();
  for case in fixture["cases"].as_array().unwrap(){
   let parent=std::env::var_os("RPI_PRIVATE_TMP").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);let scratch=Scratch(parent.join(format!("send-{}",uuid::Uuid::new_v4())));std::fs::create_dir(&scratch.0).unwrap();#[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&scratch.0,std::fs::Permissions::from_mode(0o700)).unwrap();}
   let files=FileStore::new(scratch.0.join("state.json"),scratch.0.join("token.json")).unwrap();save(&files.token,&fixture["token"].to_string());let store=RosterStore::new(scratch.0.join("roster.json")).unwrap();
   let port=Arc::new(Fixture{data:Arc::new(Mutex::new(Data{now:1000.0,active:json!(true),next_client:0,responses:HashMap::new(),held:HashMap::new(),calls:vec![]})),files,inventory:fixture["inventory"].clone(),receipt:fixture["receipt"].clone()});let runtime=Arc::new(Runtime(port.clone()));let source=Source::new(runtime.clone(),store.clone());let foreign=Source::new(runtime,store.clone());
   let signal=CancellationToken::new();let mut admissions=HashMap::<String,Arc<Admission>>::new();let mut results=vec![];let mut states=vec![];
   for step in case["steps"].as_array().unwrap(){
    let reference=step.get("reference").unwrap_or(&fixture["referenceRoute"]);let id=step["id"].as_str().unwrap_or("admission");
    let value=match step["op"].as_str().unwrap(){
     "set"=>{let mut d=port.data.lock().unwrap();if let Some(active)=step.get("active"){d.active=active.clone();}for op in ["list","get","report"]{if let Some(values)=step[op].as_array(){d.responses.entry(op.into()).or_default().extend(values.clone());}}None},
     "refresh"=>Some(source.refresh().await.map(|v|v.unwrap_or(Value::Null))),
     "admit"=>Some(match source.admit(reference,&signal).await{Ok(admission)=>{let value=json!({"metadata":admission.metadata(),"accessToken":admission.access_token()});admissions.insert(id.into(),admission);Ok(value)},Err(e)=>Err(e)}),
     "report"=>Some(if step["foreign"]==true{&foreign}else{&source}.report_status(admissions[id].clone(),step["status"].as_u64().unwrap_or(401) as u16,"direct").await.map(|v|json!(v))),
     "accept"|"decline"=>Some(source.change_route(reference,step["op"]=="accept").await.map(|_|Value::Null)),
     "cancel"=>{signal.cancel();None},"close"=>{source.close().await;None},
     "send"=>{let mut dispatch=Dispatcher{fixture:port.clone(),source:source.clone(),specs:step["responses"].as_array().unwrap().clone(),index:0,signal:signal.clone()};Some(source.send(reference,&mut dispatch,"model",&signal,"direct").await.map(|r|json!({"status":r.status,"id":r.id})))},
     _=>panic!("unknown step"),
    };
    if let Some(value)=value{results.push(result(value));}flush().await;states.push(json!({"routes":source.routes(),"reach":source.grant_reach(),"cause":source.empty_roster_cause()}));
   }
   source.close().await;foreign.close().await;flush().await;
   assert_eq!(json!(results),case["results"],"{} results",case["name"]);assert_eq!(json!(states),case["states"],"{} states",case["name"]);assert_eq!(json!(port.data.lock().unwrap().calls),case["calls"],"{} calls",case["name"]);assert_eq!(json!(std::fs::read_to_string(&store.path).ok()),case["after"],"{} bytes",case["name"]);
  }
 }).await.expect("send acceptance timed out");
}
