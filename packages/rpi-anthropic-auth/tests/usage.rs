//! Usage formatting: percentages, reset countdowns, plan names and the report.

use rpi_anthropic_auth::usage::{format_percent, format_reset, plan_from_profile, render};
use serde_json::json;

const NOW: i64 = 1_704_067_200_000; // 2024-01-01T00:00:00Z

#[test]
fn the_bar_is_twenty_cells_wide() {
    assert_eq!(format_percent(0.0), "[░░░░░░░░░░░░░░░░░░░░] 0% used");
    assert_eq!(format_percent(100.0), "[████████████████████] 100% used");
    assert_eq!(format_percent(12.5), "[███░░░░░░░░░░░░░░░░░] 12.5% used");
    // Values outside the range cannot make the bar change width.
    assert_eq!(format_percent(-5.0), "[░░░░░░░░░░░░░░░░░░░░] 0% used");
    assert_eq!(format_percent(250.0), "[████████████████████] 100% used");
}

#[test]
fn resets_read_as_a_countdown() {
    assert_eq!(format_reset(None, NOW), "");
    assert_eq!(format_reset(Some("not a date"), NOW), "");
    assert_eq!(
        format_reset(Some("2024-01-01T00:00:30Z"), NOW),
        " · resets now"
    );
    assert_eq!(
        format_reset(Some("2024-01-01T00:30:00Z"), NOW),
        " · resets in 30m"
    );
    assert_eq!(
        format_reset(Some("2024-01-01T02:05:00Z"), NOW),
        " · resets in 2h 5m"
    );
    assert_eq!(
        format_reset(Some("2024-01-06T04:00:00Z"), NOW),
        " · resets in 5d 4h"
    );
    // An offset timestamp is converted, not read as UTC.
    assert_eq!(
        format_reset(Some("2024-01-01T02:00:00+01:00"), NOW),
        " · resets in 1h 0m"
    );
}

#[test]
fn the_plan_comes_from_the_organization_or_the_tier() {
    assert_eq!(
        plan_from_profile(&json!({"organization": {"organization_type": "claude_max"}})),
        Some("Max".to_string())
    );
    assert_eq!(
        plan_from_profile(&json!({"organization": {"rate_limit_tier": "default_claude_max_20x"}})),
        Some("Max 20x".to_string())
    );
    assert_eq!(
        plan_from_profile(&json!({"organization": {"organization_type": "claude_pro"}})),
        Some("Pro".to_string())
    );
    assert_eq!(
        plan_from_profile(&json!({"organization": {"rate_limit_tier": "claude_enterprise"}})),
        Some("Enterprise".to_string())
    );
    assert_eq!(plan_from_profile(&json!({"organization": {}})), None);
    assert_eq!(plan_from_profile(&json!({})), None);
}

#[test]
fn the_report_lists_the_windows_the_endpoint_reports() {
    let usage = json!({
        "limits": [
            {
                "kind": "session",
                "group": "session",
                "percent": 25.0,
                "resets_at": "2024-01-01T02:00:00Z",
            },
            {
                "kind": "weekly",
                "group": "weekly",
                "percent": 50.0,
                "resets_at": "2024-01-06T00:00:00Z",
                "scope": {"model": {"display_name": "Opus"}},
            },
        ],
        "extra_usage": {
            "is_enabled": true,
            "utilization": 10.0,
            "used_credits": 3.5,
            "monthly_limit": 35.0,
            "currency": "USD",
            "decimal_places": 2,
        }
    });
    let text = render(&usage, Ok(Some("Max 20x".to_string())), NOW);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Claude usage");
    assert_eq!(lines[1], "  Plan: Max 20x");
    assert!(lines[2].starts_with("  5-hour"), "{}", lines[2]);
    assert!(
        lines[2].contains("[█████░░░░░░░░░░░░░░░] 25% used"),
        "{}",
        lines[2]
    );
    assert!(lines[2].ends_with(" · resets in 2h 0m"), "{}", lines[2]);
    // Weekly windows with a model scope come after the 5-hour window.
    assert!(lines[3].starts_with("  Weekly (Opus only)"), "{}", lines[3]);
    assert!(lines[4].starts_with("  Extra usage"), "{}", lines[4]);
    assert!(lines[4].ends_with(" · $3.50 of $35.00"), "{}", lines[4]);
}

#[test]
fn legacy_window_keys_still_render() {
    let usage = json!({
        "five_hour": {"utilization": 12.5, "resets_at": "2024-01-01T05:00:00Z"},
        "seven_day": {"utilization": 40.0},
        "seven_day_opus": {"utilization": 5.0},
        "iguana_necktie": {"utilization": 7.0},
    });
    let text = render(&usage, Ok(None), NOW);
    assert!(text.contains("  Plan: unavailable"), "{text}");
    assert!(text.contains("  5-hour"), "{text}");
    assert!(text.contains("  Weekly "), "{text}");
    assert!(text.contains("  Weekly (Opus only)"), "{text}");
    assert!(text.contains("  Weekly (Fable only)"), "{text}");
}

#[test]
fn a_failed_plan_lookup_is_reported_but_the_windows_still_render() {
    let usage = json!({"five_hour": {"utilization": 1.0}});
    let text = render(&usage, Err("nope".to_string()), NOW);
    assert!(text.contains("  Plan: unavailable (nope)"), "{text}");
    assert!(text.contains("  5-hour"), "{text}");
}

#[test]
fn an_empty_response_says_there_is_nothing_to_show() {
    let text = render(&json!({}), Ok(None), NOW);
    assert_eq!(
        text,
        "Claude usage\n  Plan: unavailable\n  No Claude usage data available."
    );
}
