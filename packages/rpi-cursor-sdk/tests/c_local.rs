//! Local Cursor agent-path parity fixture (ledger C03).
//!
//! `tests/c_local/run-reference.mjs` runs the unchanged Pi 1.3.0 CLI with the
//! pinned `pi-cursor-sdk` adapter against the scenario-driven fake `@cursor/sdk`;
//! `tests/c_local/run-rpi.mjs` runs the retained rpi worker (private peer
//! protocol) against the same fake and scenarios. This test compares the two
//! committed fixtures as whole JSON values and re-hashes every recorded input so
//! stale evidence cannot pass. It needs no Node runtime.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const REFERENCE: &str = include_str!("c_local/reference-evidence.json");
const RPI: &str = include_str!("c_local/evidence.json");

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/c_local")
}

fn scenarios(evidence: &Value) -> &[Value] {
    evidence["scenarios"].as_array().unwrap()
}

/// Compare the observable SDK-interaction fields the two runners share.
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
fn local_agent_paths_match_the_unchanged_pi_reference() {
    let reference: Value = serde_json::from_str(REFERENCE).unwrap();
    let rpi: Value = serde_json::from_str(RPI).unwrap();
    assert_eq!(reference["status"], "pass", "reference fixture is not passing");
    assert_eq!(rpi["status"], "pass", "rpi fixture is not passing");

    let reference_scenarios = scenarios(&reference);
    let rpi_scenarios = scenarios(&rpi);
    assert!(!rpi_scenarios.is_empty());

    let mut compared = 0;
    for scenario in rpi_scenarios {
        let name = scenario["name"].as_str().unwrap();
        assert_eq!(scenario["status"], "pass", "{name} did not pass");
        let observed = &scenario["observed"];
        let Some(reference_observed) = scenario.get("referenceObserved") else {
            // rpi-only scenarios (cancellation, concurrent owners) carry no reference.
            continue;
        };
        let reference_scenario = reference_scenarios
            .iter()
            .find(|candidate| candidate["name"] == scenario["name"])
            .unwrap_or_else(|| panic!("reference evidence has no scenario {name}"));
        assert_eq!(reference_scenario["status"], "pass");
        // Complete value comparison of the whole observable interaction object.
        assert_eq!(
            comparable(observed),
            comparable(reference_observed),
            "{name} differs from the Pi 1.3.0 reference"
        );
        assert_eq!(comparable(observed), comparable(&reference_scenario["observed"]));
        compared += 1;
    }
    assert!(compared >= 4, "expected at least four reference-compared scenarios");
}

#[test]
fn recorded_scenario_and_script_hashes_match_the_committed_files() {
    for text in [REFERENCE, RPI] {
        let evidence: Value = serde_json::from_str(text).unwrap();
        for scenario in scenarios(&evidence) {
            let relative = scenario["scenarioFile"].as_str().unwrap();
            let relative = relative.strip_prefix("tests/c_local/").unwrap();
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
                "tests/c_local/{relative} changed since the evidence was written"
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
}
