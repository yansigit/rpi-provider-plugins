//! `/anthropic-usage`: what the Claude Pro/Max account has left.
//!
//! Anthropic exposes two undocumented read-only endpoints for OAuth accounts:
//! `/api/oauth/usage` for the windows and `/api/oauth/profile` for the plan. They
//! are not part of the Messages API, so every shape here is defensive — an
//! unrecognized field degrades to a missing row rather than a failed command.
use serde_json::Value;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
pub const REQUEST_TIMEOUT_MS: u64 = 10_000;
/// The usage endpoints accept the OAuth beta only; the Messages betas are not
/// valid here.
pub const USAGE_BETA: &str = "oauth-2025-04-20";

const BAR_WIDTH: usize = 20;

/// Rows are ranked so the account's most urgent window is always first.
fn row_rank(label: &str) -> u8 {
    match label {
        "5-hour" => 0,
        "Weekly" => 1,
        "Extra usage" => 4,
        _ if label.starts_with("Weekly (") => 2,
        _ => 3,
    }
}

/// `seven_day_opus` → `Weekly (Opus only)`; `oauth` is an acronym, not a word.
fn words(segment: &str) -> String {
    segment
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            if word.eq_ignore_ascii_case("oauth") {
                "OAuth".to_string()
            } else {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn label_for_window(key: &str) -> Option<String> {
    Some(match key {
        "five_hour" => "5-hour".to_string(),
        "seven_day" => "Weekly".to_string(),
        // Anthropic's internal code name for a model-scoped weekly window.
        "iguana_necktie" => "Weekly (Fable only)".to_string(),
        _ => {
            let segment = key.strip_prefix("seven_day_")?;
            format!("Weekly ({} only)", words(segment))
        }
    })
}

/// Percentages arrive as 0–100 fractions; the bar is a fixed 20 cells.
pub fn format_percent(percent: f64) -> String {
    let percent = percent.clamp(0.0, 100.0);
    let filled = ((percent / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let bar = "█".repeat(filled) + &"░".repeat(BAR_WIDTH - filled);
    let rounded = (percent * 10.0).round() / 10.0;
    format!("[{bar}] {rounded}% used")
}

/// `resets_at` is RFC 3339; a value in the past reads as "now".
pub fn format_reset(resets_at: Option<&str>, now_ms: i64) -> String {
    let Some(resets_at) = resets_at else {
        return String::new();
    };
    let Some(epoch_ms) = parse_rfc3339_ms(resets_at) else {
        return String::new();
    };
    let remaining_ms = epoch_ms - now_ms;
    if remaining_ms <= 60_000 {
        return " · resets now".to_string();
    }
    let minutes = remaining_ms / 60_000;
    let hours = minutes / 60;
    let days = hours / 24;
    let remaining = if days >= 1 {
        format!("{days}d {}h", hours % 24)
    } else if hours >= 1 {
        format!("{hours}h {}m", minutes % 60)
    } else {
        format!("{minutes}m")
    };
    format!(" · resets in {remaining}")
}

/// Parses the subset of RFC 3339 the usage endpoint emits (`YYYY-MM-DDTHH:MM:SSZ`
/// with an optional fractional part), returning epoch milliseconds.
fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |start: usize, len: usize| -> Option<i64> {
        text.get(start..start + len)?.parse::<i64>().ok()
    };
    let (year, month, day) = (number(0, 4)?, number(5, 2)?, number(8, 2)?);
    let (hour, minute, second) = (number(11, 2)?, number(14, 2)?, number(17, 2)?);
    let offset = match bytes.get(19) {
        Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = number(20, 2)?;
            let minutes = number(23, 2)?;
            let magnitude = (hours * 3600 + minutes * 60) as i64;
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(seconds * 1000)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), which keeps the crate free of a date dependency.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn format_money(amount: f64, decimal_places: Option<u64>, currency: Option<&str>) -> String {
    let places = match decimal_places {
        Some(places @ 0..=4) => places as usize,
        _ => 2,
    };
    let symbol = match currency.unwrap_or("USD") {
        "USD" => "$".to_string(),
        other => format!("{other} "),
    };
    format!("{symbol}{amount:.places$}")
}

struct Row {
    label: String,
    percent: f64,
    reset: String,
}

fn rows(usage: &Value, now_ms: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(limits) = usage["limits"].as_array() {
        for limit in limits {
            let label = limit_label(limit);
            let percent = limit["percent"]
                .as_f64()
                .or_else(|| limit["utilization"].as_f64());
            let Some(percent) = percent else {
                continue;
            };
            rows.push(Row {
                label,
                percent,
                reset: format_reset(limit["resets_at"].as_str(), now_ms),
            });
        }
    }
    if rows.is_empty() {
        if let Some(fields) = usage.as_object() {
            for (key, window) in fields {
                let Some(label) = label_for_window(key) else {
                    continue;
                };
                let Some(percent) = window["utilization"]
                    .as_f64()
                    .or_else(|| window["percent"].as_f64())
                else {
                    continue;
                };
                rows.push(Row {
                    label,
                    percent,
                    reset: format_reset(window["resets_at"].as_str(), now_ms),
                });
            }
        }
    }
    if usage["extra_usage"]["is_enabled"] == true {
        let extra = &usage["extra_usage"];
        let percent = extra["utilization"]
            .as_f64()
            .or_else(|| extra["percent"].as_f64())
            .unwrap_or(0.0);
        let spent = format_money(
            extra["used_credits"].as_f64().unwrap_or(0.0),
            extra["decimal_places"].as_u64(),
            extra["currency"].as_str(),
        );
        let limit = format_money(
            extra["monthly_limit"].as_f64().unwrap_or(0.0),
            extra["decimal_places"].as_u64(),
            extra["currency"].as_str(),
        );
        rows.push(Row {
            label: "Extra usage".to_string(),
            percent,
            reset: format!(" · {spent} of {limit}"),
        });
    }
    rows.sort_by_key(|row| row_rank(&row.label));
    rows
}

fn limit_label(limit: &Value) -> String {
    let kind = limit["kind"].as_str().unwrap_or_default();
    let group = limit["group"].as_str().unwrap_or_default();
    let base = match kind {
        "session" => "5-hour".to_string(),
        "weekly" => "Weekly".to_string(),
        other if other.starts_with("weekly") => "Weekly".to_string(),
        other => words(other),
    };
    let mut suffix = String::new();
    if let Some(model) = limit["scope"]["model"]["display_name"].as_str() {
        suffix = model.to_string();
    } else if group == "model" {
        suffix = "model".to_string();
    }
    if let Some(surface) = limit["scope"]["surface"].as_str() {
        let surface = words(surface);
        suffix = if suffix.is_empty() {
            surface
        } else {
            format!("{suffix} · {surface}")
        };
    }
    if suffix.is_empty() || base == "Weekly" && suffix == "Weekly" {
        base
    } else if base == "Weekly" {
        format!("Weekly ({suffix} only)")
    } else {
        format!("{base} ({suffix})")
    }
}

/// `organization_type` is `claude_max`, `claude_pro`, …; the tier carries a
/// multiplier for Max (`default_claude_max_20x`).
pub fn plan_from_profile(profile: &Value) -> Option<String> {
    let organization = profile["organization"]["organization_type"]
        .as_str()
        .map(|kind| kind.trim_start_matches("claude_").to_string());
    let tier = profile["organization"]["rate_limit_tier"].as_str();
    let kind = organization.or_else(|| {
        let tier = tier?;
        ["max", "pro", "team", "enterprise", "free"]
            .into_iter()
            .find(|name| tier.contains(name))
            .map(str::to_string)
    })?;
    let multiplier = tier
        .and_then(|tier| multiplier(tier))
        .map(|value| format!(" {value}"))
        .unwrap_or_default();
    Some(format!("{}{multiplier}", words(&kind)))
}

/// `…max_20x…` → `20x`.
fn multiplier(tier: &str) -> Option<String> {
    let start = tier.find("max_")? + 4;
    let digits: String = tier[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let rest = &tier[start + digits.len()..];
    if digits.is_empty() || !rest.starts_with('x') {
        return None;
    }
    Some(format!("{digits}x"))
}

/// The whole report. `plan` is `None` when the profile call failed, which is a
/// separate failure from a broken usage response.
pub fn render(usage: &Value, plan: Result<Option<String>, String>, now_ms: i64) -> String {
    let mut lines = vec!["Claude usage".to_string()];
    match plan {
        Ok(Some(plan)) => lines.push(format!("  Plan: {plan}")),
        Ok(None) => lines.push("  Plan: unavailable".to_string()),
        Err(error) => lines.push(format!("  Plan: unavailable ({error})")),
    }
    let rows = rows(usage, now_ms);
    if rows.is_empty() {
        lines.push("  No Claude usage data available.".to_string());
        return lines.join("\n");
    }
    let width = rows.iter().map(|row| row.label.len()).max().unwrap_or(0);
    for row in rows {
        lines.push(format!(
            "  {:<width$}  {}{}",
            row.label,
            format_percent(row.percent),
            row.reset
        ));
    }
    lines.join("\n")
}

/// One read-only GET against the OAuth endpoints.
async fn get(client: &reqwest::Client, url: &str, access_token: &str) -> Result<Value, String> {
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(REQUEST_TIMEOUT_MS),
        client
            .get(url)
            .header("authorization", format!("Bearer {access_token}"))
            .header("anthropic-beta", USAGE_BETA)
            .header("accept", "application/json")
            .header(
                "user-agent",
                crate::transport::USER_AGENT_PREFIX.to_string() + "usage",
            )
            .send(),
    )
    .await
    .map_err(|_| "Anthropic usage request timed out.".to_string())?
    .map_err(|_| "Could not reach Anthropic usage API. Check your connection.".to_string())?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|_| "Could not parse Anthropic usage response.".to_string())?;
    if !(200..300).contains(&status) {
        let retry = if status == 401 || status == 403 {
            " Try /login again."
        } else {
            ""
        };
        return Err(format!(
            "Anthropic usage request failed (HTTP {status}).{retry}"
        ));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| "Could not parse Anthropic usage response.".to_string())?;
    if !value.is_object() {
        return Err("Invalid Anthropic usage response.".to_string());
    }
    Ok(value)
}

/// Fetches the windows and the plan; the plan is best effort.
pub async fn fetch(client: &reqwest::Client, access_token: &str) -> Result<String, String> {
    let usage = get(client, USAGE_URL, access_token).await?;
    let plan = match get(client, PROFILE_URL, access_token).await {
        Ok(profile) => Ok(plan_from_profile(&profile)),
        Err(error) => Err(error),
    };
    Ok(render(&usage, plan, crate::transport::now_ms()))
}
