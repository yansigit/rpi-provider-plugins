//! The commands this plugin owns: `/anthropic-auth-status` and
//! `/anthropic-usage`.
//!
//! Both are pure renderers — the C-ABI handler in `native` resolves what the host
//! knows and hands the values here, so every line below is testable without a
//! host, a network or a clock.
use crate::version::{self, LearnedFloor};
use crate::{billing, prompt, transport};
use regex::Regex;
use std::sync::OnceLock;

pub const STATUS_COMMAND: &str = "anthropic-auth-status";
pub const STATUS_DESCRIPTION: &str =
    "Claude Pro/Max: transport, credential, Claude Code version and shaped providers";
pub const USAGE_COMMAND: &str = "anthropic-usage";
pub const USAGE_DESCRIPTION: &str = "Claude Pro/Max: remaining 5-hour and weekly usage";

/// Pi registers a numbered duplicate when one account needs its own model list,
/// so shaping and status must recognize `anthropic-2`, `anthropic-3`, …
pub const SHOULD_SHAPE_PATTERN: &str = r"^anthropic(-\d+)?$";

pub fn should_shape(provider: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(SHOULD_SHAPE_PATTERN).expect("the shaped provider pattern compiles")
        })
        .is_match(provider)
}

/// What the host exposed about the credential in force.
pub struct Status<'a> {
    pub settings: &'a billing::VersionSettings,
    pub floor: &'a LearnedFloor,
    /// The resolved API key or OAuth access token, never printed.
    pub access: Option<&'a str>,
    pub source: Option<&'a str>,
    /// Expiry in epoch milliseconds, when this session saw the exchange that
    /// minted the credential.
    pub expires: Option<i64>,
    /// Provider ids the host catalog reports as matching [`SHOULD_SHAPE_PATTERN`].
    pub shaped: &'a [String],
}

pub fn status(state: &Status, now_ms: i64) -> String {
    let mut lines = vec!["Claude Pro/Max auth for anthropic".to_string()];
    lines.push(format!(
        "  transport: {} → {}{}",
        transport::API,
        transport::BASE_URL,
        transport::MESSAGES_PATH
    ));
    let oauth = state
        .access
        .is_some_and(prompt::is_anthropic_oauth_access_token);
    lines.push(match (state.access, oauth) {
        (None, _) => "  auth: none — run /login and choose Claude Pro/Max".to_string(),
        (Some(_), true) => format!(
            "  auth: OAuth token (Claude Pro/Max){}",
            source_suffix(state.source)
        ),
        (Some(_), false) => format!("  auth: API key{}", source_suffix(state.source)),
    });
    if let Some(access) = state.access {
        // Presence, family and length only: the value is a bearer credential.
        let family = if oauth { "oauth" } else { "api key" };
        lines.push(format!(
            "  token: present ({family}, {} characters)",
            access.chars().count()
        ));
        lines.push(match state.expires {
            Some(expires) => format!(
                "  expiry: {} ({})",
                iso_utc(expires),
                relative(expires - now_ms)
            ),
            None => "  expiry: unknown (held by the host credential store)".to_string(),
        });
    }
    lines.push(format!(
        "  betas: {}",
        if oauth {
            transport::REQUIRED_BETAS.join(", ")
        } else {
            transport::API_KEY_BETA.to_string()
        }
    ));
    let pinned = &state.settings.pinned;
    let billing = state.floor.apply_to(pinned);
    let pin = if state.settings.has_override {
        format!("{pinned} ({} override)", version::CLAUDE_CODE_VERSION_ENV)
    } else {
        format!("{pinned} (pin)")
    };
    let learned = if billing == *pinned {
        String::new()
    } else {
        format!(" → {billing} (learned floor)")
    };
    lines.push(format!("  claude code: {pin}{learned}"));
    let shaped = if state.shaped.is_empty() {
        "anthropic".to_string()
    } else {
        state.shaped.join(", ")
    };
    lines.push(format!(
        "  shaped providers ({SHOULD_SHAPE_PATTERN}): {shaped}"
    ));
    lines.join("\n")
}

fn source_suffix(source: Option<&str>) -> String {
    match source.filter(|source| !source.is_empty()) {
        Some(source) => format!(" · source: {source}"),
        None => String::new(),
    }
}

/// Usage needs an OAuth token; an API key cannot read these endpoints.
pub fn usage_report(access: &str) -> Result<(), &'static str> {
    if prompt::is_anthropic_oauth_access_token(access) {
        Ok(())
    } else {
        Err("Run /login and choose Claude Pro/Max to view usage.")
    }
}

fn relative(delta_ms: i64) -> String {
    if delta_ms <= 0 {
        return "expired".to_string();
    }
    let minutes = delta_ms / 60_000;
    let hours = minutes / 60;
    if hours >= 1 {
        format!("in {hours}h {}m", minutes % 60)
    } else if minutes >= 1 {
        format!("in {minutes}m")
    } else {
        "in under a minute".to_string()
    }
}

/// RFC 3339 UTC, computed from the epoch so the crate needs no date dependency.
fn iso_utc(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// The inverse of [`crate::usage`]'s `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}
