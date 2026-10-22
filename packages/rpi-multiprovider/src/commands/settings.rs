//! `extensions/settings-ui.ts`, projected onto the native `settings`, `select`
//! and `input` dialog controls (`pi-rust/docs/provider-parity/native-dialogs.md`).
//!
//! The upstream module builds Pi TUI components; the port builds the declarative
//! controls a native client renders. Theme colors are dropped (the native
//! controls carry plain text), `markDrillIn` is preserved so drill-in rows end
//! with `›`, and `formatMs` keeps JavaScript's number-to-string formatting.
use crate::types::trim;
use serde_json::{json, Value};

/// `BOOLEANS` from `settings-ui.ts`.
pub const BOOLEANS: [&str; 2] = ["true", "false"];

/// JavaScript's number-to-string for the values the settings rows carry.
pub fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// JavaScript's `Number(text)`: `""` is `0`, `0x`/`0b`/`0o` prefixes are
/// integers, anything else that is not a finite decimal is `NaN`.
pub fn js_number_parse(text: &str) -> f64 {
    let value = trim(text);
    if value.is_empty() {
        return 0.0;
    }
    let (radix, digits) = if let Some(digits) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
        (16, digits)
    } else if let Some(digits) = value.strip_prefix("0b").or_else(|| value.strip_prefix("0B")) {
        (2, digits)
    } else if let Some(digits) = value.strip_prefix("0o").or_else(|| value.strip_prefix("0O")) {
        (8, digits)
    } else {
        (0, value)
    };
    if radix != 0 {
        return i64::from_str_radix(digits, radix)
            .map(|number| number as f64)
            .unwrap_or(f64::NAN);
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .unwrap_or(f64::NAN)
}

/// `formatMs` from `settings-ui.ts`.
pub fn format_ms(ms: f64) -> String {
    if ms < 1_000.0 {
        format!("{}ms", js_number(ms))
    } else if ms < 60_000.0 {
        format!("{}s", js_number(ms / 1_000.0))
    } else if ms < 3_600_000.0 {
        format!("{}m", js_number(ms / 60_000.0))
    } else {
        format!("{}h", js_number(ms / 3_600_000.0))
    }
}

/// One `SettingItem` row of a native `settings` control.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub current_value: String,
    pub description: Option<String>,
    pub values: Vec<String>,
    pub submenu: bool,
}

impl SettingItem {
    /// `setting(id, label, currentValue)`.
    pub fn new(id: impl Into<String>, label: impl Into<String>, current_value: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            current_value: current_value.into(),
            description: None,
            values: Vec::new(),
            submenu: false,
        }
    }
    /// `setting(..., { description })`.
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
    /// `setting(..., { values })`: the row cycles inline.
    pub fn cycling(mut self, values: &[&str]) -> Self {
        self.values = values.iter().map(|value| (*value).to_owned()).collect();
        self
    }
    /// `setting(..., { values })` with computed values (the scheduler options).
    pub fn cycling_owned(mut self, values: Vec<String>) -> Self {
        self.values = values;
        self
    }
    /// `setting(..., { submenu })`: the row drills into a submenu.
    pub fn drill_in(mut self) -> Self {
        self.submenu = true;
        self
    }
    pub fn to_value(&self) -> Value {
        let mut row = json!({"id": self.id, "label": self.label, "value": self.current_value});
        if let Some(description) = &self.description {
            row["description"] = json!(description);
        }
        if !self.values.is_empty() {
            row["values"] = json!(self.values);
        }
        if self.submenu {
            row["submenu"] = json!(true);
        }
        row
    }
}

/// `markDrillIn`: rows that open a submenu are marked with `›`.
pub fn mark_drill_in(items: &mut [SettingItem]) {
    for item in items.iter_mut() {
        if item.submenu && !item.label.ends_with('›') {
            item.label = format!("{} ›", item.label);
        }
    }
}

/// The native `settings` control for one screen of a settings list.
pub fn settings_control(
    title: &str,
    description: &str,
    items: &[SettingItem],
    query: &str,
    selected: Option<&str>,
) -> Value {
    let mut control = json!({
        "kind": "settings",
        "title": title,
        "description": description,
        "searchable": true,
        "settings": items.iter().map(SettingItem::to_value).collect::<Vec<_>>(),
        "query": query,
    });
    if let Some(selected) = selected {
        control["selected"] = json!(selected);
    }
    control
}

/// The native `select` control for a `SelectSubmenu`.
pub fn select_control(
    title: &str,
    description: Option<&str>,
    options: &[(String, String)],
    selected: usize,
) -> Value {
    let mut control = json!({
        "kind": "select",
        "title": title,
        "choices": options
            .iter()
            .map(|(value, label)| json!({"value": value, "label": label}))
            .collect::<Vec<_>>(),
    });
    if let Some(description) = description {
        control["description"] = json!(description);
    }
    if let Some((value, _)) = options.get(selected) {
        control["selected"] = json!(value);
    }
    control
}

/// The native `input` control for `StringInputSubmenu` and
/// `IntegerInputSubmenu`. The host trims and enforces `integerMin`.
pub fn input_control(
    title: &str,
    description: &str,
    initial_value: &str,
    integer_min: Option<u64>,
) -> Value {
    let mut control = json!({
        "kind": "input",
        "title": title,
        "description": description,
        "initialValue": initial_value,
        "trim": true,
    });
    if let Some(min) = integer_min {
        control["integerMin"] = json!(min);
    }
    control
}

/// The value of a reply to an `input` control: `None` on cancel.
pub fn input_reply(reply: &Value) -> Result<Option<String>, String> {
    if reply.is_null() {
        return Ok(None);
    }
    if reply["type"] != "input" {
        return Err("unexpected reply to an input control".into());
    }
    reply["value"]
        .as_str()
        .map(|value| Some(trim(value).to_owned()))
        .ok_or_else(|| "input reply requires text".to_owned())
}

/// `IntegerInputSubmenu`'s validation, mirrored so a reply that slipped past the
/// host keeps the dialog open instead of being applied.
pub fn integer_reply(reply: &Value, min: u64) -> Result<Option<f64>, String> {
    let Some(text) = input_reply(reply)? else {
        return Ok(None);
    };
    let parsed = if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        text.parse::<u64>().ok().map(|number| number as f64)
    } else {
        None
    };
    match parsed {
        Some(number) if number >= min as f64 && number <= 9_007_199_254_740_991.0 => Ok(Some(number)),
        _ => Err(format!("Enter an integer greater than or equal to {min}.")),
    }
}

/// The value of a reply to a `select` control: `None` on cancel.
pub fn select_reply(reply: &Value) -> Result<Option<String>, String> {
    if reply.is_null() {
        return Ok(None);
    }
    if reply["type"] != "select" {
        return Err("unexpected reply to a select control".into());
    }
    reply["values"]
        .as_array()
        .filter(|values| values.len() == 1)
        .and_then(|values| values[0].as_str())
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| "select reply must hold one choice".to_owned())
}

/// The `id` of a reply to a `settings` control, when the reply is a change or
/// an open. `None` on cancel.
pub fn settings_reply(reply: &Value) -> Result<Option<(String, String)>, String> {
    if reply.is_null() {
        return Ok(None);
    }
    match reply["type"].as_str() {
        Some("change") => {
            let id = reply["id"]
                .as_str()
                .ok_or_else(|| "setting change requires an ID".to_owned())?;
            let value = reply["value"]
                .as_str()
                .ok_or_else(|| "setting change requires a value".to_owned())?;
            Ok(Some((id.to_owned(), value.to_owned())))
        }
        Some("open") => {
            let id = reply["id"]
                .as_str()
                .ok_or_else(|| "setting open requires an ID".to_owned())?;
            Ok(Some((id.to_owned(), String::new())))
        }
        _ => Err("unexpected reply to a settings control".into()),
    }
}

/// Whether a `settings` reply opened a submenu.
pub fn settings_opened(reply: &Value) -> bool {
    reply["type"] == "open"
}
