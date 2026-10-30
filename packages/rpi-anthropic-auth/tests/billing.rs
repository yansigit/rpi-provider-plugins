//! Mirrors `test/billing-header-fixtures.ts`, `test/billing-version-sync.test.ts`,
//! and the billing-header cases of `test/request-shaping.test.ts`
//! (gotgenes/pi-anthropic-auth).

use rpi_anthropic_auth::billing::{
    build_billing_header_value, get_first_user_text, payload_first_user_text,
    prepend_billing_header, BillingVersionSync, SyncResponse, VersionSettings,
    BILLING_HEADER_MARKER, BILLING_HEADER_POSITIONS, BILLING_HEADER_SALT, CLAUDE_CODE_ENTRYPOINT,
};
use rpi_anthropic_auth::version::{
    describe_recovery_hint, LearnedFloor, RecoveryHintReason, CLAUDE_CODE_VERSION,
    CLAUDE_CODE_VERSION_ENV,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;

const USER_TEXT: &str = "Summarize the repository status.";
const SHAPED_TEXT: &str = "Please summarize this repository status.";
const PI_AHEAD: &str = "2.9.9";
const PI_BEHIND: &str = "1.0.0";
const REQUIRED: &str = "2.9.0";
const REJECTION_REQUEST_ID: &str = "req_test_rejection";

fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Rebuilds the expected billing header independently of production code.
///
/// This is deliberately a second implementation of the hashing recipe rather
/// than a call into [`build_billing_header_value`]: it is the oracle that would
/// catch a change to that recipe, so importing the function under test would make
/// every assertion vacuous.
fn expected_billing_header(message_text: &str, version: &str) -> String {
    let cch = sha256_hex(message_text);
    let sampled: String = BILLING_HEADER_POSITIONS
        .iter()
        .map(|index| message_text.chars().nth(*index).unwrap_or('0'))
        .collect();
    let suffix = sha256_hex(&format!("{BILLING_HEADER_SALT}{sampled}{version}"));

    format!(
        "{BILLING_HEADER_MARKER} cc_version={version}.{}; cc_entrypoint={CLAUDE_CODE_ENTRYPOINT}; \
         cch={};",
        &suffix[..3],
        &cch[..5]
    )
}

fn settings(pinned: &str, has_override: bool) -> VersionSettings {
    VersionSettings {
        pinned: pinned.to_string(),
        has_override,
    }
}

/// [`VersionSettings`] with the bundled pin and no override.
fn pinned() -> VersionSettings {
    settings(CLAUDE_CODE_VERSION, false)
}

fn shaped_payload(version: &str) -> Value {
    json!({
        "model": "claude-haiku-4-5",
        "stream": true,
        "system": [
            { "type": "text", "text": expected_billing_header(USER_TEXT, version) },
            { "type": "text", "text": "You are an expert coding assistant." },
        ],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": USER_TEXT }] }],
    })
}

fn shaped_body(version: &str) -> String {
    shaped_payload(version).to_string()
}

fn sync_with_payload(floor: &LearnedFloor) -> BillingVersionSync {
    let mut sync = BillingVersionSync::new(floor.clone());
    sync.record_request(&shaped_payload(CLAUDE_CODE_VERSION));
    sync
}

fn ok_response() -> SyncResponse {
    SyncResponse {
        status: 200,
        body: "event: message_start\n\n".into(),
    }
}

/// A 400 shaped like the `claude_code_version_too_old` rejection Anthropic
/// returned on 2026-09-23.
fn claude_code_version_too_old(required_version: Option<&str>) -> SyncResponse {
    let message = match required_version {
        Some(version) => format!(
            "Claude Code 2.1.260 does not support this model; version {version} or newer is \
             required. Run 'claude update', or update the Claude desktop app, then try again."
        ),
        None => "Claude Code 2.1.260 does not support this model.".to_string(),
    };
    SyncResponse {
        status: 400,
        body: json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": message,
                "details": { "error_code": "claude_code_version_too_old" }
            },
            "request_id": REJECTION_REQUEST_ID,
        })
        .to_string(),
    }
}

/// Runs one request and returns the outcome plus every body the delegate saw.
fn fetch(
    sync: &BillingVersionSync,
    settings: &VersionSettings,
    user_agent: Option<&str>,
    body: &str,
    responses: &[SyncResponse],
) -> (rpi_anthropic_auth::billing::SyncOutcome, Vec<String>) {
    let calls = RefCell::new(Vec::new());
    let outcome = sync.fetch(settings, user_agent, body, |sent| {
        let mut calls = calls.borrow_mut();
        calls.push(sent.to_string());
        responses[(calls.len() - 1).min(responses.len() - 1)].clone()
    });
    (outcome, calls.into_inner())
}

fn dispatch(
    sync: &BillingVersionSync,
    user_agent: Option<&str>,
    body: &str,
) -> (rpi_anthropic_auth::billing::SyncOutcome, Vec<String>) {
    fetch(sync, &pinned(), user_agent, body, &[ok_response()])
}

fn expected_version(settings: &VersionSettings, user_agent: Option<&str>) -> String {
    let floor = LearnedFloor::new();
    BillingVersionSync::new(floor).target_version(settings, user_agent)
}

/// Asserts `response` is the rejection with `reason`'s hint appended, and that
/// everything a consumer might key on survived the rewrite.
fn assert_hinted(response: &SyncResponse, reason: &RecoveryHintReason) {
    assert_eq!(response.status, 400);
    let body: Value = serde_json::from_str(&response.body).expect("hinted body is JSON");
    assert_eq!(body["type"], "error");
    assert_eq!(body["request_id"], REJECTION_REQUEST_ID);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(
        body["error"]["details"]["error_code"],
        "claude_code_version_too_old"
    );
    let message = body["error"]["message"].as_str().expect("message string");
    assert!(
        message.ends_with(&format!(" {}", describe_recovery_hint(reason))),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// Header construction
// ---------------------------------------------------------------------------

#[test]
fn keeps_the_upstream_constants_locked() {
    // The salt, sampled positions, and entrypoint are part of Anthropic's recipe:
    // changing any of them makes the header indistinguishable from a forged one.
    assert_eq!(BILLING_HEADER_SALT, "59cf53e54c78");
    assert_eq!(BILLING_HEADER_POSITIONS, [4, 7, 20]);
    assert_eq!(CLAUDE_CODE_ENTRYPOINT, "sdk-cli");
    assert_eq!(BILLING_HEADER_MARKER, "x-anthropic-billing-header:");
    assert_eq!(CLAUDE_CODE_VERSION, "2.1.280");
}

#[test]
fn builds_the_pinned_header_exactly() {
    // Locked literal: a change to the salt, positions, sampling, or field order
    // breaks Anthropic's billing classification.
    assert_eq!(
        build_billing_header_value(SHAPED_TEXT, CLAUDE_CODE_VERSION).as_deref(),
        Some(
            "x-anthropic-billing-header: cc_version=2.1.280.1f8; cc_entrypoint=sdk-cli; cch=772da;"
        )
    );
    assert_eq!(
        build_billing_header_value(USER_TEXT, CLAUDE_CODE_VERSION).as_deref(),
        Some(
            "x-anthropic-billing-header: cc_version=2.1.280.626; cc_entrypoint=sdk-cli; cch=8edf5;"
        )
    );
}

#[test]
fn matches_the_independent_oracle() {
    for (text, version) in [
        (USER_TEXT, "2.1.280"),
        (USER_TEXT, "2.9.0"),
        ("a longer first user message with punctuation!", "2.1.251"),
        ("abc", "2.1.280"),
    ] {
        assert_eq!(
            build_billing_header_value(text, version),
            Some(expected_billing_header(text, version)),
            "{text} at {version}"
        );
    }
}

#[test]
fn returns_no_header_without_first_user_text() {
    assert_eq!(build_billing_header_value("", CLAUDE_CODE_VERSION), None);
}

#[test]
fn keeps_cch_stable_across_versions_and_changes_only_the_suffix() {
    let pinned = build_billing_header_value(USER_TEXT, "2.1.280").unwrap();
    let raised = build_billing_header_value(USER_TEXT, PI_AHEAD).unwrap();

    assert!(pinned.ends_with("cch=8edf5;"));
    assert!(raised.ends_with("cch=8edf5;"));
    assert!(pinned.contains("cc_version=2.1.280.626;"));
    assert!(raised.contains("cc_version=2.9.9.2ef;"));
}

#[test]
fn reads_the_first_user_text_from_string_content() {
    let messages = vec![json!({ "role": "user", "content": SHAPED_TEXT })];
    assert_eq!(get_first_user_text(&messages), SHAPED_TEXT);
}

#[test]
fn reads_the_first_user_text_from_a_text_block() {
    let messages = vec![json!({
        "role": "user",
        "content": [
            { "type": "tool_result", "output": "not text" },
            { "type": "text", "text": USER_TEXT },
        ],
    })];
    assert_eq!(get_first_user_text(&messages), USER_TEXT);
}

#[test]
fn skips_leading_non_user_messages() {
    let messages = vec![
        json!({ "role": "system", "content": "system text" }),
        json!({ "role": "user", "content": USER_TEXT }),
    ];
    assert_eq!(get_first_user_text(&messages), USER_TEXT);
}

#[test]
fn returns_no_first_user_text_without_a_user_text_message() {
    assert_eq!(get_first_user_text(&[]), "");
    assert_eq!(
        get_first_user_text(&[json!({
            "role": "user",
            "content": [{ "type": "tool_result", "output": "not text" }],
        })]),
        ""
    );
}

#[test]
fn reads_the_first_user_text_from_a_payload() {
    assert_eq!(
        payload_first_user_text(&json!({ "messages": [{ "role": "user", "content": USER_TEXT }] })),
        USER_TEXT
    );
    assert_eq!(
        payload_first_user_text(&json!({ "model": "claude-haiku-4-5" })),
        ""
    );
    assert_eq!(payload_first_user_text(&json!("not a record")), "");
}

// ---------------------------------------------------------------------------
// System-block shaping
// ---------------------------------------------------------------------------

#[test]
fn prepends_the_billing_header_block_without_adding_cache_control() {
    let system = json!([
        {
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            "cache_control": { "type": "ephemeral" },
        },
        { "type": "text", "text": "Generic system prompt." },
    ]);
    let messages = vec![json!({ "role": "user", "content": SHAPED_TEXT })];

    let blocks = prepend_billing_header(Some(&system), &messages, CLAUDE_CODE_VERSION).unwrap();

    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0]["text"],
        expected_billing_header(SHAPED_TEXT, CLAUDE_CODE_VERSION)
    );
    assert_eq!(blocks[0]["type"], "text");
    assert!(
        blocks[0].get("cache_control").is_none(),
        "the header must never consume a cache breakpoint"
    );
    assert_eq!(blocks[1]["cache_control"], json!({ "type": "ephemeral" }));
    assert_eq!(blocks[2]["text"], "Generic system prompt.");
}

#[test]
fn normalizes_a_bare_string_system_into_blocks() {
    let system = json!("Generic system prompt.");
    let messages = vec![json!({ "role": "user", "content": SHAPED_TEXT })];

    let blocks = prepend_billing_header(Some(&system), &messages, CLAUDE_CODE_VERSION).unwrap();

    assert_eq!(blocks.len(), 2);
    assert!(blocks[0]["text"]
        .as_str()
        .unwrap()
        .starts_with(BILLING_HEADER_MARKER));
    assert_eq!(
        blocks[1],
        json!({ "type": "text", "text": "Generic system prompt." })
    );
}

#[test]
fn prepends_the_header_when_the_system_field_is_absent() {
    let messages = vec![json!({ "role": "user", "content": SHAPED_TEXT })];

    let blocks = prepend_billing_header(None, &messages, CLAUDE_CODE_VERSION).unwrap();

    assert_eq!(blocks.len(), 1);
    assert!(blocks[0]["text"]
        .as_str()
        .unwrap()
        .starts_with(BILLING_HEADER_MARKER));
}

#[test]
fn does_not_duplicate_an_existing_billing_header_block() {
    let header = expected_billing_header(SHAPED_TEXT, CLAUDE_CODE_VERSION);
    let system = json!([
        { "type": "text", "text": header },
        { "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." },
    ]);
    let messages = vec![json!({ "role": "user", "content": SHAPED_TEXT })];

    let blocks = prepend_billing_header(Some(&system), &messages, CLAUDE_CODE_VERSION).unwrap();

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["text"], header);
}

#[test]
fn leaves_the_system_untouched_without_a_user_text_message() {
    let messages = vec![json!({
        "role": "user",
        "content": [{ "type": "tool_result", "output": "not text" }],
    })];

    assert_eq!(
        prepend_billing_header(
            Some(&json!([{ "type": "text", "text": "kept" }])),
            &messages,
            CLAUDE_CODE_VERSION
        ),
        None
    );
}

// ---------------------------------------------------------------------------
// Version sync before dispatch
// ---------------------------------------------------------------------------

#[test]
fn leaves_the_body_untouched_when_pi_reports_no_claude_cli_version() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = shaped_body(CLAUDE_CODE_VERSION);

    let (outcome, calls) = dispatch(&sync, None, &original);

    assert_eq!(calls, vec![original]);
    assert_eq!(outcome.attempts, 1);
}

#[test]
fn leaves_the_body_untouched_when_pis_version_is_lower() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = shaped_body(CLAUDE_CODE_VERSION);

    let (_, calls) = dispatch(&sync, Some(&format!("claude-cli/{PI_BEHIND}")), &original);

    assert_eq!(calls, vec![original]);
}

#[test]
fn leaves_the_body_untouched_when_pis_version_matches_the_pin() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = shaped_body(CLAUDE_CODE_VERSION);

    let (_, calls) = dispatch(
        &sync,
        Some(&format!("claude-cli/{CLAUDE_CODE_VERSION}")),
        &original,
    );

    assert_eq!(calls, vec![original]);
}

#[test]
fn rewrites_cc_version_and_its_suffix_when_pis_version_is_higher() {
    let sync = sync_with_payload(&LearnedFloor::new());

    let (_, calls) = dispatch(
        &sync,
        Some(&format!("claude-cli/{PI_AHEAD}")),
        &shaped_body(CLAUDE_CODE_VERSION),
    );

    assert_eq!(calls, vec![shaped_body(PI_AHEAD)]);
    assert!(calls[0].contains(&expected_billing_header(USER_TEXT, PI_AHEAD)));
    assert!(!calls[0].contains(&expected_billing_header(USER_TEXT, CLAUDE_CODE_VERSION)));
}

#[test]
fn changes_nothing_outside_the_billing_header_block() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = shaped_body(CLAUDE_CODE_VERSION);

    let (_, calls) = dispatch(&sync, Some(&format!("claude-cli/{PI_AHEAD}")), &original);

    let excise = |body: &str, header: &str| body.replace(header, "<billing>");
    assert_eq!(
        excise(&calls[0], &expected_billing_header(USER_TEXT, PI_AHEAD)),
        excise(
            &original,
            &expected_billing_header(USER_TEXT, CLAUDE_CODE_VERSION)
        )
    );
    assert_ne!(calls[0], original);
}

#[test]
fn honors_an_explicit_version_override_instead_of_raising_it() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = shaped_body("2.1.300");
    let overridden = settings("2.1.300", true);

    let (_, calls) = fetch(
        &sync,
        &overridden,
        Some(&format!("claude-cli/{PI_AHEAD}")),
        &original,
        &[ok_response()],
    );

    assert_eq!(calls, vec![original]);
}

#[test]
fn leaves_the_body_untouched_when_no_request_was_recorded() {
    let sync = BillingVersionSync::new(LearnedFloor::new());
    let original = shaped_body(CLAUDE_CODE_VERSION);

    let (_, calls) = dispatch(&sync, Some(&format!("claude-cli/{PI_AHEAD}")), &original);

    assert_eq!(calls, vec![original]);
}

#[test]
fn leaves_the_body_untouched_when_it_carries_no_billing_header() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let original = json!({ "model": "claude-haiku-4-5", "stream": true }).to_string();

    let (outcome, calls) = dispatch(&sync, Some(&format!("claude-cli/{PI_AHEAD}")), &original);

    assert_eq!(calls, vec![original]);
    assert_eq!(outcome.response, ok_response());
}

#[test]
fn returns_the_delegates_response_unchanged() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let response = SyncResponse {
        status: 200,
        body: "event: message_start\n\n".into(),
    };

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &[response.clone()],
    );

    assert_eq!(outcome.response, response);
    assert_eq!(outcome.attempts, 1);
}

#[test]
fn resolves_the_target_version_from_pi_and_the_learned_floor() {
    let floor = LearnedFloor::new();
    assert_eq!(
        expected_version(&pinned(), Some(&format!("claude-cli/{PI_AHEAD}"))),
        PI_AHEAD
    );
    assert_eq!(
        expected_version(&pinned(), Some(&format!("claude-cli/{PI_BEHIND}"))),
        CLAUDE_CODE_VERSION
    );

    floor.learn(REQUIRED);
    let sync = BillingVersionSync::new(floor);
    assert_eq!(
        sync.target_version(&pinned(), None),
        REQUIRED,
        "the learned floor applies to the next request"
    );
    assert_eq!(
        sync.target_version(
            &settings("2.1.260", true),
            Some(&format!("claude-cli/{PI_AHEAD}"))
        ),
        "2.1.260",
        "an override is absolute"
    );
}

// ---------------------------------------------------------------------------
// Recovery from claude_code_version_too_old
// ---------------------------------------------------------------------------

#[test]
fn retries_once_with_the_billing_header_at_the_required_version() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_eq!(outcome.attempts, 2);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1], shaped_body(REQUIRED));
    assert_eq!(outcome.response, ok_response());
}

#[test]
fn changes_nothing_outside_the_billing_header_block_on_retry() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];

    let (_, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    let excise = |body: &str| {
        body.replace(&expected_billing_header(USER_TEXT, REQUIRED), "<billing>")
            .replace(
                &expected_billing_header(USER_TEXT, CLAUDE_CODE_VERSION),
                "<billing>",
            )
    };
    assert_eq!(excise(&calls[1]), excise(&calls[0]));
    assert_ne!(calls[1], calls[0]);
}

#[test]
fn keeps_the_same_dispatch_target_on_retry() {
    // The transport receives one body string per attempt; the URL and init are
    // the caller's to reuse, exactly as upstream reuses them.
    let sync = sync_with_payload(&LearnedFloor::new());
    let seen = RefCell::new(Vec::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];

    let outcome = sync.fetch(&pinned(), None, &shaped_body(CLAUDE_CODE_VERSION), |sent| {
        let mut seen = seen.borrow_mut();
        seen.push(sent.to_string());
        responses[(seen.len() - 1).min(responses.len() - 1)].clone()
    });

    assert_eq!(outcome.attempts, 2);
    assert_eq!(seen.borrow().len(), 2);
}

#[test]
fn sends_later_requests_at_the_learned_floor_without_a_rejection() {
    let floor = LearnedFloor::new();
    let first_responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];
    let first = sync_with_payload(&floor);
    fetch(
        &first,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &first_responses,
    );

    let second = sync_with_payload(&floor);
    let (outcome, calls) = fetch(
        &second,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &[ok_response()],
    );

    assert_eq!(outcome.attempts, 1);
    assert_eq!(calls, vec![shaped_body(REQUIRED)]);
}

#[test]
fn does_not_read_a_success_response_for_a_rejection() {
    let sync = sync_with_payload(&LearnedFloor::new());

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &[ok_response()],
    );

    assert_eq!(outcome.attempts, 1);
    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response.status, 200);
}

#[test]
fn returns_another_400_untouched() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let rejection = SyncResponse {
        status: 400,
        body: r#"{"type":"error","error":{"type":"invalid_request_error"}}"#.into(),
    };

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &[rejection.clone()],
    );

    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response, rejection);
}

#[test]
fn does_not_retry_when_an_explicit_version_override_is_set() {
    let floor = LearnedFloor::new();
    let sync = sync_with_payload(&floor);
    let overridden = settings("2.1.260", true);
    let responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];

    let (outcome, calls) = fetch(
        &sync,
        &overridden,
        None,
        &shaped_body("2.1.260"),
        &responses,
    );

    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response.status, 400);
    assert_eq!(floor.apply_to("1.0.0"), "1.0.0");
}

#[test]
fn does_not_apply_a_learned_floor_over_an_explicit_version_override() {
    let floor = LearnedFloor::new();
    floor.learn(REQUIRED);
    let sync = sync_with_payload(&floor);
    let original = shaped_body("2.1.260");

    let (_, calls) = fetch(
        &sync,
        &settings("2.1.260", true),
        None,
        &original,
        &[ok_response()],
    );

    assert_eq!(calls, vec![original]);
}

#[test]
fn does_not_retry_when_the_required_version_is_not_above_the_one_sent() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some("2.1.200")), ok_response()];

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response.status, 400);
}

#[test]
fn does_not_retry_when_the_rejection_names_no_required_version() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(None), ok_response()];

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response.status, 400);
}

#[test]
fn does_not_retry_a_body_it_cannot_rebuild() {
    let sync = BillingVersionSync::new(LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED)), ok_response()];

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_eq!(calls.len(), 1);
    assert_eq!(outcome.response.status, 400);
}

#[test]
fn retries_at_most_once_when_the_retry_is_rejected_too() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [
        claude_code_version_too_old(Some(REQUIRED)),
        claude_code_version_too_old(Some("2.9.5")),
        ok_response(),
    ];

    let (outcome, calls) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_eq!(calls.len(), 2);
    assert_eq!(outcome.attempts, 2);
    assert_eq!(outcome.response.status, 400);
}

// ---------------------------------------------------------------------------
// Hints on unrecovered rejections
// ---------------------------------------------------------------------------

#[test]
fn tells_an_override_user_to_raise_or_unset_the_override() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED))];

    let (outcome, _) = fetch(
        &sync,
        &settings("2.1.260", true),
        None,
        &shaped_body("2.1.260"),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::Override {
            override_version: "2.1.260".into(),
            required_version: Some(REQUIRED.into()),
        },
    );
}

#[test]
fn blames_pi_when_the_billing_header_already_met_the_floor() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some("2.1.200"))];

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::UpgradePi {
            sent_version: CLAUDE_CODE_VERSION.into(),
        },
    );
}

#[test]
fn points_at_the_override_when_the_floor_is_not_named() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [claude_code_version_too_old(None)];

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::SetOverride {
            required_version: None,
        },
    );
}

#[test]
fn points_at_the_override_when_the_body_cannot_be_rebuilt() {
    let sync = BillingVersionSync::new(LearnedFloor::new());
    let responses = [claude_code_version_too_old(Some(REQUIRED))];

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::SetOverride {
            required_version: Some(REQUIRED.into()),
        },
    );
}

#[test]
fn hints_the_retrys_rejection_when_the_retry_is_rejected_too() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [
        claude_code_version_too_old(Some(REQUIRED)),
        claude_code_version_too_old(Some("2.9.5")),
    ];

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::SetOverride {
            required_version: Some("2.9.5".into()),
        },
    );
}

#[test]
fn blames_pi_when_the_retry_at_the_named_floor_is_rejected_at_that_floor() {
    let sync = sync_with_payload(&LearnedFloor::new());
    let responses = [
        claude_code_version_too_old(Some(REQUIRED)),
        claude_code_version_too_old(Some(REQUIRED)),
    ];

    let (outcome, _) = fetch(
        &sync,
        &pinned(),
        None,
        &shaped_body(CLAUDE_CODE_VERSION),
        &responses,
    );

    assert_hinted(
        &outcome.response,
        &RecoveryHintReason::UpgradePi {
            sent_version: REQUIRED.into(),
        },
    );
}

#[test]
fn the_override_environment_variable_name_is_stable() {
    // The hints tell users which variable to set; a rename is a user-visible break.
    assert_eq!(
        CLAUDE_CODE_VERSION_ENV,
        "PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION"
    );
}
