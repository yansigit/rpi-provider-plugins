use serde_json::Value;

/// Public protocol diagnostics contain codes, never raw replies or request material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub class: &'static str,
    pub action: &'static str,
    pub unknown_class: Option<String>,
}
impl Error {
    pub fn transport(code: &str) -> Self {
        Self {
            code: code.into(),
            class: "transient",
            action: "retry",
            unknown_class: None,
        }
    }

    pub fn from_reply(response: &Value, fallback: &str) -> Self {
        let error = &response["result"]["error"];
        let raw_class = error["class"].as_str();
        let raw_code = error["code"].as_str();
        let known = matches!(
            raw_class,
            Some("transient" | "permanent" | "auth_required" | "context_overflow")
        );
        let class = if raw_code.is_some() && known {
            raw_class.unwrap()
        } else {
            "transient"
        };
        let (class, action) = match class {
            "permanent" => ("permanent", "gone"),
            "auth_required" => ("auth_required", "reauth"),
            "context_overflow" => ("context_overflow", "reduce_and_retry"),
            _ => ("transient", "retry"),
        };
        Self {
            code: raw_code.unwrap_or(fallback).into(),
            class,
            action,
            unknown_class: (!known).then(|| raw_class.unwrap_or("unknown").into()),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Claustrum credential request failed: {}", self.code)
    }
}
impl std::error::Error for Error {}
