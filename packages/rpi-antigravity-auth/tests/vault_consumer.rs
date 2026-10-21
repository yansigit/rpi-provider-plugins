#[path = "common/vault.rs"]
mod support;
use rpi_antigravity_auth::vault::{
    consumer::Consumer, custody::Receipt, file_store::FileStore, roster_store::RosterStore,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use support::{flush, outcome, save, Data, Fixture, Runtime, Scratch};
use tokio_util::sync::CancellationToken;
type Attempts = Arc<Mutex<HashMap<String, Arc<Receipt>>>>;
async fn invoke(
    consumer: Arc<Consumer<Runtime>>,
    step: Value,
    signal: CancellationToken,
    attempts: Attempts,
) -> Value {
    let method = step["method"]
        .as_str()
        .or_else(|| step["op"].as_str())
        .unwrap();
    let route = step["route"]
        .as_str()
        .unwrap_or("vault:id~9af211329b")
        .to_owned();
    outcome(match method {
        "refresh" => consumer.refresh().await.map(|v| v.unwrap_or(Value::Null)),
        "authorize" => match consumer.authorize(route, signal).await {
            Ok(attempt) => {
                let value =
                    json!({"metadata":attempt.metadata(),"accessToken":attempt.access_token()});
                attempts.lock().unwrap().insert(
                    step["attempt"].as_str().unwrap_or("attempt").into(),
                    attempt,
                );
                Ok(value)
            }
            Err(e) => Err(e),
        },
        "accept" | "decline" => consumer
            .change_route(route, method == "accept")
            .await
            .map(|_| Value::Null),
        "report" => {
            let attempt =
                attempts.lock().unwrap()[step["attempt"].as_str().unwrap_or("attempt")].clone();
            consumer
                .report_failure(
                    attempt,
                    step["status"].as_u64().unwrap_or(401) as u16,
                    "direct",
                )
                .await
                .map(|_| Value::Null)
        }
        _ => panic!("unknown method"),
    })
}
#[tokio::test]
async fn pi_consumer_authority_shared_work_and_persistence_match_original() {
    tokio::time::timeout(Duration::from_secs(60),async{
        let fixture:Value=serde_json::from_str(include_str!("fixtures/vault-consumer.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap(){
            let parent=std::env::var_os("RPI_PRIVATE_TMP").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);let scratch=Scratch(parent.join(format!("consumer-{}",uuid::Uuid::new_v4())));std::fs::create_dir(&scratch.0).unwrap();#[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&scratch.0,std::fs::Permissions::from_mode(0o700)).unwrap();}
            let files=FileStore::new(scratch.0.join("state.json"),scratch.0.join("token.json")).unwrap();save(&files.token,&fixture["token"].to_string());let store=RosterStore::new(scratch.0.join("roster.json")).unwrap();
            let port=Arc::new(Fixture{data:Arc::new(Mutex::new(Data{now:1000.0,active:json!(true),next_client:0,responses:HashMap::new(),held:HashMap::new(),calls:vec![]})),files,inventory:fixture["inventory"].clone(),receipt:fixture["receipt"].clone()});let consumer=Consumer::new(Arc::new(Runtime(port.clone())),store.clone());
            let mut signals=HashMap::<String,CancellationToken>::new();let mut pending=HashMap::new();let attempts:Attempts=Arc::new(Mutex::new(HashMap::new()));let mut results=vec![];let mut states=vec![];
            for step in case["steps"].as_array().unwrap(){
                let signal=signals.entry(step["signal"].as_str().unwrap_or("signal").into()).or_default().clone();
                match step["op"].as_str().unwrap(){
                    "set"=>{let mut d=port.data.lock().unwrap();if let Some(active)=step.get("active"){d.active=active.clone();}if let Some(now)=step["now"].as_f64(){d.now=now;}for op in ["connect","list","get","report"]{if let Some(values)=step[op].as_array(){d.responses.entry(op.into()).or_default().extend(values.clone());}}},
                    "begin"=>{pending.insert(step["id"].as_str().unwrap().to_owned(),tokio::spawn(invoke(consumer.clone(),step.clone(),signal,attempts.clone())));if let Some(until)=step["until"].as_str(){while !port.data.lock().unwrap().held.contains_key(until){tokio::task::yield_now().await;}}},
                    "join"=>results.push(pending.remove(step["id"].as_str().unwrap()).unwrap().await.unwrap()),
                    "release"=>{port.data.lock().unwrap().held.remove(step["id"].as_str().unwrap()).unwrap().send(()).unwrap();},
                    "cancel"=>signal.cancel(),"close"=>consumer.close().await,
                    "peer"=>{let mut roster:Value=serde_json::from_slice(&std::fs::read(&store.path).unwrap()).unwrap();if step["remove"]==true{std::fs::remove_file(&store.path).unwrap();}else if let Some(raw)=step["raw"].as_str(){save(&store.path,raw);}else{if let Some(fields)=step["fields"].as_object(){for(k,v)in fields{roster["rows"][0][k]=v.clone();}}if step["clear"]==true{roster["rows"]=json!([]);}if let Some(view)=step.get("view"){roster["view"]=view.clone();}save(&store.path,&format!("{}\n",roster));}},
                    "busy"=>{let lock=PathBuf::from(format!("{}.claustrum-roster.lock",store.path.display()));save(&lock,&json!({"ownerId":"peer","expiresAt":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64+60000}).to_string());},
                    "token"=>{if step["remove"]==true{std::fs::remove_file(&port.files.token).unwrap();}else{save(&port.files.token,&step["value"].to_string());}},
                    _=>results.push(invoke(consumer.clone(),step.clone(),signal,attempts.clone()).await),
                }
                flush().await;states.push(if step["op"]=="release"||step["op"]=="begin"&&step.get("until").is_none(){Value::Null}else{json!({"snapshot":consumer.snapshot(),"reach":consumer.grant_reach()})});
            }
            consumer.close().await;flush().await;assert!(pending.is_empty());
            assert_eq!(json!(results),case["results"],"{} results",case["name"]);assert_eq!(json!(states),case["states"],"{} states",case["name"]);assert_eq!(json!(port.data.lock().unwrap().calls),case["calls"],"{} calls",case["name"]);assert_eq!(json!(std::fs::read_to_string(&store.path).ok()),case["after"],"{} committed bytes",case["name"]);
        }
    }).await.expect("consumer acceptance timed out");
}
