//! C-USAGE C07/C09 differential: the exact usage-journal frame sequences and the
//! crashed-writer recovery prefixes recorded by the unchanged pinned upstream
//! usage writers must replay byte-for-byte through the native `JournalPort`.
//!
//! The fixtures are produced by `tests/c-usage/usage-reference.mjs` and
//! `tests/c-usage/lifecycle-reference.mjs` (Node 24, unchanged upstream modules).
//! This test is pure Rust: it compares complete byte sequences and outcomes, so
//! it runs in the ordinary `cargo test -p rpi-cursor-sdk` gate.
use base64::{engine::general_purpose::STANDARD, Engine};
use rpi_cursor_sdk::journal::port::JournalPort;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const USAGE_FIXTURE: &str = include_str!("c-usage/usage-fixture.json");
const USAGE_EVIDENCE: &str = include_str!("c-usage/evidence.json");
const LIFECYCLE_FIXTURE: &str = include_str!("c-usage/lifecycle-fixture.json");
const LIFECYCLE_EVIDENCE: &str = include_str!("c-usage/lifecycle-evidence.json");

fn scratch(label: &str) -> PathBuf {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "rpi-c-usage-{label}-{:x}",
        u128::from_ne_bytes(random)
    ));
    std::fs::create_dir(&directory).unwrap();
    directory
}

fn journal_path(directory: &Path, kind: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(b"fixture"));
    let suffix = &hash[..32];
    if kind == "usage" {
        directory.join(format!("cursor-usage-{suffix}.journal"))
    } else {
        directory.join(format!(".cursor-cloud-lifecycle-{suffix}.journal"))
    }
}

/// Read the whole journal through the port and return `(bytes, digest)`.
fn snapshot(port: &JournalPort, kind: &str) -> (Vec<u8>, String) {
    let opened = port
        .request(json!({"action": "open", "kind": kind}))
        .expect("open journal");
    let reader = opened["reader"].as_str().expect("reader").to_owned();
    let mut bytes = Vec::new();
    let digest = loop {
        let chunk = port
            .request(json!({"action": "next", "reader": reader}))
            .expect("next chunk");
        bytes.extend(STANDARD.decode(chunk["bytes"].as_str().unwrap()).unwrap());
        if !chunk["digest"].is_null() {
            break chunk["digest"].as_str().unwrap().to_owned();
        }
    };
    port.request(json!({"action": "close", "reader": reader}))
        .expect("close reader");
    (bytes, digest)
}

fn outcome(error: Result<Value, String>) -> (String, bool) {
    match error {
        Ok(value) if value["committed"] == true => ("committed".to_owned(), true),
        Ok(value) if value["committed"] == false => ("refused".to_owned(), true),
        Ok(value) => (format!("unexpected: {value}"), false),
        Err(message) => (format!("threw: {message}"), false),
    }
}

/// Reference outcomes are `committed`, `refused` or `threw: <upstream error>`.
/// The native port wraps IO errors, so only the semantic class is compared.
fn outcome_matches(expected: &str, observed: &str) -> bool {
    if expected == observed {
        return true;
    }
    if expected.starts_with("threw:") {
        let expected_torn = expected.contains("incomplete journal frame");
        let expected_limit = expected.contains("journal exceeds size limit");
        return observed.starts_with("threw:")
            && expected_torn == observed.contains("incomplete journal frame")
            && expected_limit == observed.contains("journal exceeds size limit");
    }
    false
}

fn frames(value: &Value) -> Vec<Vec<u8>> {
    value["frameLines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| STANDARD.decode(line.as_str().unwrap()).unwrap())
        .collect()
}

fn assert_fixture_evidence() {
    let evidence: Value = serde_json::from_str(USAGE_EVIDENCE).unwrap();
    assert_eq!(evidence["status"], "pass", "usage reference evidence");
    assert_eq!(evidence["counts"]["scenarioPass"], evidence["counts"]["scenarios"]);
    let lifecycle: Value = serde_json::from_str(LIFECYCLE_EVIDENCE).unwrap();
    assert_eq!(lifecycle["status"], "pass", "lifecycle reference evidence");
    assert_eq!(lifecycle["counts"]["cases"], lifecycle["cases"].as_array().unwrap().len());
}

#[test]
fn recorded_usage_journals_replay_byte_identically_through_the_native_port() {
    assert_fixture_evidence();
    let fixture: Value = serde_json::from_str(USAGE_FIXTURE).unwrap();
    assert_eq!(fixture["schema"], "rpi-cursor-c-usage-fixture/1");
    let scenarios = fixture["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 20, "C07 scenario count");
    let mut journals = 0usize;
    let mut frames_replayed = 0usize;
    for scenario in scenarios {
        assert!(
            scenario["problems"].as_array().unwrap().is_empty(),
            "scenario {} reported problems: {}",
            scenario["name"],
            scenario["problems"]
        );
        for journal in scenario["journals"].as_array().unwrap() {
            journals += 1;
            if journal["replayable"] == false {
                // A deliberately torn tail is exercised by the lifecycle refusal case.
                continue;
            }
            let directory = scratch("usage");
            let expected = STANDARD
                .decode(journal["bytesBase64"].as_str().unwrap())
                .unwrap();
            let port = JournalPort::for_session(&directory.join("sessions.jsonl"), "fixture").unwrap();
            for frame in frames(journal) {
                frames_replayed += 1;
                let (_, digest) = snapshot(&port, "usage");
                let receipt = port
                    .request(json!({
                        "action": "append",
                        "kind": "usage",
                        "bytes": STANDARD.encode(&frame),
                        "expectedDigest": digest,
                    }))
                    .expect("append usage frame");
                assert_eq!(
                    receipt["committed"], true,
                    "scenario {} frame commit",
                    scenario["name"]
                );
                assert_eq!(receipt["durability"], "durable");
            }
            let actual = std::fs::read(journal_path(&directory, "usage")).unwrap_or_default();
            assert_eq!(
                actual, expected,
                "scenario {} journal {} bytes",
                scenario["name"], journal["name"]
            );
            let (read_back, read_digest) = snapshot(&port, "usage");
            assert_eq!(read_back, expected, "port read-back");
            assert_eq!(
                read_digest,
                format!("{:x}", Sha256::digest(&expected)),
                "port digest"
            );
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    assert!(journals > 0, "no journals recorded");
    assert!(frames_replayed >= 20, "unexpected frame count");
}

#[test]
fn closing_the_journal_port_stops_admission_and_preserves_committed_frames() {
    let directory = scratch("close");
    let port = JournalPort::for_session(&directory.join("sessions.jsonl"), "fixture").unwrap();
    let path = journal_path(&directory, "usage");
    for record in [
        json!({"version":1,"id":"a","kind":"start"}),
        json!({"version":1,"id":"b","kind":"raw"}),
    ] {
        let (_, digest) = snapshot(&port, "usage");
        let frame = format!("{}\n", serde_json::to_string(&record).unwrap());
        let receipt = port
            .request(json!({
                "action": "append",
                "kind": "usage",
                "bytes": STANDARD.encode(frame.as_bytes()),
                "expectedDigest": digest,
            }))
            .expect("append");
        assert_eq!(receipt["committed"], true);
    }
    // Leave an abandoned reader open, as an interrupted worker would.
    let opened = port
        .request(json!({"action": "open", "kind": "usage"}))
        .unwrap();
    let reader = opened["reader"].as_str().unwrap().to_owned();
    let committed = std::fs::read(&path).unwrap();
    assert_eq!(committed.iter().filter(|byte| **byte == b'\n').count(), 2);

    // Stop admission: the port refuses new durability work and releases readers.
    port.close();
    let refused = port.request(json!({"action": "open", "kind": "usage"}));
    assert!(refused.unwrap_err().contains("closed"));
    let refused = port.request(json!({
        "action": "append",
        "kind": "usage",
        "bytes": STANDARD.encode(b"{}\n"),
        "expectedDigest": format!("{:x}", Sha256::digest(&committed)),
    }));
    assert!(refused.unwrap_err().contains("closed"));
    let refused = port.request(json!({"action": "next", "reader": reader}));
    assert!(refused.is_err(), "abandoned reader must be released");

    // The committed frames are preserved byte-for-byte.
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn crashed_usage_writer_is_refused_and_cloud_writer_repairs_the_boundary() {
    let fixture: Value = serde_json::from_str(LIFECYCLE_FIXTURE).unwrap();
    assert_eq!(fixture["schema"], "rpi-cursor-c09-lifecycle-fixture/1");
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8, "lifecycle case count");
    assert_eq!(
        cases.iter().filter(|case| case["kind"] == "usage").count(),
        4
    );
    assert_eq!(
        cases.iter().filter(|case| case["kind"] == "cloud").count(),
        4
    );
    for case in cases {
        let directory = scratch("lifecycle");
        let kind = case["kind"].as_str().unwrap();
        let path = journal_path(&directory, kind);
        if let Some(prefix) = case["prefixBase64"].as_str() {
            std::fs::write(&path, STANDARD.decode(prefix).unwrap()).unwrap();
        }
        let port = JournalPort::for_session(&directory.join("sessions.jsonl"), "fixture").unwrap();
        for operation in case["ops"].as_array().unwrap() {
            let expected = operation["outcome"].as_str().unwrap();
            let observed = if kind == "usage" {
                let frame = STANDARD
                    .decode(operation["frameBase64"].as_str().unwrap())
                    .unwrap();
                let (_, digest) = snapshot(&port, "usage");
                outcome(port.request(json!({
                    "action": "append",
                    "kind": "usage",
                    "bytes": STANDARD.encode(&frame),
                    "expectedDigest": digest,
                })))
            } else {
                let frame = STANDARD
                    .decode(operation["frameBase64"].as_str().unwrap())
                    .unwrap();
                outcome(port.request(json!({
                    "action": "append",
                    "kind": "cloud",
                    "bytes": STANDARD.encode(&frame),
                })))
            };
            assert!(
                outcome_matches(expected, &observed.0),
                "case {}: expected {expected}, observed {}",
                case["name"],
                observed.0
            );
            let expected_bytes = STANDARD
                .decode(operation["bytesAfterBase64"].as_str().unwrap())
                .unwrap();
            let actual = std::fs::read(&path).unwrap_or_default();
            assert_eq!(
                actual, expected_bytes,
                "case {} bytes after operation",
                case["name"]
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(&actual)),
                operation["bytesAfterSha256"].as_str().unwrap(),
                "case {} byte hash",
                case["name"]
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
