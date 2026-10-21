use super::schema;
use crate::services::{iso_date, Error};
use crate::types::trim;
use serde_json::{json, Map, Value};

fn error(message: impl Into<String>) -> Error {
    Error::Message(format!("multiprovider: {}", message.into()))
}
fn text<'a>(request: &'a Value, field: &str) -> Result<&'a str, Error> {
    request
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| error(format!("missing {field}")))
}
fn property<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(key),
        Value::Array(array) => index(key).and_then(|index| array.get(index)),
        _ => None,
    }
}
fn index(key: &str) -> Option<usize> {
    key.parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX && index.to_string() == key)
        .map(|index| index as usize)
}
fn put(value: &mut Value, key: &str, entry: Value) {
    match value {
        Value::Object(map) => {
            map.insert(key.into(), entry);
        }
        Value::Array(array) => {
            if let Some(index) = index(key) {
                array.resize(array.len().max(index + 1), Value::Null);
                array[index] = entry;
            }
        }
        _ => {}
    }
}
fn remove(value: &mut Value, key: &str) {
    match value {
        Value::Object(map) => {
            map.shift_remove(key);
        }
        Value::Array(array) => {
            if let Some(entry) = index(key).and_then(|index| array.get_mut(index)) {
                *entry = Value::Null;
            }
        }
        _ => {}
    }
}
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().is_some_and(|n| n != 0.0),
        _ => true,
    }
}
fn account_public(account: &Value) -> Value {
    let mut value = account.clone();
    remove(&mut value, "credential");
    value
}
fn pool_public(id: &str, pool: &Value) -> Value {
    let mut result = json!({"providerId":id});
    for key in ["policy", "affinity", "includeUpstream", "upstream"] {
        if let Some(value) = pool.get(key) {
            result[key] = value.clone();
        }
    }
    result["accounts"] = json!(pool["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(account_public)
        .collect::<Vec<_>>());
    result
}
fn label(value: Option<&Value>) -> Result<String, Error> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| error("account label is required"))?;
    let value = trim(value);
    if value.is_empty() {
        return Err(error("account label is required"));
    }
    Ok(value.into())
}
pub fn writable(request: &Value) -> bool {
    matches!(
        request["op"].as_str(),
        Some(
            "add"
                | "replace"
                | "account"
                | "setPool"
                | "remove"
                | "setScheduler"
                | "saveVirtual"
                | "removeVirtual"
        )
    )
}
pub fn prepare(request: &mut Value) -> Result<(), Error> {
    let op = text(request, "op")?;
    if !matches!(
        op,
        "add"
            | "replace"
            | "account"
            | "setPool"
            | "pool"
            | "has"
            | "list"
            | "remove"
            | "getScheduler"
            | "setScheduler"
            | "saveVirtual"
            | "getVirtual"
            | "listVirtual"
            | "removeVirtual"
    ) {
        return Err(error("unknown account store operation"));
    }
    if matches!(op, "add" | "replace" | "setPool") {
        schema::safe_key(text(request, "provider")?, "provider id")?;
    }
    if op == "add" {
        label(request["input"].get("label"))?;
        schema::credential(&request["input"]["credential"])?;
    }
    if op == "replace" {
        schema::credential(&request["credential"])?;
    }
    if matches!(op, "getVirtual" | "removeVirtual") {
        schema::safe_key(text(request, "id")?, "virtual provider id")?;
    }
    if op == "saveVirtual" {
        request["input"] = schema::normalize_virtual(&request["input"])?;
    }
    Ok(())
}
fn stamp(now: &dyn Fn() -> f64) -> Result<String, Error> {
    let at = now();
    if !at.is_finite() || at.abs() > 8_640_000_000_000_000.0 {
        return Err(Error::InvalidTime);
    }
    Ok(iso_date(at))
}
fn upstream(pool: &mut Value, input: &Value, merge: bool) -> Result<(), Error> {
    if let Some(upstream) = input.get("upstream") {
        let upstream = if merge {
            let mut combined = pool.get("upstream").cloned().unwrap_or_else(|| json!({}));
            for (key, value) in schema::entries(upstream) {
                put(&mut combined, &key, value.clone());
            }
            schema::normalize_upstream(&combined)?
        } else {
            schema::normalize_upstream(upstream)?
        };
        if upstream.as_object().unwrap().is_empty() {
            remove(pool, "upstream");
        } else {
            pool["upstream"] = upstream;
        }
    }
    Ok(())
}
pub fn execute(
    state: &mut Value,
    request: &Value,
    now: &dyn Fn() -> f64,
    random_id: &dyn Fn() -> String,
) -> Result<Value, Error> {
    let op = text(request, "op")?;
    let provider = request["provider"].as_str().unwrap_or("");
    let input = &request["input"];
    match op {
        "list" => {
            let mut ids: Vec<_> = schema::entries(&state["providers"])
                .into_iter()
                .filter(|(_, pool)| !pool["accounts"].as_array().unwrap().is_empty())
                .map(|(id, _)| id)
                .collect();
            ids.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            Ok(json!(ids))
        }
        "pool" => Ok(property(&state["providers"], provider)
            .map(|pool| pool_public(provider, pool))
            .unwrap_or(Value::Null)),
        "has" => Ok(json!(property(&state["providers"], provider).is_some_and(
            |pool| pool["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|account| truthy(&account["enabled"]))
        ))),
        "add" => {
            let mut pool = property(&state["providers"], provider).cloned().unwrap_or_else(|| json!({"policy":"round-robin","affinity":true,"includeUpstream":true,"accounts":[]}));
            let settings = &input["pool"];
            for key in ["policy", "affinity", "includeUpstream"] {
                if let Some(value) = settings.get(key) {
                    pool[key] = value.clone();
                }
            }
            upstream(&mut pool, settings, true)?;
            let at = stamp(now)?;
            let account = json!({"id":random_id(),"label":label(input.get("label"))?,"authKind":if input["credential"]["type"] == "oauth" { "oauth" } else { "api-key" },
                "credential":input["credential"],"enabled":input.get("enabled").filter(|value| !value.is_null()).cloned().unwrap_or(json!(true)),
                "weight":schema::weight(input.get("weight")),"priority":schema::priority(input.get("priority"), &json!(pool["accounts"].as_array().unwrap().len()+1)),"createdAt":at,"updatedAt":at});
            pool["accounts"]
                .as_array_mut()
                .unwrap()
                .push(account.clone());
            put(&mut state["providers"], provider, pool);
            Ok(account_public(&account))
        }
        "account" | "replace" => {
            let id = text(request, "account")?;
            let mut pool = property(&state["providers"], provider)
                .cloned()
                .ok_or_else(|| error(format!("unknown stored account \"{id}\"")))?;
            let account = pool["accounts"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|account| account["id"] == id)
                .ok_or_else(|| error(format!("unknown stored account \"{id}\"")))?;
            if op == "replace" {
                account["credential"] = request["credential"].clone();
                account["authKind"] = json!(if request["credential"]["type"] == "oauth" {
                    "oauth"
                } else {
                    "api-key"
                });
            } else {
                if let Some(value) = input.get("label") {
                    account["label"] = json!(label(Some(value))?);
                }
                if let Some(value) = input.get("enabled") {
                    account["enabled"] = value.clone();
                }
                if let Some(value) = input.get("weight") {
                    account["weight"] = schema::weight(Some(value));
                }
                if let Some(value) = input.get("priority") {
                    account["priority"] = schema::priority(Some(value), &account["priority"]);
                }
            }
            account["updatedAt"] = json!(stamp(now)?);
            let result = account_public(account);
            put(&mut state["providers"], provider, pool);
            Ok(result)
        }
        "remove" => {
            let id = text(request, "account")?;
            let Some(mut pool) = property(&state["providers"], provider).cloned() else {
                return Ok(json!(false));
            };
            let accounts = pool["accounts"].as_array_mut().unwrap();
            let Some(index) = accounts.iter().position(|account| account["id"] == id) else {
                return Ok(json!(false));
            };
            accounts.remove(index);
            if accounts.is_empty() {
                remove(&mut state["providers"], provider);
            } else {
                put(&mut state["providers"], provider, pool);
            }
            Ok(json!(true))
        }
        "setPool" => {
            let mut pool = property(&state["providers"], provider)
                .cloned()
                .ok_or_else(|| error(format!("unknown stored pool \"{provider}\"")))?;
            for key in ["policy", "affinity", "includeUpstream"] {
                if let Some(value) = input.get(key) {
                    pool[key] = value.clone();
                }
            }
            upstream(&mut pool, input, false)?;
            let result = pool_public(provider, &pool);
            put(&mut state["providers"], provider, pool);
            Ok(result)
        }
        "getScheduler" => {
            let mut result = Map::new();
            for key in schema::SETTINGS {
                if let Some(value) = state["scheduler"].get(key) {
                    result.insert(key.into(), value.clone());
                }
            }
            Ok(result.into())
        }
        "setScheduler" => {
            let clear = request["clear"].as_array().cloned().unwrap_or_default();
            let mut result = Map::new();
            for key in schema::SETTINGS {
                if clear.iter().any(|value| value == key) {
                    continue;
                }
                if let Some(value) = input.get(key).or_else(|| state["scheduler"].get(key)) {
                    if !value
                        .as_f64()
                        .is_some_and(|value| value.is_finite() && value >= 0.0)
                    {
                        return Err(error(format!(
                            "scheduler setting \"{key}\" must be a non-negative number"
                        )));
                    }
                    result.insert(key.into(), value.clone());
                }
            }
            if result.is_empty() {
                remove(state, "scheduler");
            } else {
                state["scheduler"] = result.clone().into();
            }
            Ok(result.into())
        }
        "saveVirtual" => {
            if state.get("virtuals").is_none() {
                state["virtuals"] = json!({});
            }
            if !state["virtuals"].is_object() && !state["virtuals"].is_array() {
                let (kind, value) = match &state["virtuals"] {
                    Value::String(value) => ("string", value.clone()),
                    Value::Bool(value) => ("boolean", value.to_string()),
                    value => ("number", value.to_string()),
                };
                return Err(Error::Typed(
                    "TypeError",
                    format!(
                        "Cannot create property '{}' on {kind} '{value}'",
                        input["id"].as_str().unwrap()
                    ),
                ));
            }
            put(
                &mut state["virtuals"],
                input["id"].as_str().unwrap(),
                input.clone(),
            );
            Ok(input.clone())
        }
        "getVirtual" => Ok(property(&state["virtuals"], text(request, "id")?)
            .cloned()
            .unwrap_or(Value::Null)),
        "listVirtual" => {
            let mut list: Vec<_> = schema::entries(&state["virtuals"])
                .into_iter()
                .map(|(_, value)| value.clone())
                .collect();
            let collator = rpi_node_collation::collator(&rpi_node_collation::default_locale())
                .map_err(error)?;
            list.sort_by(|a, b| {
                collator.compare(a["id"].as_str().unwrap(), b["id"].as_str().unwrap())
            });
            Ok(list.into())
        }
        "removeVirtual" => {
            let id = text(request, "id")?;
            if property(&state["virtuals"], id).is_none() {
                return Ok(json!(false));
            }
            remove(&mut state["virtuals"], id);
            if schema::entries(&state["virtuals"]).is_empty() {
                remove(state, "virtuals");
            }
            Ok(json!(true))
        }
        _ => Err(error("unknown account store operation")),
    }
}
pub fn ordered(value: &Value) -> Value {
    match value {
        Value::Object(_) => schema::entries(value)
            .into_iter()
            .map(|(key, value)| (key, ordered(value)))
            .collect::<Map<_, _>>()
            .into(),
        Value::Array(array) => array.iter().map(ordered).collect::<Vec<_>>().into(),
        Value::Number(number) if number.is_f64() => {
            let number = number.as_f64().unwrap();
            if number.fract() == 0.0 && number >= i64::MIN as f64 && number < -(i64::MIN as f64) {
                json!(number as i64)
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}
