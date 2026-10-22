//! Cloud selection/lifecycle parity fixture (ledger C06).
//!
//! `tests/c-cloud/run-reference.mjs` runs the unchanged Pi 1.3.0 CLI with the
//! pinned `pi-cursor-sdk@0.5.3` adapter against the scenario-driven fake
//! `@cursor/sdk`; `tests/c-cloud/run-rpi.mjs` drives the retained rpi worker
//! (packaged `runtime/main.mjs`, private peer protocol, emulated native host
//! including the durable journal port) against the same fake and scenarios.
//! This test loads both committed fixtures with `include_str!` and compares the
//! COMPLETE `observed` objects for every scenario. It needs no Node runtime and
//! no network.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const REFERENCE: &str = include_str!("c-cloud/reference-evidence.json");
const RPI: &str = include_str!("c-cloud/evidence.json");

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/c-cloud")
}

fn scenarios(evidence: &Value) -> &Vec<Value> {
    evidence["scenarios"]
        .as_array()
        .expect("evidence scenarios is an array")
}

fn scenario<'a>(evidence: &'a Value, name: &str) -> &'a Value {
    scenarios(evidence)
        .iter()
        .find(|scenario| scenario["name"] == name)
        .unwrap_or_else(|| panic!("scenario {name} missing from evidence"))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn every_cloud_scenario_matches_the_unchanged_pi_reference() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    assert_eq!(reference["status"], "pass", "reference fixture is not passing");
    assert_eq!(rpi["status"], "pass", "rpi fixture is not passing");
    assert_eq!(reference["schema"], "rpi-cursor-c-cloud-evidence/1");
    assert_eq!(rpi["schema"], "rpi-cursor-c-cloud-evidence/1");
    assert_eq!(reference["role"], "reference");
    assert_eq!(rpi["role"], "candidate");

    let candidate_scenarios = scenarios(&rpi);
    assert_eq!(candidate_scenarios.len(), 13, "scenario count");
    for candidate in candidate_scenarios {
        let name = candidate["name"].as_str().unwrap();
        assert_eq!(candidate["status"], "pass", "{name} did not pass");
        assert!(
            candidate["problems"].as_array().unwrap().is_empty(),
            "{}: {:?}",
            name,
            candidate["problems"]
        );
        assert_eq!(candidate["determinism"]["candidate"], true, "{name}");
        let reference_scenario = scenario(&reference, name);
        assert_eq!(reference_scenario["status"], "pass");
        assert_eq!(reference_scenario["determinism"]["reference"], true);
        // Complete value comparison of the whole observable object.
        assert_eq!(
            reference_scenario["observed"], candidate["observed"],
            "{name} differs from the Pi 1.3.0 reference"
        );
    }
}

#[test]
fn recorded_scenario_and_script_hashes_match_the_committed_files() {
    for text in [REFERENCE, RPI] {
        let evidence: Value = serde_json::from_str(text).unwrap();
        for scenario in scenarios(&evidence) {
            let relative = scenario["scenarioFile"].as_str().unwrap();
            let relative = relative.strip_prefix("tests/c-cloud/").unwrap();
            let bytes = std::fs::read(fixture_dir().join(relative)).unwrap();
            assert_eq!(
                scenario["scenarioSha256"].as_str().unwrap(),
                sha256(&bytes),
                "{relative} changed since the evidence was written"
            );
        }
        let files = evidence["files"]
            .as_object()
            .expect("evidence records script hashes");
        for (relative, recorded) in files {
            let bytes = std::fs::read(fixture_dir().join(relative)).unwrap();
            assert_eq!(
                recorded.as_str().unwrap(),
                sha256(&bytes),
                "tests/c-cloud/{relative} changed since the evidence was written"
            );
        }
    }
}

#[test]
fn reference_provenance_pins_the_unchanged_inputs() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    assert_eq!(reference["reference"]["piVersion"], "1.3.0");
    assert_eq!(reference["reference"]["cursorSdkVersion"], "1.0.37");
    assert_eq!(reference["reference"]["piCursorSdkVersion"], "0.5.3");
    assert_eq!(reference["clock"], "2026-01-01T00:00:00.000Z");
    assert_eq!(std::fs::read_to_string(fixture_dir().join("README.md")).is_ok(), true);
    for (name, digest) in reference["reference"]["files"].as_object().unwrap() {
        let digest = digest.as_str().unwrap();
        assert_eq!(digest.len(), 64, "{name} is not a sha256 digest");
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}

#[test]
fn cloud_create_carries_the_repository_ref_and_environment_options() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    let observed = &scenario(&rpi, "cloud-create-archive")["observed"];
    let create = observed["cloudCalls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|call| call["api"] == "Agent.create")
        .expect("cloud turn creates one SDK agent");
    assert_eq!(create["options"]["apiKey"], "<redacted>");
    assert_eq!(create["options"]["mode"], "agent");
    assert_eq!(
        create["options"]["cloud"]["repos"],
        serde_json::json!([{
            "url": "https://github.com/example-org/sample-repo",
            "startingRef": "main"
        }])
    );
    assert_eq!(
        create["options"]["cloud"]["env"],
        serde_json::json!({"type": "pool", "name": "fake-pool"})
    );
    // The runtime is recorded as cloud on the send.
    let send = observed["cloudCalls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|call| call["api"] == "agent.send")
        .expect("cloud turn sends one prompt");
    assert_eq!(send["runtime"], "cloud");
}

#[test]
fn lifecycle_records_every_intent_before_its_mutation() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    let happy = &scenario(&rpi, "cloud-create-archive")["observed"];
    let actions: Vec<String> = happy["lifecycle"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        actions,
        vec![
            "record",
            "record",
            "record",
            "archive_intent",
            "archive",
            "delete_intent",
            "delete",
        ],
        "intent is persisted before the archive/delete SDK mutation"
    );
    // The durable journal carries the same ordered actions and the complete
    // agent identity; only per-run session identifiers are placeholdered.
    let journal_actions: Vec<String> = happy["journal"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(journal_actions, actions);
    for entry in happy["journal"].as_array().unwrap() {
        assert_eq!(entry["agentId"], "bc-00000000-0000-4000-8000-000000000001");
        assert_eq!(entry["runtime"], "cloud");
        assert_eq!(entry["version"], 1);
        assert_eq!(entry["sessionId"], "<session>");
        assert_eq!(entry["sessionFile"], "<file>");
        assert_eq!(entry["anchorEntryId"], "<anchor>");
    }
    // The run report with its PR branch is persisted on the recorded agent.
    let branch = happy["lifecycle"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["branches"].is_array())
        .expect("a record carries the run branches");
    assert_eq!(
        branch["branches"],
        serde_json::json!([{
            "branch": "main",
            "prUrl": "https://github.com/example-org/sample-repo/pull/1"
        }])
    );
}

#[test]
fn a_failed_sync_leaves_the_intent_pending_and_is_not_replayed() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    let failed = &scenario(&rpi, "cloud-archive-uncertain")["observed"];
    // The archive SDK call was attempted exactly once and failed.
    assert_eq!(failed["sdkCalls"]["Agent.archive"], 1);
    // The durable intent survives without an `archive` result entry.
    let actions: Vec<String> = failed["lifecycle"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap().to_string())
        .collect();
    assert!(actions.contains(&"archive_intent".to_string()));
    assert!(!actions.contains(&"archive".to_string()));
    // The retry is refused before any second SDK mutation.
    assert_eq!(failed["sdkCalls"]["Agent.archive"], 1);
    let notifications = scenario(&rpi, "cloud-archive-uncertain")["notifications"]
        .as_array()
        .unwrap();
    let texts: Vec<&str> = notifications
        .iter()
        .map(|notification| notification["text"].as_str().unwrap())
        .collect();
    assert!(texts.iter().any(|text| text.contains("is unresolved")));
    assert!(texts.iter().any(|text| text.contains("archive pending")));
    assert!(texts
        .iter()
        .any(|text| text.contains("unresolved archive request")));
}

#[test]
fn explicit_delete_requires_yes_and_no_mutation_ever_replays() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    // Without --yes the agent is never deleted.
    let refused = &scenario(&rpi, "cloud-delete-without-yes")["observed"];
    assert!(refused["sdkCalls"].get("Agent.delete").is_none());
    let notifications = scenario(&rpi, "cloud-delete-without-yes")["notifications"]
        .as_array()
        .unwrap();
    assert!(notifications.iter().any(|notification| notification["text"]
        .as_str()
        .unwrap()
        .contains("Delete requires exactly one recorded bc- cloud agent ID and --yes")));
    // Listing repeatedly and re-archiving an archived agent never re-runs a mutation.
    let no_replay = &scenario(&rpi, "cloud-list-no-replay")["observed"];
    assert_eq!(no_replay["sdkCalls"]["Agent.archive"], 1);
    assert_eq!(no_replay["sdkCalls"]["Agent.delete"], 1);
    assert_eq!(no_replay["lifecycleCounts"]["archive"], 1);
    assert_eq!(no_replay["lifecycleCounts"]["delete"], 1);
}

#[test]
fn every_restriction_refuses_the_send_before_any_sdk_agent() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    let restrictions = [
        "cloud-ack-missing",
        "cloud-repo-invalid",
        "cloud-branch-no-repo",
        "cloud-branch-invalid",
        "cloud-env-forwarding",
        "cloud-env-type-invalid",
        "cloud-env-type-required",
        "cloud-env-repo-conflict",
    ];
    for name in restrictions {
        let observed = &scenario(&rpi, name)["observed"];
        assert!(
            observed["sdkCalls"]
                .as_object()
                .unwrap()
                .get("Agent.create")
                .is_none(),
            "{name} must not create an SDK agent"
        );
        let error = observed["sendError"]
            .as_str()
            .unwrap_or_else(|| panic!("{name} must surface a refused send"));
        assert!(
            error.starts_with("Cursor cloud runtime is not ready to start."),
            "{name}: {error}"
        );
    }
}

#[test]
fn unknown_recorded_agent_is_not_mutated() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    let observed = &scenario(&rpi, "cloud-archive-unknown")["observed"];
    assert!(observed["sdkCalls"].get("Agent.archive").is_none());
    let notifications = scenario(&rpi, "cloud-archive-unknown")["notifications"]
        .as_array()
        .unwrap();
    let texts = strings(&serde_json::Value::Array(
        notifications
            .iter()
            .map(|notification| notification["text"].clone())
            .collect(),
    ));
    assert!(texts
        .iter()
        .any(|text| text.contains("is not recorded in this session branch")));
}
