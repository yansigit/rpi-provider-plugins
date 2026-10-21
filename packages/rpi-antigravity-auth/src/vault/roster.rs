//! Identity-bound roster projection and persisted-state validation.
use super::policy::ConsumerError;
use crate::json::trim;
use rpi_node_collation::Collator;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

fn rows(value: Option<&Value>) -> &[Value] {
    value
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}
pub fn member_ids(row: &Value) -> Vec<&str> {
    std::iter::once(text(row, "credentialId"))
        .chain(rows(row.get("aliases")).iter().map(|v| v.as_str().unwrap()))
        .collect()
}
fn matches(entry: &Value, id: &str, identity: Option<&Value>) -> bool {
    match (entry.get("accountIdentity"), identity) {
        (Some(left), Some(right)) => left == right,
        _ => entry["credentialId"] == id,
    }
}
pub fn is_declined(entries: &[Value], id: &str, identity: Option<&Value>) -> bool {
    entries.iter().any(|entry| matches(entry, id, identity))
}
pub fn decline_account(entries: &[Value], id: &str, identity: Option<&Value>) -> Vec<Value> {
    let mut result: Vec<_> = entries
        .iter()
        .filter(|entry| entry["credentialId"] != id || entry.get("accountIdentity") != identity)
        .cloned()
        .collect();
    let mut entry = json!({"credentialId":id});
    if let Some(identity) = identity {
        entry["accountIdentity"] = identity.clone();
    }
    result.push(entry);
    result
}
pub fn accept_account(entries: &[Value], ids: &[&str], identity: Option<&Value>) -> Vec<Value> {
    entries
        .iter()
        .filter(|entry| !ids.iter().any(|id| matches(entry, id, identity)))
        .cloned()
        .collect()
}

pub struct Projection {
    pub now: f64,
    pub prefix: String,
    pub reserved: HashSet<String>,
    pub collator: Collator,
}
impl Projection {
    pub fn new(locale: &str, now: f64) -> Result<Self, ConsumerError> {
        Ok(Self {
            now,
            prefix: "vault:".into(),
            reserved: HashSet::new(),
            collator: rpi_node_collation::collator(locale).map_err(|e| {
                ConsumerError::new(
                    "invalid-state",
                    format!("Unable to initialize vault roster locale: {e}"),
                )
            })?,
        })
    }
}
struct Member<'a> {
    credential: &'a Value,
    identity: Option<&'a Value>,
    claimed: bool,
}

/// Inputs have crossed the inventory and roster decoders; labels come from the vault.
pub fn project(previous: Option<&Value>, inventory: &Value, options: &Projection) -> Value {
    let previous_rows = rows(previous.and_then(|v| v.get("rows")));
    let declined = rows(previous.and_then(|v| v.get("declined")));
    let credentials = rows(inventory.get("credentials"));
    let skipped = rows(inventory.get("skipped"));
    let listed: HashSet<_> = credentials
        .iter()
        .map(|v| text(v, "credentialId"))
        .collect();
    let rejected: HashSet<_> = skipped
        .iter()
        .filter_map(|v| v.get("credentialId").and_then(Value::as_str))
        .collect();
    let unrecoverable = skipped.iter().any(|v| v.get("credentialId").is_none());
    let unaccounted = |id: &&str| !listed.contains(id) && (unrecoverable || rejected.contains(id));
    let mut known = HashMap::new();
    for row in previous_rows {
        if let Some(identity) = row.get("accountIdentity") {
            for id in member_ids(row) {
                known.entry(id).or_insert(identity);
            }
        }
    }
    let mut groups: indexmap::IndexMap<String, Vec<Member<'_>>> = indexmap::IndexMap::new();
    for credential in credentials {
        let claimed = credential.get("accountIdentity");
        let id = text(credential, "credentialId");
        let identity = claimed.or_else(|| known.get(id).copied());
        let key = identity
            .map(|v| format!("identity:{}", v.as_str().unwrap()))
            .unwrap_or_else(|| format!("credential:{id}"));
        groups.entry(key).or_default().push(Member {
            credential,
            identity,
            claimed: claimed.is_some(),
        });
    }
    let by_id = |left: &Member<'_>, right: &Member<'_>| {
        options.collator.compare(
            text(left.credential, "credentialId"),
            text(right.credential, "credentialId"),
        )
    };
    let mut groups: Vec<_> = groups.into_values().collect();
    for group in &mut groups {
        group.sort_by(by_id);
    }
    groups.sort_by(|left, right| by_id(&left[0], &right[0]));
    let mut used = HashSet::new();
    let mut taken = options.reserved.clone();
    let mut projected = vec![];
    for ordered in groups {
        let identity = ordered[0].identity;
        let active: Vec<_> = ordered
            .iter()
            .filter(|v| v.credential["state"] == "active")
            .collect();
        let usable: Vec<_> = if active.is_empty() {
            ordered.iter().collect()
        } else {
            active
        };
        let claimed: Vec<_> = usable.iter().copied().filter(|v| v.claimed).collect();
        let choices = if claimed.is_empty() { usable } else { claimed };
        let members: HashSet<_> = ordered
            .iter()
            .map(|v| text(v.credential, "credentialId"))
            .collect();
        let bound = previous_rows.iter().enumerate().find(|(index, row)| {
            !used.contains(index)
                && member_ids(row).iter().any(|id| members.contains(id))
                && (row.get("accountIdentity").is_none() || row.get("accountIdentity") == identity)
        });
        let chosen = choices
            .iter()
            .find(|v| {
                bound.is_some_and(|(_, row)| row["credentialId"] == v.credential["credentialId"])
            })
            .unwrap_or(&choices[0]);
        let representative = chosen.credential;
        let existing = bound.or_else(|| {
            identity.and_then(|id| {
                previous_rows.iter().enumerate().find(|(index, row)| {
                    !used.contains(index) && row.get("accountIdentity") == Some(id)
                })
            })
        });
        if let Some((index, _)) = existing {
            used.insert(index);
        }
        let mut route = existing
            .map(|(_, row)| text(row, "routeId").to_owned())
            .unwrap_or_default();
        if route.is_empty() || options.reserved.contains(&route) {
            let hash = identity
                .map(|v| {
                    format!("~{:x}", Sha256::digest(v.as_str().unwrap().as_bytes()))[..11]
                        .to_owned()
                })
                .unwrap_or_default();
            let base = format!(
                "{}{}{hash}",
                options.prefix,
                text(representative, "credentialId")
            );
            route = base.clone();
            let mut suffix = 2;
            while taken.contains(&route) {
                route = format!("{base}~{suffix}");
                suffix += 1;
            }
        }
        taken.insert(route.clone());
        let retained = existing
            .map(|(_, row)| member_ids(row))
            .unwrap_or_default()
            .into_iter()
            .filter(unaccounted);
        let mut seen = HashSet::new();
        let mut aliases: Vec<_> = ordered
            .iter()
            .map(|v| text(v.credential, "credentialId"))
            .chain(retained)
            .filter(|id| *id != text(representative, "credentialId") && seen.insert(*id))
            .collect();
        aliases.sort_by(|a, b| options.collator.compare(a, b));
        let id = text(representative, "credentialId");
        let label = id
            .splitn(3, ':')
            .nth(2)
            .map(trim)
            .filter(|v| !v.is_empty())
            .or_else(|| {
                representative
                    .get("email")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(id);
        let mut row = json!({"routeId":route,"credentialId":id,"credentialType":representative["credentialType"]});
        if let Some(identity) = identity {
            row["accountIdentity"] = identity.clone();
        }
        if !aliases.is_empty() {
            row["aliases"] = json!(aliases);
        }
        row["state"] = representative["state"].clone();
        row["label"] = json!(label);
        for field in ["email", "orgName"] {
            if let Some(value) = representative.get(field) {
                row[field] = value.clone();
            }
        }
        row["enabled"] = json!(!std::iter::once(id)
            .chain(aliases.iter().copied())
            .any(|id| is_declined(declined, id, identity)));
        row["addedAt"] = existing
            .map(|(_, r)| r["addedAt"].clone())
            .unwrap_or_else(|| crate::json::finite_number(options.now));
        if let Some(quota) = existing.and_then(|(_, r)| r.get("quota")) {
            row["quota"] = quota.clone();
        }
        if !chosen.claimed && identity.is_some() {
            row["unclaimed"] = json!(true);
        }
        projected.push((existing.map(|(index, _)| index).unwrap_or(usize::MAX), row));
    }
    for (index, row) in previous_rows.iter().enumerate() {
        if used.contains(&index)
            || !member_ids(row).iter().any(unaccounted)
            || taken.contains(text(row, "routeId"))
        {
            continue;
        }
        let ids: Vec<_> = member_ids(row)
            .into_iter()
            .filter(|id| !listed.contains(id))
            .collect();
        let Some((id, aliases)) = ids.split_first() else {
            continue;
        };
        taken.insert(text(row, "routeId").to_owned());
        let mut row = row.clone();
        row.as_object_mut().unwrap().shift_remove("aliases");
        row["credentialId"] = json!(id);
        if !aliases.is_empty() {
            row["aliases"] = json!(aliases);
        }
        row["stale"] = json!(true);
        projected.push((index, row));
    }
    projected.sort_by(|(a, left), (b, right)| {
        a.cmp(b).then_with(|| {
            options
                .collator
                .compare(text(left, "credentialId"), text(right, "credentialId"))
        })
    });
    let mut result = json!({"version":1,"view":inventory["view"],"complete":skipped.is_empty()});
    if !skipped.is_empty() {
        result["rejected"] = json!(skipped
            .iter()
            .map(|entry| {
                let mut v = json!({});
                if let Some(id) = entry.get("credentialId") {
                    v["credentialId"] = id.clone();
                }
                v["reason"] = entry["reason"].clone();
                v
            })
            .collect::<Vec<_>>());
    }
    result["rows"] = json!(projected
        .into_iter()
        .map(|(_, row)| row)
        .collect::<Vec<_>>());
    result["declined"] = json!(declined);
    result
}

fn optional(value: &Value, field: &str, check: impl Fn(&Value) -> bool) -> bool {
    value.get(field).map_or(true, check)
}
fn finite(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}
fn name(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}
fn counter(value: &Value) -> bool {
    value.is_object()
        && value["value"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= 0.0)
        && finite(&value["checkedAt"])
}
fn quota(value: &Value) -> Option<Value> {
    let limits = value.get("limits")?.as_array()?;
    let mut seen = HashSet::new();
    for entry in limits {
        if !entry.is_object()
            || !name(&entry["scope"])
            || !name(&entry["label"])
            || !seen.insert((text(entry, "scope"), text(entry, "label")))
        {
            return None;
        }
        let valid = match entry["kind"].as_str() {
            Some("reading") => {
                finite(&entry["checkedAt"])
                    && finite(&entry["usedPercent"])
                    && optional(entry, "resetsAt", Value::is_string)
                    && optional(entry, "windowMinutes", |v| {
                        v.as_f64().is_some_and(|n| n.is_finite() && n > 0.0)
                    })
            }
            Some("retired") => finite(&entry["retiredAt"]),
            Some("absent") => finite(&entry["checkedAt"]),
            _ => false,
        };
        if !valid {
            return None;
        }
    }
    if !optional(value, "budget", |budget| {
        budget.is_object()
            && finite(&budget["checkedAt"])
            && (budget["kind"] == "cleared"
                || budget["kind"] == "reading"
                    && budget["reached"].is_boolean()
                    && [
                        "remainingPercent",
                        "usedPercent",
                        "limit",
                        "used",
                        "remaining",
                    ]
                    .iter()
                    .all(|key| optional(budget, key, finite))
                    && ["resetsAt", "unit"]
                        .iter()
                        .all(|key| optional(budget, key, Value::is_string)))
    }) {
        return None;
    }
    let mut result = value.clone();
    if let Some(counters) = value.get("counters") {
        if counters
            .as_object()
            .is_some_and(|v| v.values().all(counter))
        {
            return Some(result);
        }
        result.as_object_mut()?.shift_remove("counters");
        let usable: serde_json::Map<_, _> = counters
            .as_object()
            .into_iter()
            .flat_map(|v| v.iter())
            .filter(|(_, v)| counter(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !usable.is_empty() {
            result["counters"] = Value::Object(usable);
        }
    }
    Some(result)
}
fn invalid() -> ConsumerError {
    ConsumerError::new("invalid-state", "Claustrum roster file is not valid")
}
pub fn decode(value: &Value) -> Result<Value, ConsumerError> {
    if !value.is_object()
        || value["version"].as_f64() != Some(1.0)
        || !optional(value, "view", Value::is_string)
        || !value["rows"].is_array()
        || !value["declined"].is_array()
        || !optional(value, "complete", Value::is_boolean)
        || !optional(value, "rejected", |v| {
            v.as_array().is_some_and(|entries| {
                entries.iter().all(|entry| {
                    entry.is_object()
                        && optional(entry, "credentialId", Value::is_string)
                        && matches!(
                            entry["reason"].as_str(),
                            Some(
                                "empty credential id"
                                    | "duplicate credential id"
                                    | "blank account identity"
                                    | "empty state"
                            )
                        )
                })
            })
        })
    {
        return Err(invalid());
    }
    let mut decoded = vec![];
    for row in rows(value.get("rows")) {
        if !row.is_object()
            || !["routeId", "credentialId", "state", "label"]
                .iter()
                .all(|k| row[*k].is_string())
            || !matches!(row["credentialType"].as_str(), Some("oauth" | "api_key"))
            || !row["enabled"].is_boolean()
            || !row["addedAt"].is_number()
            || !["accountIdentity", "email", "orgName"]
                .iter()
                .all(|k| optional(row, k, Value::is_string))
            || !optional(row, "aliases", |v| {
                v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            })
            || !["stale", "unclaimed"]
                .iter()
                .all(|k| optional(row, k, |v| v == true))
        {
            return Err(invalid());
        }
        let mut copy = row.clone();
        if let Some(value) = row.get("quota") {
            copy["quota"] = quota(value).ok_or_else(invalid)?;
        }
        decoded.push(copy);
    }
    if !rows(value.get("declined")).iter().all(|entry| {
        entry.is_object()
            && entry["credentialId"].is_string()
            && optional(entry, "accountIdentity", Value::is_string)
    }) {
        return Err(invalid());
    }
    let mut result = json!({"version":1});
    if let Some(view) = value.get("view") {
        result["view"] = view.clone();
    }
    result["complete"] = json!(value["complete"] != false);
    if let Some(rejected) = value.get("rejected") {
        result["rejected"] = rejected.clone();
    }
    result["rows"] = json!(decoded);
    result["declined"] = value["declined"].clone();
    Ok(result)
}
pub fn change_route(current: &Value, route: &str, accept: bool) -> Result<Value, ConsumerError> {
    let index = rows(current.get("rows"))
        .iter()
        .position(|row| row["routeId"] == route)
        .ok_or_else(|| {
            ConsumerError::new("route-unavailable", "Claustrum route is not in the roster")
        })?;
    let row = &current["rows"][index];
    let entries = rows(current.get("declined"));
    let identity = row.get("accountIdentity");
    let declined = if accept {
        accept_account(entries, &member_ids(row), identity)
    } else {
        decline_account(entries, text(row, "credentialId"), identity)
    };
    let mut next = current.clone();
    next["declined"] = json!(declined);
    next["rows"][index]["enabled"] = json!(accept);
    Ok(next)
}
