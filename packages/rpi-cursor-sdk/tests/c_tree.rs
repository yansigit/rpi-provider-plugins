//! Cursor branch-continuity parity fixture (ledger C04).
//!
//! `tests/c-tree/run-reference.mjs` runs the unchanged Pi 1.3.0 CLI with the
//! pinned `pi-cursor-sdk` adapter against the scenario-driven fake `@cursor/sdk`;
//! `tests/c-tree/run-rpi.mjs` runs the retained rpi worker (private peer protocol)
//! against the same fake and an authoritative awaited session-port host. This test
//! compares the committed fixtures as whole JSON values, checks every rpi-only
//! contract expectation recorded by its scenario, and re-hashes every recorded
//! input so stale evidence cannot pass. It needs no Node runtime.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const REFERENCE: &str = include_str!("c-tree/reference-evidence.json");
const RPI: &str = include_str!("c-tree/evidence.json");

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/c-tree")
}

fn scenarios(evidence: &Value) -> &[Value] {
    evidence["scenarios"].as_array().unwrap()
}

fn find_scenario<'a>(evidence: &'a Value, name: &str) -> &'a Value {
    scenarios(evidence)
        .iter()
        .find(|candidate| candidate["name"] == name)
        .unwrap_or_else(|| panic!("evidence has no scenario {name}"))
}

fn comparable(observed: &Value) -> Value {
    serde_json::json!({
        "sdkCalls": observed["sdkCalls"],
        "runs": observed["runs"],
        "bridgeToolExecutions": observed["bridgeToolExecutions"],
        "toolExecutions": observed["toolExecutions"],
        "nativeToolCalls": observed["nativeToolCalls"],
        "violations": observed["violations"],
    })
}

#[test]
fn c04_entries_match_the_unchanged_pi_reference() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    assert_eq!(reference["status"], "pass", "reference fixture is not passing");
    assert_eq!(rpi["status"], "pass", "rpi fixture is not passing");

    let mut compared = 0;
    for scenario in scenarios(&rpi) {
        let name = scenario["name"].as_str().unwrap();
        assert_eq!(scenario["status"], "pass", "{name} did not pass");
        assert!(
            scenario["problems"].as_array().unwrap().is_empty(),
            "{name} recorded problems"
        );
        if scenario.get("referenceEntries").is_none() {
            // rpi-only scripted contract scenarios carry no Pi reference.
            continue;
        }
        let reference_scenario = find_scenario(&reference, name);
        assert_eq!(reference_scenario["status"], "pass");
        // Complete value comparison of every C04 session entry, in order.
        assert_eq!(
            scenario["c04Entries"],
            reference_scenario["c04Entries"],
            "{name} C04 session entries differ from the Pi 1.3.0 reference"
        );
        assert_eq!(
            scenario["c04Entries"],
            scenario["referenceEntries"],
            "{name} did not embed the compared reference entries"
        );
        // Complete comparison of the observable SDK interaction counters.
        assert_eq!(
            comparable(&scenario["observed"]),
            comparable(&reference_scenario["observed"]),
            "{name} observable counters differ from the Pi 1.3.0 reference"
        );
        compared += 1;
    }
    assert_eq!(compared, 4, "expected four reference-compared C04 scenarios");
}

#[test]
fn rpi_only_contract_scenarios_record_their_expectations() {
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    // Compaction summary agents: pooled run released, temporary summary agent,
    // resume after the compaction boundary.
    let compaction = find_scenario(&rpi, "compaction-summary");
    assert_eq!(compaction["status"], "pass");
    assert_eq!(
        compaction["c04Entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["customType"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["cursor-sdk-agent-lineage", "cursor-sdk-agent-resume", "cursor-sdk-agent-resume"]
    );
    assert_eq!(
        compaction["observed"]["sdkCalls"]["Agent.create"], 2,
        "one conversation agent and one temporary summary agent"
    );
    assert_eq!(compaction["observed"]["sdkCalls"]["Agent.resume"], 1);
    assert_eq!(compaction["observed"]["sdkCalls"]["agent.asyncDispose"], 3);

    // Late cleanup result: durable intent first, then a journalAppend that does
    // not advance the newly selected branch.
    let durability = find_scenario(&rpi, "cleanup-durability");
    assert_eq!(durability["status"], "pass");
    let details = durability["appendDetails"].as_array().unwrap();
    let intent = details
        .iter()
        .find(|item| item["customType"] == "cursor-sdk-agent-cleanup" && item["phase"] == "intent")
        .expect("durable intent");
    assert_eq!(intent["action"], "append");
    assert_eq!(intent["durability"], "durable");
    let result = details
        .iter()
        .find(|item| item["customType"] == "cursor-sdk-agent-cleanup" && item["phase"] == "result")
        .expect("cleanup result");
    assert_eq!(result["action"], "journalAppend");
    assert_eq!(result["advancedLane"], false);
    assert_ne!(durability["c04Entries"][4]["data"]["failedAgentIds"][0]["agentId"], Value::Null);

    // Non-durable result: the result is retained as an ephemeral journalAppend and
    // the durable intent remains the retry barrier.
    let nondurable = find_scenario(&rpi, "cleanup-nondurable");
    assert_eq!(nondurable["status"], "pass");
    let result = nondurable["appendDetails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["customType"] == "cursor-sdk-agent-cleanup" && item["phase"] == "result")
        .expect("cleanup result");
    assert_eq!(result["durability"], "ephemeral");
    assert_eq!(result["action"], "journalAppend");
    assert!(nondurable["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .any(|notice| notice["text"]
            .as_str()
            .unwrap()
            .contains("durable intent blocks automatic retries")));
}

#[test]
fn recorded_scenario_and_script_hashes_match_the_committed_files() {
    for text in [REFERENCE, RPI] {
        let evidence: Value = serde_json::from_str(text).unwrap();
        for scenario in scenarios(&evidence) {
            let relative = scenario["scenarioFile"].as_str().unwrap();
            let relative = relative.strip_prefix("tests/c-tree/").unwrap();
            let bytes = std::fs::read(fixture_dir().join(relative)).unwrap();
            assert_eq!(
                scenario["scenarioSha256"].as_str().unwrap(),
                sha256(&bytes),
                "{relative} changed since the evidence was written"
            );
        }
        let files = evidence
            .get("files")
            .or_else(|| evidence.get("fake"))
            .and_then(Value::as_object)
            .expect("evidence must record its script hashes");
        for (relative, recorded) in files {
            let bytes = std::fs::read(fixture_dir().join(relative)).unwrap();
            assert_eq!(
                recorded.as_str().unwrap(),
                sha256(&bytes),
                "tests/c-tree/{relative} changed since the evidence was written"
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
    for (name, digest) in reference["reference"]["files"].as_object().unwrap() {
        let digest = digest.as_str().unwrap();
        assert_eq!(digest.len(), 64, "{name} is not a sha256 digest");
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    // The fixtures must never embed the machine's temporary working tree.
    assert!(!REFERENCE.contains(".rpi-c-tree-ref-"));
    assert!(!RPI.contains(".rpi-c-tree-"));
}
