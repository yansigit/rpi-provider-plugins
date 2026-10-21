//! Only projected menu data crosses the host boundary; original secret-shape scrubbing remains.
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
const WS: &str =
    r"\x09-\x0d \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
pub fn text(input: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(||vec![
        Regex::new(&format!(r"(?-u:\b)(?:Bearer[{WS}]+[A-Za-z0-9_.-]+|sk-[A-Za-z0-9_-]+|eyJ[A-Za-z0-9_.-]+)(?-u:\b)|ckh_[A-Za-z0-9_-]{{20,}}")).unwrap(),
        Regex::new(&format!(r#"(?i-u:\b[\w-]*(?:secret|token|password|passwd|api[_-]?key|authorization|verifier))["']?[{WS}]*[:=][{WS}]*["']?(?:(?i-u:Bearer|Basic)[{WS}]+)?[^{WS}"'&,;}}]+"#)).unwrap(),
        Regex::new(r"(?-u)\b[0-9a-fA-F]{32,}\b").unwrap(),
    ]).iter().fold(input.into(),|value,pattern|pattern.replace_all(&value,"***REDACTED***").into_owned())
}
fn secret_name(name: &str) -> bool {
    let normalized = name.to_lowercase().replace(['-', '_'], "");
    matches!(
        normalized.as_str(),
        "access" | "refresh" | "apikey" | "password" | "authheader" | "credential" | "credentials"
    ) || ["token", "key", "secret"]
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}
pub fn scrub(value: &mut Value) {
    match value {
        Value::String(v) => *v = text(v),
        Value::Array(v) => v.iter_mut().for_each(scrub),
        Value::Object(v) => {
            v.retain(|key, _| !secret_name(key));
            v.values_mut().for_each(scrub);
        }
        _ => {}
    }
}
pub fn folder_refusal(error: &str) -> Option<String> {
    static CLIENT: OnceLock<Regex> = OnceLock::new();
    static REASON: OnceLock<Regex> = OnceLock::new();
    static EMBEDDED: OnceLock<Regex> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        Regex::new(r"(?s)^enrollment token folder (.+) is not private: (.+)$").unwrap()
    });
    let reason = REASON.get_or_init(|| Regex::new(&format!(r"^(?:foreign owner|world-writable ancestor|group-writable ancestor|could not (?:read|parse) [^{WS}]+ to establish a user-private group)")).unwrap());
    let embedded = EMBEDDED.get_or_init(|| Regex::new(r"(?s)^Claustrum enrollment path has a writable ancestor: (.+?)\. Another user could replace it\.").unwrap());
    let matched = client
        .captures(error)
        .filter(|m| reason.is_match(&m[2]))
        .or_else(|| embedded.captures(error));
    matched.map(|m| format!("The vault key can't be saved because the folder {} can be changed by other users. Make that folder owned by you and not writable by your group or by others, then try again.", &m[1]))
}
