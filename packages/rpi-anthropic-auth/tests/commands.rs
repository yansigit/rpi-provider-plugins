//! The two owned commands render from values alone: no host, network or clock.

use rpi_anthropic_auth::billing::VersionSettings;
use rpi_anthropic_auth::commands::{
    self, Status, SHOULD_SHAPE_PATTERN, STATUS_COMMAND, USAGE_COMMAND,
};
use rpi_anthropic_auth::version::{LearnedFloor, CLAUDE_CODE_VERSION_ENV};
use rpi_anthropic_auth::{transport, version};
use std::sync::OnceLock;

const OAUTH_TOKEN: &str = "sk-ant-oat01-abcdefghijklmnop";
const EXPIRES: i64 = 1_704_164_645_000; // 2024-01-02T03:04:05Z

fn settings() -> &'static VersionSettings {
    static SETTINGS: OnceLock<VersionSettings> = OnceLock::new();
    SETTINGS.get_or_init(|| VersionSettings {
        pinned: version::CLAUDE_CODE_VERSION.to_string(),
        has_override: false,
    })
}

fn floor() -> &'static LearnedFloor {
    static FLOOR: OnceLock<LearnedFloor> = OnceLock::new();
    FLOOR.get_or_init(LearnedFloor::default)
}

fn status<'a>(access: Option<&'a str>, shaped: &'a [String]) -> Status<'a> {
    Status {
        settings: settings(),
        floor: floor(),
        access,
        source: None,
        expires: None,
        shaped,
    }
}

#[test]
fn the_commands_are_named_the_way_the_host_registers_them() {
    assert_eq!(STATUS_COMMAND, "anthropic-auth-status");
    assert_eq!(USAGE_COMMAND, "anthropic-usage");
    assert!(!commands::STATUS_DESCRIPTION.is_empty());
    assert!(!commands::USAGE_DESCRIPTION.is_empty());
}

#[test]
fn numbered_anthropic_accounts_are_shaped() {
    assert_eq!(SHOULD_SHAPE_PATTERN, r"^anthropic(-\d+)?$");
    for provider in ["anthropic", "anthropic-2", "anthropic-31"] {
        assert!(commands::should_shape(provider), "{provider}");
    }
    for provider in [
        "anthropic-beta",
        "Anthropic",
        "my-anthropic",
        "anthropic-",
        "anthropic-2-extra",
        "openai",
        "",
    ] {
        assert!(!commands::should_shape(provider), "{provider}");
    }
}

#[test]
fn without_a_credential_the_status_points_at_login() {
    let shaped = Vec::new();
    let text = commands::status(&status(None, &shaped), 0);
    assert!(
        text.contains("  auth: none — run /login and choose Claude Pro/Max"),
        "{text}"
    );
    assert!(
        text.contains("  transport: anthropic-messages → https://api.anthropic.com/v1/messages"),
        "{text}"
    );
    assert!(text.contains(transport::API_KEY_BETA), "{text}");
    assert!(!text.contains("token:"), "{text}");
    assert!(!text.contains("expiry:"), "{text}");
}

#[test]
fn the_status_reports_the_credential_without_printing_it() {
    let shaped = vec!["anthropic".to_string(), "anthropic-2".to_string()];
    let mut state = status(Some(OAUTH_TOKEN), &shaped);
    state.source = Some("stored credential");
    state.expires = Some(EXPIRES);
    let text = commands::status(&state, EXPIRES - 5_400_000);
    assert!(
        text.contains("  auth: OAuth token (Claude Pro/Max) · source: stored credential"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "  token: present (oauth, {} characters)",
            OAUTH_TOKEN.chars().count()
        )),
        "{text}"
    );
    assert!(
        text.contains("  expiry: 2024-01-02T03:04:05Z (in 1h 30m)"),
        "{text}"
    );
    assert!(
        text.contains(
            "  betas: claude-code-20250219, oauth-2025-04-20, interleaved-thinking-2025-05-14"
        ),
        "{text}"
    );
    assert!(
        text.contains("  shaped providers (^anthropic(-\\d+)?$): anthropic, anthropic-2"),
        "{text}"
    );
    assert!(
        !text.contains(OAUTH_TOKEN),
        "the token must never be printed"
    );
}

#[test]
fn an_expired_credential_says_so() {
    let shaped = Vec::new();
    let mut state = status(Some(OAUTH_TOKEN), &shaped);
    state.expires = Some(EXPIRES);
    let text = commands::status(&state, EXPIRES + 1_000);
    assert!(text.contains("(expired)"), "{text}");
}

#[test]
fn an_api_key_gets_the_narrower_beta_set() {
    let shaped = Vec::new();
    let text = commands::status(&status(Some("sk-ant-api03-key"), &shaped), 0);
    assert!(text.contains("  auth: API key"), "{text}");
    assert!(
        text.contains(&format!(
            "  token: present (api key, {} characters)",
            "sk-ant-api03-key".chars().count()
        )),
        "{text}"
    );
    assert!(
        text.contains("  betas: interleaved-thinking-2025-05-14"),
        "{text}"
    );
    assert!(
        text.contains("  expiry: unknown (held by the host credential store)"),
        "{text}"
    );
}

#[test]
fn the_claude_code_version_shows_its_pin_and_any_learned_floor() {
    let shaped = Vec::new();
    let text = commands::status(&status(None, &shaped), 0);
    assert!(
        text.contains(&format!(
            "  claude code: {} (pin)",
            version::CLAUDE_CODE_VERSION
        )),
        "{text}"
    );

    let floor = LearnedFloor::default();
    floor.learn("2.1.300");
    let state = Status {
        settings: &VersionSettings {
            pinned: "2.1.280".to_string(),
            has_override: true,
        },
        floor: &floor,
        access: None,
        source: None,
        expires: None,
        shaped: &shaped,
    };
    let text = commands::status(&state, 0);
    assert!(
        text.contains(&format!(
            "  claude code: 2.1.280 ({CLAUDE_CODE_VERSION_ENV} override) → 2.1.300 (learned floor)"
        )),
        "{text}"
    );
}

#[test]
fn usage_needs_an_oauth_token() {
    assert_eq!(commands::usage_report(OAUTH_TOKEN), Ok(()));
    assert_eq!(
        commands::usage_report("sk-ant-api03-key"),
        Err("Run /login and choose Claude Pro/Max to view usage.")
    );
}
