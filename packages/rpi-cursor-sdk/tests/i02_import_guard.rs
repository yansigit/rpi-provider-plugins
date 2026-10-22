//! I02: the committed fixture is produced by the unchanged pinned
//! pi-cursor-sdk build plus the unchanged Pi 1.3.0 session manager. Every case
//! compares the COMPLETE decision object, and the upstream-acceptance summaries
//! compare the guard's identifier and ledger predicates with that code.

use rpi_cursor_sdk::import_guard::{
    self, accepted_cleanup_entry, accepted_resume_entry, cloud_agents, valid_session_id, Artifact,
    Authority, Decision,
};
use serde_json::{json, Value};
use std::path::Path;

fn fixture() -> Value {
    serde_json::from_str(include_str!("i02-import-guard.json")).unwrap()
}

fn lines(case: &Value) -> Vec<String> {
    case["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect()
}

fn text(case: &Value) -> String {
    let mut text = lines(case).join("\n");
    text.push('\n');
    text
}

fn artifacts(case: &Value) -> Vec<Artifact> {
    case["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| {
            Artifact::new(
                artifact["name"].as_str().unwrap(),
                artifact["text"].as_str().unwrap(),
            )
        })
        .collect()
}

/// Lenient per-line parse, mirroring the reference's independent reporting.
fn parse_lines(case: &Value) -> (Option<Value>, Vec<Value>) {
    let lines = lines(case);
    let header = lines
        .first()
        .and_then(|line| serde_json::from_str::<Value>(line.trim_end_matches('\r')).ok())
        .filter(Value::is_object);
    let entries = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line.trim_end_matches('\r')).ok())
        .filter(Value::is_object)
        .collect();
    (header, entries)
}

#[test]
fn every_decision_matches_the_reference_case() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    let mut refused = 0;
    let mut allowed = 0;
    for case in cases {
        let authority = Authority::parse(case["authority"].as_str().unwrap()).unwrap();
        let decision = import_guard::evaluate(
            Path::new(case["path"].as_str().unwrap()),
            &text(case),
            authority,
            &artifacts(case),
        );
        assert_eq!(decision.to_json(), case["result"], "{}", case["id"]);
        if decision.allowed {
            allowed += 1;
        } else {
            refused += 1;
        }
    }
    assert_eq!(fixture["counts"]["cases"], json!(cases.len()));
    assert_eq!(fixture["counts"]["refused"], json!(refused));
    assert_eq!(fixture["counts"]["allowed"], json!(allowed));
    assert_eq!(fixture["counts"]["resume"], json!(21));
    assert_eq!(fixture["counts"]["cleanup"], json!(5));
    assert_eq!(fixture["counts"]["cloud"], json!(12));
    assert_eq!(
        fixture["reference"],
        "f337dc4bb7cea936c52e69a7fe08e7e376c8467b"
    );
    assert_eq!(fixture["pi"], "1.3.0");
    assert_eq!(fixture["upstreamHashes"].as_object().unwrap().len(), 5);
}

#[test]
fn upstream_acceptance_summaries_match_the_pinned_code() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let (header, entries) = parse_lines(case);
        let upstream = &case["upstream"];
        let session_id = header
            .as_ref()
            .and_then(|header| header["id"].as_str())
            .unwrap_or_default();
        assert_eq!(
            valid_session_id(session_id),
            upstream["sessionIdValid"].as_bool().unwrap(),
            "{} session id",
            case["id"]
        );
        assert_eq!(upstream["piCurrentVersion"], json!(3));

        let mut resume = Vec::new();
        let mut cleanup = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match entry["customType"].as_str() {
                Some(import_guard::RESUME_ENTRY) => {
                    let parsed = accepted_resume_entry(&entry["data"]);
                    resume.push(json!({
                        "index": index,
                        "accepted": parsed.is_some(),
                        "version": parsed.as_ref().map(|value| value.version),
                        "agentId": parsed.as_ref().map(|value| value.agent_id.clone()),
                    }));
                }
                Some(import_guard::CLEANUP_ENTRY) => {
                    let parsed = accepted_cleanup_entry(&entry["data"]);
                    cleanup.push(json!({
                        "index": index,
                        "accepted": parsed.is_some(),
                        "action": parsed.as_ref().map(|value| value.action.clone()),
                        "phase": parsed.as_ref().and_then(|value| value.phase.clone()),
                    }));
                }
                _ => {}
            }
        }
        assert_eq!(json!(resume), upstream["resume"], "{} resume", case["id"]);
        assert_eq!(
            json!(cleanup),
            upstream["cleanup"],
            "{} cleanup",
            case["id"]
        );

        let cloud = cloud_agents(&entries)
            .into_iter()
            .map(|agent| {
                json!({
                    "agentId": agent.agent_id,
                    "pendingAction": agent.pending.map(|(action, _)| action),
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(json!(cloud), upstream["cloud"], "{} cloud", case["id"]);
    }
}

#[test]
fn imported_origin_never_grants_management_authority() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let authority = Authority::parse(case["authority"].as_str().unwrap()).unwrap();
        let decision = import_guard::evaluate(
            Path::new(case["path"].as_str().unwrap()),
            &text(case),
            authority,
            &artifacts(case),
        );
        if decision.origin.name() == "imported" {
            assert!(!decision.allowed, "{}", case["id"]);
        }
        if !decision.allowed && decision.authority.name() == "resume" {
            assert!(decision.fresh_agent, "{}", case["id"]);
            assert_eq!(decision.notice, Some(import_guard::FRESH_AGENT_NOTICE));
        } else if decision.allowed {
            assert!(!decision.fresh_agent, "{}", case["id"]);
            assert_eq!(decision.notice, None, "{}", case["id"]);
        }
    }
}

#[test]
fn refusals_are_bounded_before_any_dispatch() {
    let fixture = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let decision: Decision = import_guard::evaluate(
            Path::new(case["path"].as_str().unwrap()),
            &text(case),
            Authority::parse(case["authority"].as_str().unwrap()).unwrap(),
            &artifacts(case),
        );
        let serialized = decision.to_json();
        if !decision.allowed {
            assert!(serialized["code"].is_string(), "{}", case["id"]);
            assert!(serialized["agentId"].is_null(), "{}", case["id"]);
        }
    }
}
