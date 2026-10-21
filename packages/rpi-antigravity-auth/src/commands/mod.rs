//! The reachable Pi menu model; native rendering consumes only its sealed payload.
pub mod render;
pub mod seam;
mod status;
pub mod watch;
use crate::vault::setup::Error;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc};

pub struct Accounts {
    pub list: Vec<Value>,
    pub reach: Option<Value>,
}
pub trait Port: Send + Sync + 'static {
    fn status(&self) -> impl Future<Output = Result<Value, Error>> + Send;
    fn accounts(&self) -> impl Future<Output = Result<Option<Accounts>, String>> + Send;
    fn use_vault(&self, route: Option<String>) -> impl Future<Output = Result<(), Error>> + Send;
    fn use_local(&self) -> impl Future<Output = Result<(), Error>> + Send;
    fn disconnect(&self) -> impl Future<Output = Result<(), Error>> + Send;
    fn change_route(
        &self,
        route: String,
        accept: bool,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    fn connect_step(&self) -> impl Future<Output = Result<Value, Error>> + Send;
    /// Start at most one watch, retaining the initiating session's notification target.
    fn watch(&self);
}
pub struct Menu<P: Port> {
    pub port: Arc<P>,
    pub salt: [u8; 16],
}
#[derive(Clone)]
enum Work {
    Request,
    UseVault(Option<String>),
    UseLocal,
    Forget,
    Account {
        route: String,
        name: String,
        action: String,
    },
}
struct Section {
    value: Value,
    work: Vec<(Option<String>, String, Work)>,
}
fn strings(value: &mut Value) -> &mut Vec<Value> {
    value.as_array_mut().unwrap()
}
fn section(id: &str, title: &str, lines: Vec<String>) -> Section {
    Section {
        value: json!({"id":id,"slot":if id=="vault"{"extra"}else{id},"title":title,"lines":lines,"items":[],"actions":[]}),
        work: vec![],
    }
}
fn action(id: &str, label: &str) -> Value {
    json!({"id":id,"label":label,"knobs":[]})
}
fn unavailable() -> Value {
    json!({"ok":false,"code":"unavailable","text":"That action is no longer available."})
}
fn success(text: impl Into<String>) -> Value {
    json!({"ok":true,"text":text.into()})
}
fn failed() -> Value {
    json!({"ok":false,"code":"action-failed","text":"That action failed."})
}
fn refusal(error: Error) -> Value {
    match error {
        Error::Refused(text) => json!({"ok":false,"code":"vault-refused","text":text}),
        _ => failed(),
    }
}
fn name(index: usize) -> String {
    format!("Vault account {}", index + 1)
}
fn serving(status: &Value, accounts: &[Value]) -> Option<usize> {
    let mut available = accounts
        .iter()
        .enumerate()
        .filter(|(_, v)| v["declined"] != true);
    if let Some(route) = status.get("selectedRouteId") {
        available
            .find(|(_, v)| &v["routeId"] == route)
            .map(|(i, _)| i)
    } else {
        let only = available.next()?;
        available.next().is_none().then_some(only.0)
    }
}
fn detail(declined: bool, serving: bool) -> &'static str {
    if declined {
        "turned off for Pi on this computer"
    } else if serving {
        "in use by Pi"
    } else {
        "available"
    }
}
impl<P: Port> Menu<P> {
    pub fn item_id(&self, route: &str) -> String {
        let mut hash = Sha256::new();
        hash.update(self.salt);
        hash.update(route.as_bytes());
        format!("vault-{:.16}", format!("{:x}", hash.finalize()))
    }
    async fn custody_accounts(&self, status: &Value) -> Result<Accounts, String> {
        if status["mode"] != "custody" || status["enrollment"]["state"] != "approved" {
            return Ok(Accounts {
                list: vec![],
                reach: None,
            });
        }
        self.port.accounts().await.map(|v| {
            v.unwrap_or(Accounts {
                list: vec![],
                reach: None,
            })
        })
    }
    async fn accounts_section(&self) -> Result<Section, Error> {
        let status = self.port.status().await?;
        if status["mode"] == "unreadable" {
            return Ok(section("accounts","Accounts",vec![match status.get("modeProblem") {
                None=>"Pi can't tell whether to use the vault or its own login, so it sends no Antigravity requests. See the Vault section.".into(),
                Some(v)=>format!("Pi can't tell whether to use the vault or its own login ({}), so it sends no Antigravity requests. Choose one in the Vault section.",v.as_str().unwrap_or("undefined")),
            }]));
        }
        if status["mode"] == "local" {
            return Ok(section("accounts","Accounts",vec!["Pi serves Antigravity from its own login, one account, kept in Pi's auth store.".into(),match status["hostSlot"].as_str() {
                Some("dormant")=>"Pi's own login is set aside while Pi uses the vault.",
                Some("login")=>"Signed in with Pi's own login. Pi's /login replaces it and /logout removes it.",
                Some("placeholder")=>"Not signed in with Pi's own login. Run /login, or choose Use the vault in the Vault section.",
                _=>"Not signed in. Run Pi's /login to sign in.",
            }.into()]));
        }
        let mut result=section("accounts","Accounts",vec!["Pi uses the vault for Antigravity, one account at a time. Choose an account, or turn accounts off and on, in the Vault section.".into()]);
        if status["hostSlot"] == "login" {
            strings(&mut result.value["lines"]).push(json!("Pi is also signed in with its own login, so requests are refused. Run /logout, or switch back to Pi's own login in the Vault section."));
        }
        match self.custody_accounts(&status).await {
            Err(e) => strings(&mut result.value["lines"]).push(json!(format!(
                "Pi can't load the vault accounts right now: {e}"
            ))),
            Ok(accounts) => {
                if accounts.list.is_empty() && status["enrollment"]["state"] == "approved" {
                    strings(&mut result.value["lines"]).extend(
                        status::empty(accounts.reach.as_ref())
                            .into_iter()
                            .map(Value::from),
                    );
                }
                let current = serving(&status, &accounts.list);
                result.value["items"]=accounts.list.iter().enumerate().map(|(i,v)|json!({"id":self.item_id(v["routeId"].as_str().unwrap()),"label":name(i),"detail":detail(v["declined"]==true,current==Some(i)),"actions":[]})).collect();
            }
        }
        Ok(result)
    }
    async fn routing_section(&self) -> Result<Section, Error> {
        let status = self.port.status().await?;
        let lines = if status["mode"] == "unreadable" {
            vec![
                "Pi can't read its vault setup, so requests are refused. See the Vault section."
                    .into(),
            ]
        } else if status["mode"] != "custody" {
            vec!["Every request uses Pi's own login. There is no account rotation.".into()]
        } else {
            let accounts = self.custody_accounts(&status).await;
            match accounts {
                Ok(accounts) if accounts.list.is_empty() && status["enrollment"]["state"]=="approved"=>{
                    let mut lines=vec!["The vault gives Pi no accounts, so requests are refused. There is no account rotation.".into()];lines.extend(status::empty(accounts.reach.as_ref()));lines
                },
                accounts=>vec![match accounts.ok().and_then(|v|serving(&status,&v.list)) {
                    Some(index)=>format!("Every request uses {}, with a fresh token and project from the vault for each send. There is no account rotation.",name(index)),
                    None=>"No single vault account is chosen, so requests are refused. Choose one in the Vault section. There is no account rotation.".into(),
                }],
            }
        };
        Ok(section("routing", "Routing", lines))
    }
    async fn vault_section(&self) -> Result<Section, Error> {
        let status = self.port.status().await?;
        let mut result = section("vault", "Vault", status::format(&status));
        let approved = status["enrollment"]["state"] == "approved";
        if approved {
            match self.port.accounts().await {
                Ok(Some(accounts)) => {
                    for (i, account) in accounts.list.iter().enumerate() {
                        let route = account["routeId"].as_str().unwrap();
                        let id = self.item_id(route);
                        let label = name(i);
                        let declined = account["declined"] == true;
                        let mut actions = if declined {
                            vec![action("accept", "Turn back on for Pi")]
                        } else {
                            vec![
                                action("use", "Use this account"),
                                action("decline", "Turn off for Pi"),
                            ]
                        };
                        if !declined {
                            actions[1]["description"]="The account stays in the vault and in this list, but Pi on this computer never uses it until you turn it back on.".into();
                        }
                        for action in &actions {
                            let action = action["id"].as_str().unwrap().to_owned();
                            result.work.push((
                                Some(id.clone()),
                                action.clone(),
                                Work::Account {
                                    route: route.into(),
                                    name: label.clone(),
                                    action,
                                },
                            ));
                        }
                        strings(&mut result.value["items"]).push(json!({"id":id,"label":label,"detail":detail(declined,status["mode"]=="custody" && status["selectedRouteId"]==account["routeId"]),"actions":actions}));
                    }
                    if accounts.list.is_empty() {
                        strings(&mut result.value["lines"]).extend(
                            status::empty(accounts.reach.as_ref())
                                .into_iter()
                                .map(Value::from),
                        );
                    } else if status["mode"] != "custody" {
                        strings(&mut result.value["lines"]).push(json!(
                            "Next: choose Use the vault to have Pi use these accounts."
                        ));
                    }
                }
                Err(e) => strings(&mut result.value["lines"]).push(json!(format!(
                    "Pi can't load the vault accounts right now: {e}"
                ))),
                Ok(None) => {}
            }
        }
        let mut add = |value: Value, work: Work| {
            result
                .work
                .push((None, value["id"].as_str().unwrap().into(), work));
            strings(&mut result.value["actions"]).push(value);
        };
        if !approved {
            let mut a = action("request-access", "Request vault access");
            a["description"] =
                "Asks the vault operator to let Pi use the vault's Antigravity accounts.".into();
            add(a, Work::Request);
        }
        if approved && status["mode"] != "custody" {
            add(
                action("use-vault", "Use the vault"),
                Work::UseVault(status["selectedRouteId"].as_str().map(str::to_owned)),
            );
        }
        if status["mode"] != "local"
            && matches!(
                status["enrollment"]["state"].as_str(),
                Some("idle" | "pending" | "denied")
            )
        {
            add(action("use-local", "Use Pi's own login"), Work::UseLocal);
        }
        if status["enrollment"]["state"] == "pending" {
            strings(&mut result.value["lines"]).push(json!(format!("You can't disconnect while this request waits for the vault operator. To withdraw it, ask the vault operator to run ck auth enroll deny --request-id {}, then choose Request vault access here so Pi sees the answer.",status["enrollment"]["requestId"].as_str().unwrap_or("<id>"))));
        } else if status["enrollment"]["state"] != "idle" {
            let mut a = action("forget", "Disconnect from the vault");
            a["description"]="Removes Pi's vault key from this computer and goes back to Pi's own login. Using the vault here again needs a new request and a new approval. The earlier approval stays in the vault until the vault operator revokes it.".into();
            a["confirm"] = json!({"message":"Disconnect this computer from the vault? Pi goes back to its own login, and using the vault here again needs a new approval from the vault operator.","irreversible":true});
            add(a, Work::Forget);
        }
        Ok(result)
    }
    async fn sections(&self) -> Vec<Section> {
        let fallback =
            |id, title| section(id, title, vec!["This section could not be loaded.".into()]);
        vec![
            self.accounts_section().await.unwrap_or_else(|_|fallback("accounts","Accounts")),
            section("quota","Quota",vec!["Pi does not record Antigravity quota. A request Google refuses for quota fails with Google's own error.".into()]),
            self.routing_section().await.unwrap_or_else(|_|fallback("routing","Routing")),
            section("limits","Limits",vec!["Pi has no per-account limits for Antigravity: with one account there is nothing to fall back to.".into()]),
            self.vault_section().await.unwrap_or_else(|_|fallback("vault","Vault")),
        ]
    }
    pub async fn open(&self) -> Value {
        let mut value = json!({"command":"antigravity","menu":{"command":"antigravity","title":"Antigravity","sections":self.sections().await.into_iter().map(|v|v.value).collect::<Vec<_>>()}});
        seam::scrub(&mut value);
        value
    }
    pub async fn step_text(&self, step: &Value, suffix: &str) -> String {
        let mut lines = vec![format!("{}{suffix}", status::enrollment(step))];
        lines.extend(status::guidance(step));
        if step["state"] == "approved" {
            if let Ok(Some(accounts)) = self.port.accounts().await {
                if accounts.list.is_empty() {
                    lines.extend(status::empty(accounts.reach.as_ref()));
                }
            }
        }
        lines.join("\n")
    }
    async fn perform(&self, work: Work) -> Value {
        match work {
            Work::Request=>match self.port.connect_step().await {
                Err(e)=>seam::folder_refusal(&e.to_string()).map(|text|json!({"ok":false,"code":"vault-folder-shared","text":text})).unwrap_or_else(failed),
                Ok(step)=>{
                    let watching=matches!(step["state"].as_str(),Some("pending"|"busy"));
                    if watching {self.port.watch();}
                    let text=self.step_text(&step,if watching{" Pi tells you here when the operator decides."}else{""}).await;
                    if watching || step["state"]=="approved" {success(text)}else{json!({"ok":false,"text":text,"code":"refused"})}
                }
            },
            Work::UseVault(route)=>match self.port.use_vault(route).await {Ok(())=>success("Pi now uses the vault for Antigravity."),Err(e)=>refusal(e)},
            Work::UseLocal=>match self.port.use_local().await {Ok(())=>success("Pi now uses its own login for Antigravity. Run /login if you haven't signed in."),Err(_)=>failed()},
            Work::Forget=>match self.port.disconnect().await {Ok(())=>success("Disconnected from the vault. Pi now uses its own login for Antigravity."),Err(_)=>failed()},
            Work::Account{route,name,action}=>if action=="use" {
                match self.port.use_vault(Some(route)).await {Ok(())=>success(format!("Pi now uses {name} for Antigravity.")),Err(e)=>refusal(e)}
            }else{match self.port.change_route(route,action=="accept").await {Ok(())=>success(if action=="accept" {format!("Pi can use {name} again.")}else{format!("Pi on this computer won't use {name} until you turn it back on.")}),Err(_)=>failed()}},
        }
    }
    pub async fn apply(&self, request: &Value) -> Value {
        let sections = if request["command"] == "antigravity" {
            self.sections().await
        } else {
            vec![]
        };
        let selected = sections
            .iter()
            .find(|s| s.value["id"] == request["sectionId"]);
        let work = selected.and_then(|s| {
            let item = request.get("itemId").and_then(Value::as_str);
            // The original menu resolves the first item with this ID, including duplicate routes.
            let actions = if let Some(id) = item {
                s.value["items"]
                    .as_array()?
                    .iter()
                    .find(|v| v["id"] == id)?["actions"]
                    .as_array()?
            } else {
                s.value["actions"].as_array()?
            };
            let action = actions.iter().find(|v| v["id"] == request["actionId"])?;
            let work = s
                .work
                .iter()
                .find(|(i, a, _)| i.as_deref() == item && request["actionId"] == a.as_str())?
                .2
                .clone();
            Some((action, work))
        });
        let mut result = match work {
            None => unavailable(),
            Some((action, _))
                if action.get("confirm").is_some() && request["confirmed"] != true =>
            {
                json!({"ok":false,"code":"needs-confirmation","text":action["confirm"]["message"],"needsConfirmation":true})
            }
            Some((_, work)) => self.perform(work).await,
        };
        result["command"] = "antigravity".into();
        result["menu"] = self.open().await["menu"].clone();
        seam::scrub(&mut result);
        result
    }
}
