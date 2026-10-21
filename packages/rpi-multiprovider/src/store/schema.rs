use crate::services::Error;
use crate::types::trim;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const SETTINGS: [&str; 6] = [
    "rateLimitCooldownMs",
    "quotaCooldownMs",
    "authCooldownMs",
    "transientBaseCooldownMs",
    "maxCooldownMs",
    "errorsBeforeSwitch",
];
fn error(message: impl Into<String>) -> Error {
    Error::Message(format!("multiprovider: {}", message.into()))
}
fn object(value: &Value) -> bool {
    value.is_object() || value.is_array()
}
pub fn entries(value: &Value) -> Vec<(String, &Value)> {
    let mut entries = match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| (key.clone(), value))
            .collect(),
        Value::Array(array) => array
            .iter()
            .enumerate()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        _ => Vec::new(),
    };
    let index = |key: &str| {
        key.parse::<u32>()
            .ok()
            .filter(|n| *n != u32::MAX && n.to_string() == key)
    };
    entries.sort_by(|(a, _), (b, _)| match (index(a), index(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    entries
}
pub fn safe_key(value: &str, label: &str) -> Result<(), Error> {
    if trim(value).is_empty() || matches!(value, "__proto__" | "prototype" | "constructor") {
        return Err(error(format!("invalid {label}")));
    }
    Ok(())
}
pub fn credential(value: &Value) -> Result<(), Error> {
    if !object(value) || value.get("type").is_none() {
        return Err(error("malformed stored credential"));
    }
    if !matches!(value["type"].as_str(), Some("api_key" | "oauth")) {
        return Err(error("unsupported stored credential type"));
    }
    Ok(())
}
fn upstream(value: &Value) -> Result<(), Error> {
    let Some(candidate) = value.as_object() else {
        return Err(error("malformed upstream preferences"));
    };
    if candidate
        .get("label")
        .is_some_and(|value| !value.is_string())
    {
        return Err(error("malformed upstream label"));
    }
    for key in ["weight", "priority"] {
        if candidate
            .get(key)
            .is_some_and(|value| !value.as_f64().is_some_and(f64::is_finite))
        {
            return Err(error(format!("malformed upstream {key}")));
        }
    }
    Ok(())
}
pub fn scheduler(value: &Value) -> Result<(), Error> {
    let Some(candidate) = value.as_object() else {
        return Err(error("malformed scheduler settings"));
    };
    for key in SETTINGS {
        if candidate.get(key).is_some_and(|value| {
            !value
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= 0.0)
        }) {
            return Err(error(format!("malformed scheduler setting \"{key}\"")));
        }
    }
    Ok(())
}

pub fn state(value: &Value) -> Result<(), Error> {
    if !object(value) {
        return Err(error("auth store must contain an object"));
    }
    if value.get("version").and_then(Value::as_f64) != Some(1.0) || !object(&value["providers"]) {
        return Err(error("unsupported auth store format"));
    }
    if let Some(value) = value.get("scheduler") {
        scheduler(value)?;
    }
    for (id, pool) in entries(&value["providers"]) {
        safe_key(&id, "provider id")?;
        if !object(pool) {
            return Err(error(format!("malformed pool for \"{id}\"")));
        }
        let Some(accounts) = pool["accounts"].as_array() else {
            return Err(error(format!("malformed accounts for \"{id}\"")));
        };
        if let Some(value) = pool.get("upstream") {
            upstream(value)?;
        }
        for account in accounts {
            if !object(account) {
                return Err(error(format!("malformed account for \"{id}\"")));
            }
            let Some(account_id) = account["id"]
                .as_str()
                .filter(|_| account["label"].is_string())
            else {
                return Err(error(format!("malformed account identity for \"{id}\"")));
            };
            safe_key(account_id, "account id")?;
            credential(&account["credential"])?;
        }
    }
    if let Some(virtuals) = value.get("virtuals") {
        if virtuals.is_null() {
            return Err(Error::Typed(
                "TypeError",
                "Cannot convert undefined or null to object".into(),
            ));
        }
        if virtuals.as_str().is_some_and(|value| !value.is_empty()) {
            return Err(error("malformed virtual provider"));
        }
        for (_, virtual_value) in entries(virtuals) {
            normalize_virtual(virtual_value)?;
        }
    }
    Ok(())
}

fn virtual_id<'a>(value: &'a Value, label: &str) -> Result<&'a str, Error> {
    let Some(value) = value
        .as_str()
        .filter(|value| !trim(value).is_empty() && !value.contains("::"))
    else {
        return Err(error(format!("malformed virtual {label}")));
    };
    safe_key(value, &format!("virtual {label}"))?;
    Ok(value)
}
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value < -(i64::MIN as f64) {
        json!(value as i64)
    } else {
        json!(value)
    }
}
pub fn weight(value: Option<&Value>) -> Value {
    number(
        value
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(f64::floor)
            .unwrap_or(1.0),
    )
}
pub fn priority(value: Option<&Value>, fallback: &Value) -> Value {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| number(value.floor()))
        .unwrap_or_else(|| fallback.clone())
}
fn trim_method<'a>(value: &'a Value, expression: &str) -> Result<&'a str, Error> {
    value.as_str().map(trim).ok_or_else(|| {
        Error::Typed(
            "TypeError",
            if value.is_null() {
                "Cannot read properties of null (reading 'trim')".into()
            } else {
                format!("{expression}.trim is not a function")
            },
        )
    })
}
pub fn normalize_upstream(value: &Value) -> Result<Value, Error> {
    if value.is_null() {
        return Err(Error::Typed(
            "TypeError",
            "Cannot read properties of null (reading 'label')".into(),
        ));
    }
    let mut result = Map::new();
    if let Some(value) = value.get("label") {
        let label = trim_method(value, "input.label")?;
        if !label.is_empty() {
            result.insert("label".into(), json!(label));
        }
    }
    if let Some(value) = value.get("weight") {
        result.insert("weight".into(), weight(Some(value)));
    }
    if let Some(value) = value.get("priority") {
        result.insert("priority".into(), priority(Some(value), &json!(0)));
    }
    Ok(result.into())
}
fn normalize_template(value: &Value, label: &str) -> Result<Value, Error> {
    let malformed = || error(format!("malformed template for {label}"));
    if !object(value) {
        return Err(malformed());
    }
    if !value["api"].as_str().is_some_and(|value| !value.is_empty())
        || !value["baseUrl"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        || !value["reasoning"].is_boolean()
        || !value["input"]
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
        || !object(&value["cost"])
        || !["contextWindow", "maxTokens"].iter().all(|key| {
            value[key]
                .as_f64()
                .is_some_and(|value| value.is_finite() && value > 0.0)
        })
    {
        return Err(malformed());
    }
    let mut cost = Map::new();
    for key in ["input", "output", "cacheRead", "cacheWrite"] {
        let rate = &value["cost"][key];
        if !rate
            .as_f64()
            .is_some_and(|value| value.is_finite() && value >= 0.0)
        {
            return Err(malformed());
        }
        cost.insert(key.into(), rate.clone());
    }
    let mut thinking = None;
    if let Some(map) = value.get("thinkingLevelMap") {
        if !object(map) {
            return Err(malformed());
        }
        let mut result = Map::new();
        for (level, effort) in entries(map) {
            if !effort.is_string() && !effort.is_null() {
                return Err(malformed());
            }
            // An assignment to __proto__ on the original plain object does not create an own key.
            if level != "__proto__" {
                result.insert(level, effort.clone());
            }
        }
        thinking = Some(Value::Object(result));
    }
    if value["cost"]["tiers"].is_array() {
        cost.insert("tiers".into(), value["cost"]["tiers"].clone());
    }
    let mut result =
        json!({"api":value["api"],"baseUrl":value["baseUrl"],"reasoning":value["reasoning"]});
    if let Some(thinking) = thinking {
        result["thinkingLevelMap"] = thinking;
    }
    result["input"] = value["input"].clone();
    result["cost"] = cost.into();
    result["contextWindow"] = value["contextWindow"].clone();
    result["maxTokens"] = value["maxTokens"].clone();
    Ok(result)
}

pub fn normalize_virtual(value: &Value) -> Result<Value, Error> {
    if !object(value) {
        return Err(error("malformed virtual provider"));
    }
    let id = virtual_id(&value["id"], "provider id")?;
    let Some(label) = value["label"]
        .as_str()
        .filter(|value| !trim(value).is_empty())
    else {
        return Err(error(format!(
            "malformed label for virtual provider \"{id}\""
        )));
    };
    let Some(models) = value["models"]
        .as_array()
        .filter(|values| !values.is_empty())
    else {
        return Err(error(format!(
            "malformed models for virtual provider \"{id}\""
        )));
    };
    let models: Result<Vec<_>, Error> = models.iter().map(|model| {
        if !object(model) { return Err(error(format!("malformed model for virtual provider \"{id}\""))); }
        let model_id = virtual_id(&model["id"], "model id")?;
        let Some(backends) = model["backends"].as_array().filter(|values| !values.is_empty()) else { return Err(error(format!("malformed backends for virtual model \"{model_id}\""))); };
        let mut seen = HashSet::new();
        let backends: Vec<_> = backends.iter().map(|backend| {
            if !object(backend) { return Err(error(format!("malformed backend for virtual model \"{model_id}\""))); }
            let provider = virtual_id(&backend["providerId"], "backend provider id")?;
            let backend_model = virtual_id(&backend["modelId"], "backend model id")?;
            if backend.get("enabled").is_some_and(|value| !value.is_boolean()) { return Err(error(format!("malformed backend enabled for virtual model \"{model_id}\""))); }
            let key = format!("{provider}::{backend_model}");
            if !seen.insert(key.clone()) { return Err(error(format!("duplicate backend \"{key}\" for virtual model \"{model_id}\""))); }
            let template = backend.get("template").map(|value| normalize_template(value, &format!("virtual model \"{model_id}\" backend \"{provider}/{backend_model}\""))).transpose()?;
            let mut result = json!({"providerId":provider,"modelId":backend_model});
            if let Some(enabled) = backend.get("enabled") { result["enabled"] = enabled.clone(); }
            result["weight"] = weight(backend.get("weight"));
            if let Some(template) = template { result["template"] = template; }
            Ok(result)
        }).collect::<Result<_, Error>>()?;
        let mut result = json!({"id":model_id});
        if let Some(label) = model.get("label") {
            let label = trim_method(label, "model.label")?;
            if !label.is_empty() { result["label"] = json!(label); }
        }
        result["backends"] = json!(backends);
        Ok(result)
    }).collect();
    Ok(json!({"id":id,"label":trim(label),"models":models?}))
}
