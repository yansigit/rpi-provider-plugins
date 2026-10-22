//! Differential test for the extension's provider-composition layer.
//!
//! `tests/ext-diff-reference.mjs` drives the unchanged upstream extension
//! through Pi's plain registry and through the composed wrapper Pi returns when
//! another extension overlays a provider (sha256-asserted). It records every
//! `registerProvider`/`unregisterProvider` with the registered provider's
//! classification, so this test can replay the same sessions and compare the
//! complete trace: which provider each session lifts, what a departing session
//! hands back, and the registry left behind.
mod common;

use common::normalize;
use rpi_multiprovider::composition::{Composition, Session};
use rpi_multiprovider::virtual_provider::VirtualProviderConfig;
use serde_json::{json, Value};
use std::collections::HashMap;

const FIXTURE: &str = include_str!("ext-diff-reference.json");

struct Run {
    sessions: HashMap<usize, Session>,
}

fn run_case(case: &Value) -> Value {
    let input = &case["input"];
    let composed = input["registry"].as_str() == Some("composed");
    let mut composition = Composition::new(composed);
    let mut run = Run {
        sessions: HashMap::new(),
    };

    let models: Vec<Value> = input["models"].as_array().cloned().unwrap_or_default();
    for id in input["bases"].as_array().unwrap() {
        composition.add_base(id.as_str().unwrap(), models.clone());
    }
    let mut virtuals: Vec<VirtualProviderConfig> = input["virtual"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("virtual config"))
        .collect();
    let stored: Vec<String> = input["managed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();

    let mut requests = Vec::new();
    for op in input["ops"].as_array().unwrap() {
        let session_index = op["session"].as_u64().unwrap() as usize;
        let name = op["op"].as_str().unwrap();
        composition
            .trace
            .push(json!({"op": "step", "name": format!("{session_index}:{name}")}));
        match name {
            "load" => {
                let mut session = Session::default();
                composition.load(&mut session, &mut virtuals);
                run.sessions.insert(session_index, session);
            }
            "start" | "before_agent_start" => {
                let session = run.sessions.get_mut(&session_index).expect("loaded session");
                composition.reconcile(session, &mut virtuals, &stored);
            }
            "shutdown" => {
                let session = run.sessions.get_mut(&session_index).expect("loaded session");
                composition.shutdown(session);
            }
            "request" => requests.push(composition.classify(composition.get("example"))),
            other => panic!("unknown op {other}"),
        }
    }

    let mut registry = serde_json::Map::new();
    for id in input["ids"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        registry.insert(
            id.into(),
            if composition.has(id) {
                json!({
                    "view": composition.classify(composition.get(id)),
                    "native": composition.classify(composition.native(id)),
                })
            } else {
                Value::Null
            },
        );
    }
    let notify: Vec<Value> = composition
        .notifications
        .iter()
        .map(|(message, level)| json!([message, level]))
        .collect();

    json!({
        "trace": composition.trace,
        "registry": registry,
        "requests": requests,
        "notify": notify,
    })
}

#[test]
fn composition_cases_match_the_upstream_reference() {
    let reference: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = reference["composition"].as_array().unwrap();
    assert!(!cases.is_empty(), "the reference records composition cases");
    for case in cases {
        let actual = run_case(case);
        assert_eq!(
            normalize(&actual),
            normalize(&case["expected"]),
            "{}",
            case["name"]
        );
    }
}

/// Graph decisions the scripted sessions do not reach: the identity guards of
/// `isLiftOf`, `realBase` walking a lift of a lift, and `replaceLiveProvider`
/// when the departing provider was never live.
#[test]
fn graph_identity_guards_match_the_upstream_functions() {
    use rpi_multiprovider::composition::{Graph, Live};

    let mut graph = Graph::new();
    let base = graph.add_base("example", Vec::new());
    let lift = graph.add_lift(base);
    let composed = graph.add_composed(lift);
    let composed_base = graph.add_composed(base);
    let second = graph.add_lift(base);

    // `isLiftOf` needs both sides; only a shared headers object crosses objects.
    assert!(!graph.is_lift_of(None, None));
    assert!(!graph.is_lift_of(Some(lift), None));
    assert!(!graph.is_lift_of(None, Some(lift)));
    assert!(graph.is_lift_of(Some(lift), Some(lift)));
    assert!(graph.is_lift_of(Some(composed), Some(lift)));
    assert!(!graph.is_lift_of(Some(composed_base), Some(lift)));
    assert!(!graph.is_lift_of(Some(lift), Some(second)));

    // `realBase` reaches through Pi's composed wrapper and through a lift of a
    // lift down to the provider the extension must lift. An untagged composed
    // wrapper is matched against the live lifts.
    let mut live = Live::default();
    live.replace("example", None, Some(lift));
    assert_eq!(graph.real_base(&live, "example", Some(composed)), Some(base));
    assert_eq!(graph.real_base(&live, "example", Some(base)), Some(base));
    assert_eq!(graph.real_base(&live, "example", None), None);
    let chained = graph.add_lift(lift);
    assert_eq!(graph.real_base(&live, "example", Some(chained)), Some(base));

    // `replaceLiveProvider`: removing an absent provider changes nothing, and
    // removing the last one forgets the id.
    assert_eq!(live.replace("example", Some(second), None), Some(lift));
    assert_eq!(live.replace("example", Some(lift), None), None);
    assert!(live.get("example").is_empty());
}

/// A managed pool whose base is not in the registry warns once and stays
/// uninstalled until the base appears.
#[test]
fn install_waits_for_a_missing_base_and_warns_once() {
    use rpi_multiprovider::composition::{Composition, Session};

    let mut composition = Composition::new(false);
    let mut session = Session::default();
    session.managed.insert("example".into());
    composition.reconcile(&mut session, &mut [], &["example".into()]);
    composition.reconcile(&mut session, &mut [], &["example".into()]);
    assert_eq!(
        composition.notifications,
        vec![(
            "multiprovider: provider \"example\" is not registered".to_owned(),
            "warning".to_owned()
        )]
    );
    assert!(composition.trace.is_empty());

    composition.add_base("example", Vec::new());
    composition.reconcile(&mut session, &mut [], &["example".into()]);
    assert_eq!(composition.trace.len(), 1);
    assert_eq!(composition.trace[0]["op"], "register");
    assert_eq!(composition.trace[0]["isLift"], true);
    assert_eq!(composition.trace[0]["liftedBaseId"], "example");
}
