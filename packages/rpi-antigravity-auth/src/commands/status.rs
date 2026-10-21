use crate::vault::{
    path::{self, GRANT_COMMAND},
    source::PROPOSED_NAME,
};
use serde_json::Value;
fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("undefined")
}
pub fn enrollment(status: &Value) -> String {
    match text(&status["state"]) {
        "idle"=>"Vault access: not requested yet.".into(),
        "pending"=>if crate::json::truthy(&status["retryCode"]) {format!("Vault access: the vault can't take Pi's request right now; Pi asks again the next time it checks (reference: {}).",text(&status["retryCode"]))}else{"Vault access: waiting for the vault operator to approve this request.".into()},
        "approved"=>"Vault access: approved by the vault operator.".into(),
        "denied"=>"Vault access: the vault operator denied the request. Choose Request vault access to ask again.".into(),
        "blocked"=>match text(&status["code"]) {
            "pending_exists"=>"Vault access: the vault turned down a new request because an earlier request from Pi is still waiting.".into(),
            "missing_token"=>"Vault access: approved, but Pi's vault key is missing from this computer. Disconnect from the vault, then request vault access again.".into(),
            "unreadable-state"=>"Vault access: Pi can't read its record of the vault request. Disconnect from the vault, then request vault access again.".into(),
            code=>format!("Vault access: the vault ended this request (reference: {code}). Choose Request vault access to start a new one."),
        },
        "unavailable"=>format!("Vault access: Pi can't reach the vault right now. Try again later (reference: {}).",text(&status["code"])),
        "busy"=>"Vault access: another vault setup step is running. Try again in a moment.".into(),
        _=>String::new(),
    }
}
pub fn guidance(status: &Value) -> Vec<String> {
    if status["state"] == "pending" {
        vec![
            "Next, the vault operator runs these outside Pi:".into(),
            status.get("requestId").map(|id| format!("1. Approve the request: ck auth enroll approve --request-id {}",text(id))).unwrap_or_else(||format!("1. Find the request from {PROPOSED_NAME} with ck auth enroll list, then approve it: ck auth enroll approve --request-id <id>")),
            format!("2. Grant read access: {GRANT_COMMAND}"),
            "Both are needed: without the grant, Pi can see no accounts in the vault.".into(),
            "3. Pi switches to the vault once it sees the approval. Choose Request vault access to check again.".into(),
        ]
    } else if status["state"] == "blocked" && status["code"] == "pending_exists" {
        vec![
            "The earlier request may come from a vault setup that was disconnected here before the operator answered it.".into(),
            "It expires 15 minutes after it was made; request vault access again after that.".into(),
            format!("Or the vault operator clears it now: find the request from {PROPOSED_NAME} with ck auth enroll list, run ck auth enroll deny --request-id <id>, then request vault access again here."),
        ]
    } else {
        vec![]
    }
}
pub fn source(status: &Value) -> String {
    match text(&status["mode"]) {
        "custody"=>"Using the vault for Antigravity accounts; Pi's own login is set aside.".into(),
        "local"=>match text(&status["hostSlot"]) {
            "login"=>"Using Pi's own login (the account you signed in with /login), not the vault.".into(),
            "dormant"=>"Using Pi's own login, not the vault.".into(),
            _=>"Using Pi's own login, not the vault, but you are not signed in. Run /login to sign in.".into(),
        },
        _=>match status.get("modeProblem") {
            None=>"Pi can't tell whether to use the vault or its own login, so it sends no Antigravity requests.".into(),
            Some(reason)=>format!("Pi can't tell whether to use the vault or its own login ({}), so it sends no Antigravity requests.",text(reason)),
        }
    }
}
pub fn format(status: &Value) -> Vec<String> {
    let mut lines = vec![source(status), enrollment(&status["enrollment"])];
    if status["mode"] == "custody" && status.get("selectedRouteId").is_some() {
        lines.push("Pi uses the vault account marked \"in use by Pi\" below.".into());
    }
    lines.extend(guidance(&status["enrollment"]));
    lines
}
pub fn empty(reach: Option<&Value>) -> Vec<String> {
    path::empty_guidance(match reach {
        None => "unknown",
        Some(v) if v["grants"].as_f64() == Some(0.0) => "no-grant",
        _ => "grant-reaches-nothing",
    })
}
