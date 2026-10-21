//! Pi's custody admission and account-list projection, before the physical send policy.
use serde_json::{json, Value};

pub const GRANT_COMMAND: &str = "ck auth grant --principal enrolled:antigravity-auth-pi --selector-kind category --selector antigravity-native --operation read";

pub fn require_custody(read: &Value) -> Result<&Value, String> {
    if read["ok"] != true {
        return Err(format!("Antigravity vault mode is unclear ({}). Choose a mode in the Vault section of the Antigravity menu.", read["reason"].as_str().unwrap_or("undefined")));
    }
    if read["record"]["mode"] != "custody" {
        return Err("Antigravity is not served from the vault. Switch in the Vault section of the Antigravity menu.".into());
    }
    Ok(&read["record"])
}

pub fn empty_guidance(cause: &str) -> Vec<String> {
    match cause {
        "no-grant" => vec![
            "The vault operator approved Pi. The vault has not granted Pi read access.".into(),
            format!("The vault operator grants it with: {GRANT_COMMAND}"),
        ],
        "grant-reaches-nothing" => vec![
            "The vault operator approved Pi. Pi has read access, but it reaches no Antigravity login in the vault.".into(),
            "The vault operator updates the vault's logins with: ck auth reclassify --from-registry".into(),
            "Then they check them with: ck auth status".into(),
        ],
        _ => vec![
            "The vault operator approved Pi, but Pi can't see any Antigravity accounts in the vault yet.".into(),
            format!("If the operator has not granted Pi read access yet, they run: {GRANT_COMMAND}"),
        ],
    }
}

pub fn select_route<'a>(
    routes: &'a [Value],
    route_id: Option<&Value>,
    cause: &str,
) -> Result<&'a Value, String> {
    if let Some(route) = route_id {
        return routes.iter().find(|row| &row["routeId"] == route).ok_or_else(||
            "The chosen vault account is no longer available (declined, removed or changed). Choose another in the Vault section of the Antigravity menu.".into());
    }
    match routes {
        [] => Err(if cause == "unknown" {
            "The vault serves Pi no Antigravity accounts yet.".into()
        } else { empty_guidance(cause).join("\n") }),
        [only] => Ok(only),
        _ => Err("The vault serves Pi several Antigravity accounts. Choose one in the Vault section of the Antigravity menu.".into()),
    }
}

pub fn account_choices(roster: Option<&Value>) -> (Vec<Value>, Vec<Value>) {
    let mut references = vec![];
    let mut choices = vec![];
    if let Some(roster) = roster {
        for row in roster["rows"].as_array().unwrap() {
            if !crate::json::truthy(&row["accountIdentity"]) {
                continue;
            }
            let mut reference = json!({"routeId":row["routeId"],"credentialId":row["credentialId"],"accountIdentity":row["accountIdentity"],"label":row["label"]});
            let mut choice = json!({"routeId":row["routeId"],"label":row["label"]});
            if let Some(email) = row.get("email") {
                reference["email"] = email.clone();
                choice["email"] = email.clone();
            }
            choice["declined"] = roster["declined"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry["credentialId"] == row["credentialId"]
                        && (entry.get("accountIdentity").is_none()
                            || entry["accountIdentity"] == row["accountIdentity"])
                })
                .into();
            references.push(reference);
            choices.push(choice);
        }
    }
    (references, choices)
}
