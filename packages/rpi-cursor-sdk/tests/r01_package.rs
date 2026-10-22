//! R01 packaging invariants: sidecar asset manifest, external SDK, licenses, catalog.
//!
//! These checks are offline and need no built worker: they validate the committed
//! manifest against the tracked runtime files, the pinned SDK metadata, the
//! recorded SDK file hashes, the catalog entry and the packaging evidence.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MANIFEST: &str = include_str!("r01-package-assets.json");
const EVIDENCE: &str = include_str!("r01-package-evidence.json");
const SDK_HASHES: &str = include_str!("../distribution/cursor-sdk-1.0.37.sha256.json");
const PACKAGE_JSON: &str = include_str!("../runtime/package.json");
const PACKAGE_LOCK: &str = include_str!("../runtime/package-lock.json");
const LICENSE: &str = include_str!("../runtime/LICENSE");
const MAIN: &[u8] = include_bytes!("../runtime/main.mjs");
const CARGO: &str = include_str!("../Cargo.toml");
const CATALOG: &str = include_str!("../../../catalog/packages.json");
const REFERENCE: &str = include_str!("r01-package-reference.mjs");
const STAGING_SCRIPT: &str = include_str!("../scripts/stage-package-runtime.mjs");

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn manifest() -> Value {
    serde_json::from_str(MANIFEST).expect("asset manifest is valid JSON")
}

fn assets(manifest: &Value) -> Vec<(String, String)> {
    manifest["assets"]
        .as_array()
        .expect("assets array")
        .iter()
        .map(|item| {
            (
                item["path"].as_str().expect("asset path").to_string(),
                item["sha256"].as_str().expect("asset sha256").to_string(),
            )
        })
        .collect()
}

#[test]
fn manifest_declares_a_strict_host_compatible_sidecar_layout() {
    let manifest = manifest();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["package"], "rpi-cursor-sdk");
    assert_eq!(manifest["source"], ".package");
    let assets = assets(&manifest);
    assert_eq!(
        assets.len(),
        21,
        "the packaged runtime is one launcher, five bundles, four tracked files and ten shared files"
    );
    // The host accepts at most 128 archive entries, including the library.
    assert!(assets.len() + 1 <= 128);

    let mut folded = HashSet::new();
    for (path, sha) in &assets {
        assert!(!path.is_empty() && path.len() <= 512, "{path}");
        assert!(path.starts_with("runtime/"), "{path}");
        assert!(!path.split('/').any(|part| part.starts_with('.')), "{path}");
        assert!(
            !path.split('/').any(|part| part == "node_modules"),
            "{path}"
        );
        assert!(
            path.chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || matches!(character, '/' | '-' | '_' | '.')),
            "{path}"
        );
        assert!(
            ![".so", ".dylib", ".dll", ".pyd"]
                .iter()
                .any(|extension| path.ends_with(extension)),
            "{path}"
        );
        assert_eq!(sha.len(), 64, "{path}");
        assert!(sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert!(folded.insert(path.to_lowercase()), "duplicate asset {path}");
    }
    for required in [
        "runtime/main.mjs",
        "runtime/package.json",
        "runtime/package-lock.json",
        "runtime/LICENSE",
        "runtime/dist/rpi/peer.js",
        "runtime/dist/rpi/paths.js",
        "runtime/dist/rpi/runtime.js",
        "runtime/dist/rpi/transport-thread.js",
        "runtime/dist/cursor-sensitive-text.js",
        "runtime/shared/cursor-sensitive-text.mjs",
    ] {
        assert!(
            folded.contains(&required.to_lowercase()),
            "manifest is missing {required}"
        );
    }
    assert_eq!(
        assets
            .iter()
            .filter(|(path, _)| path.starts_with("runtime/shared/"))
            .count(),
        12
    );
}

#[test]
fn tracked_runtime_files_match_their_declared_hashes() {
    let manifest = manifest();
    let assets = assets(&manifest);
    for (path, bytes) in [
        ("runtime/main.mjs", MAIN),
        ("runtime/package.json", PACKAGE_JSON.as_bytes()),
        ("runtime/package-lock.json", PACKAGE_LOCK.as_bytes()),
        ("runtime/LICENSE", LICENSE.as_bytes()),
    ] {
        let declared = assets
            .iter()
            .find(|(name, _)| name == path)
            .unwrap_or_else(|| panic!("manifest is missing {path}"));
        assert_eq!(
            declared.1,
            sha256(bytes),
            "{path} changed without restaging the manifest"
        );
    }
}

#[test]
fn the_proprietary_sdk_is_external_and_no_asset_is_an_sdk_file() {
    let manifest = manifest();
    let assets = assets(&manifest);
    let sdk: Value = serde_json::from_str(SDK_HASHES).expect("SDK hash manifest is valid JSON");
    let packages = sdk["packages"].as_array().expect("packages");
    let sdk_package = packages
        .iter()
        .find(|package| package["name"] == "@cursor/sdk")
        .expect("@cursor/sdk hashes");
    assert_eq!(sdk_package["version"], "1.0.37");
    let forbidden: HashSet<&str> = packages
        .iter()
        .flat_map(|package| package["files"].as_array().expect("files"))
        .map(|hash| hash.as_str().expect("hash"))
        .collect();
    for (path, sha) in &assets {
        assert!(
            !forbidden.contains(sha.as_str()),
            "{path} is a byte copy of a proprietary SDK file"
        );
    }

    let package: Value = serde_json::from_str(PACKAGE_JSON).expect("package.json");
    assert_eq!(package["dependencies"]["@cursor/sdk"], "1.0.37");
    assert_eq!(package["license"], "MIT");
    assert_eq!(package["private"], true);
    let lock: Value = serde_json::from_str(PACKAGE_LOCK).expect("package-lock.json");
    assert_eq!(
        lock["packages"][""]["dependencies"]["@cursor/sdk"],
        "1.0.37"
    );
    assert!(LICENSE.starts_with("MIT License"));
}

#[test]
fn cargo_metadata_and_catalog_advertise_the_prebuilt_release() {
    assert!(CARGO.contains("[package.metadata.rpi]"));
    assert!(CARGO.contains("prebuilt = true"));
    assert!(CARGO.contains(r#"assets = ".package/runtime-assets.json""#));

    let catalog: Value = serde_json::from_str(CATALOG).expect("catalog JSON");
    let entry = catalog
        .as_array()
        .expect("catalog array")
        .iter()
        .find(|entry| entry["name"] == "rpi-cursor-sdk")
        .expect("catalog entry");
    assert_eq!(entry["version"], "0.1.6");
    assert_eq!(entry["type"], "extension");
    assert_eq!(
        entry["install"],
        "rpi install rpi-cursor-sdk --github pi-rust/rpi-package"
    );
    assert_eq!(
        entry["prebuilt_manifest"],
        "https://github.com/pi-rust/rpi-package/releases/download/rpi-cursor-sdk-v0.1.6/rpi-extension.json"
    );
}

#[test]
fn packaging_scripts_and_evidence_are_present_and_consistent() {
    assert!(STAGING_SCRIPT.contains("packages: \"external\""));
    assert!(STAGING_SCRIPT.contains("stage-package-runtime"));
    assert!(REFERENCE.contains("MAX_ENTRIES = 128"));

    let evidence: Value = serde_json::from_str(EVIDENCE).expect("evidence JSON");
    assert_eq!(evidence["status"], "pass");
    assert_eq!(evidence["ledger"], "R01");
    assert_eq!(
        evidence["assetManifest"]["sha256"],
        sha256(MANIFEST.as_bytes())
    );
    assert_eq!(evidence["assetManifest"]["assets"], 21);
    assert_eq!(evidence["assetManifest"]["entries"], 22);
    assert_eq!(evidence["assetManifest"]["hostEntryLimit"], 128);
    assert_eq!(evidence["sdk"]["vendoredInArchive"], false);
    let sdk: Value = serde_json::from_str(SDK_HASHES).expect("SDK hash manifest");
    let expected: Vec<Value> = sdk["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|package| {
            serde_json::json!({
                "name": package["name"],
                "version": package["version"],
                "files": package["files"].as_array().expect("files").len(),
            })
        })
        .collect();
    assert_eq!(evidence["sdk"]["packages"], Value::Array(expected));
    let limits = evidence["notTested"].as_array().expect("notTested");
    assert!(limits.iter().any(|item| item
        .as_str()
        .unwrap_or_default()
        .starts_with("hostBinaryInstallUninstall")));
}
