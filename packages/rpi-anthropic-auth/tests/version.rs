//! Mirrors `test/claude-code-version.test.ts` and `test/version-rejection.test.ts`
//! (gotgenes/pi-anthropic-auth).

use rpi_anthropic_auth::version::{
    describe_recovery_hint, has_claude_code_version_override_from, higher_version, is_bare_version,
    read_claude_cli_version, read_claude_code_version_rejection, resolve_claude_code_version_from,
    InvalidClaudeCodeVersion, LearnedFloor, RecoveryHintReason, CLAUDE_CODE_VERSION,
    CLAUDE_CODE_VERSION_ENV,
};
use serde_json::{json, Value};

fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    }
}

fn unset(_: &str) -> Option<String> {
    None
}

#[test]
fn resolves_the_bundled_pin_without_an_override() {
    assert_eq!(resolve_claude_code_version_from(unset).unwrap(), "2.1.280");
    assert_eq!(
        resolve_claude_code_version_from(unset).unwrap(),
        CLAUDE_CODE_VERSION
    );
}

#[test]
fn resolves_a_trimmed_override() {
    let env = vars(&[(CLAUDE_CODE_VERSION_ENV, " 2.1.300 ")]);
    assert_eq!(resolve_claude_code_version_from(env).unwrap(), "2.1.300");
}

#[test]
fn treats_a_blank_override_as_unset() {
    for blank in ["", "   ", "\t"] {
        let env = move |_: &str| Some(blank.to_string());
        assert_eq!(
            resolve_claude_code_version_from(env).unwrap(),
            CLAUDE_CODE_VERSION
        );
    }
}

#[test]
fn rejects_a_malformed_override_through_the_environment_lookup() {
    // The malformed value must surface loudly instead of silently sending the
    // bundled version the user was trying to replace.
    let error = resolve_claude_code_version_from(|_: &str| Some("latest".into())).unwrap_err();
    assert_eq!(
        error,
        InvalidClaudeCodeVersion {
            value: "latest".into()
        }
    );
    assert_eq!(
        error.to_string(),
        format!("{CLAUDE_CODE_VERSION_ENV} must be a bare X.Y.Z version, received \"latest\"")
    );
}

#[test]
fn rejects_every_non_bare_override_shape() {
    for invalid in [
        "latest",
        "2.1",
        "v2.1.260",
        "2.1.260-beta",
        "2.1.260.1",
        "2..1",
    ] {
        let error =
            resolve_claude_code_version_from(move |_: &str| Some(invalid.to_string())).unwrap_err();
        assert_eq!(error.value, invalid);
    }
}

#[test]
fn recognizes_bare_versions() {
    for valid in ["0.0.0", "2.1.280", "10.20.30", "01.02.03"] {
        assert!(is_bare_version(valid), "{valid}");
    }
    for invalid in [
        "", "2", "2.1", "2.1.", ".2.1", "2.1.280.", "v2.1.280", "2.1.280x",
    ] {
        assert!(!is_bare_version(invalid), "{invalid}");
    }
}

#[test]
fn reports_whether_an_override_is_set() {
    assert!(!has_claude_code_version_override_from(unset));
    assert!(!has_claude_code_version_override_from(|_: &str| {
        Some("  ".into())
    }));
    assert!(has_claude_code_version_override_from(vars(&[(
        CLAUDE_CODE_VERSION_ENV,
        "2.1.300"
    )])));
}

#[test]
fn pins_a_version_at_or_above_the_opus_5_5_floor() {
    assert_eq!(
        higher_version(CLAUDE_CODE_VERSION, Some("2.1.280")),
        "2.1.280"
    );
    assert_eq!(
        higher_version("2.1.251", Some(CLAUDE_CODE_VERSION)),
        CLAUDE_CODE_VERSION
    );
}

#[test]
fn reads_the_version_out_of_pis_claude_cli_user_agent() {
    assert_eq!(
        read_claude_cli_version(Some("claude-cli/2.1.280")).as_deref(),
        Some("2.1.280")
    );
}

#[test]
fn ignores_surrounding_user_agent_tokens() {
    assert_eq!(
        read_claude_cli_version(Some("Mozilla/5.0 claude-cli/2.1.251 extra/1.0")).as_deref(),
        Some("2.1.251")
    );
    assert_eq!(
        read_claude_cli_version(Some("pi/0.87.1\tclaude-cli/2.1.260")).as_deref(),
        Some("2.1.260")
    );
}

#[test]
fn returns_none_when_no_user_agent_is_present() {
    assert_eq!(read_claude_cli_version(None), None);
    assert_eq!(read_claude_cli_version(Some("")), None);
}

#[test]
fn returns_none_for_a_non_claude_cli_user_agent() {
    assert_eq!(read_claude_cli_version(Some("pi/0.87.1")), None);
    assert_eq!(read_claude_cli_version(Some("2.1.280")), None);
    assert_eq!(read_claude_cli_version(Some("claude-cli")), None);
}

#[test]
fn returns_none_for_a_malformed_claude_cli_version() {
    assert_eq!(read_claude_cli_version(Some("claude-cli/2.1")), None);
    assert_eq!(read_claude_cli_version(Some("claude-cli/next")), None);
    assert_eq!(read_claude_cli_version(Some("claude-cli/")), None);
    assert_eq!(read_claude_cli_version(Some("claude-cli/2.1.280x")), None);
}

#[test]
fn does_not_match_a_user_agent_that_merely_contains_the_token() {
    assert_eq!(read_claude_cli_version(Some("notclaude-cli/2.1.280")), None);
}

#[test]
fn returns_the_higher_candidate_version() {
    assert_eq!(higher_version("2.1.260", Some("2.1.280")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("2.2.0")), "2.2.0");
    assert_eq!(higher_version("2.1.280", Some("3.0.0")), "3.0.0");
}

#[test]
fn returns_the_baseline_when_the_candidate_is_lower() {
    assert_eq!(higher_version("2.1.280", Some("2.1.251")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("2.0.999")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("1.9.9")), "2.1.280");
}

#[test]
fn returns_the_baseline_when_the_versions_are_equal() {
    assert_eq!(higher_version("2.1.280", Some("2.1.280")), "2.1.280");
}

#[test]
fn compares_components_numerically_not_lexically() {
    // "2.1.9" sorts after "2.1.280" as a string but is a lower version.
    assert_eq!(higher_version("2.1.280", Some("2.1.9")), "2.1.280");
    assert_eq!(higher_version("2.1.9", Some("2.1.280")), "2.1.280");
}

#[test]
fn returns_the_baseline_for_an_absent_or_unparseable_candidate() {
    assert_eq!(higher_version("2.1.280", None), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("2.1")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("latest")), "2.1.280");
    assert_eq!(higher_version("2.1.280", Some("v2.1.999")), "2.1.280");
}

#[test]
fn leaves_a_version_unchanged_before_anything_is_learned() {
    let floor = LearnedFloor::new();
    assert_eq!(floor.apply_to("2.1.280"), "2.1.280");
}

#[test]
fn raises_a_lower_version_to_the_learned_floor() {
    let floor = LearnedFloor::new();
    floor.learn("2.1.300");
    assert_eq!(floor.apply_to("2.1.280"), "2.1.300");
}

#[test]
fn leaves_a_version_above_the_learned_floor_unchanged() {
    let floor = LearnedFloor::new();
    floor.learn("2.1.300");
    assert_eq!(floor.apply_to("2.2.0"), "2.2.0");
}

#[test]
fn never_lowers_the_learned_floor() {
    let floor = LearnedFloor::new();
    floor.learn("2.1.300");
    floor.learn("2.1.250");
    assert_eq!(floor.apply_to("1.0.0"), "2.1.300");
}

#[test]
fn ignores_an_unparseable_learned_version() {
    let floor = LearnedFloor::new();
    floor.learn("2.1.300");
    floor.learn("latest");
    assert_eq!(floor.apply_to("1.0.0"), "2.1.300");
}

#[test]
fn ignores_an_unparseable_version_before_anything_is_learned() {
    let floor = LearnedFloor::new();
    floor.learn("latest");
    assert_eq!(floor.apply_to("1.0.0"), "1.0.0");
}

#[test]
fn shares_the_learned_floor_between_clones() {
    let floor = LearnedFloor::new();
    let shared = floor.clone();
    shared.learn("2.1.300");
    assert_eq!(floor.apply_to("1.0.0"), "2.1.300");
}

const REJECTION_REQUEST_ID: &str = "req_test_rejection";

fn rejection_body(required_version: Option<&str>) -> String {
    let message = match required_version {
        Some(version) => format!(
            "Claude Code 2.1.260 does not support this model; version {version} or newer is \
             required. Run 'claude update', or update the Claude desktop app, then try again."
        ),
        None => "Claude Code 2.1.260 does not support this model.".to_string(),
    };
    json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": message,
            "details": { "error_code": "claude_code_version_too_old" }
        },
        "request_id": REJECTION_REQUEST_ID,
    })
    .to_string()
}

#[test]
fn parses_the_floor_a_rejection_names() {
    let rejection = read_claude_code_version_rejection(&rejection_body(Some("2.9.0"))).unwrap();
    assert_eq!(rejection.required_version.as_deref(), Some("2.9.0"));
}

#[test]
fn parses_a_floor_named_after_an_unrelated_version_mention() {
    let body = json!({
        "type": "error",
        "error": {
            "message": "Update your version of Claude Code; version 2.1.280 or newer is required.",
            "details": { "error_code": "claude_code_version_too_old" }
        }
    })
    .to_string();
    let rejection = read_claude_code_version_rejection(&body).unwrap();
    assert_eq!(rejection.required_version.as_deref(), Some("2.1.280"));
}

#[test]
fn leaves_the_floor_unset_when_the_message_names_none() {
    assert_eq!(
        read_claude_code_version_rejection(&rejection_body(None))
            .unwrap()
            .required_version,
        None
    );

    let unparseable = json!({
        "error": {
            "message": "Claude Code 2.1.260 does not support this model; version 2.1 or newer is required.",
            "details": { "error_code": "claude_code_version_too_old" }
        }
    })
    .to_string();
    assert_eq!(
        read_claude_code_version_rejection(&unparseable)
            .unwrap()
            .required_version,
        None
    );
}

#[test]
fn ignores_anything_that_is_not_a_version_too_old_rejection() {
    for body in [
        "not json".to_string(),
        json!({ "type": "error" }).to_string(),
        json!({ "error": { "message": "boom" } }).to_string(),
        json!({ "error": { "details": { "error_code": "invalid_request_error" } } }).to_string(),
        json!([1, 2, 3]).to_string(),
    ] {
        assert!(
            read_claude_code_version_rejection(&body).is_none(),
            "{body}"
        );
    }
}

#[test]
fn appends_a_hint_without_losing_any_other_field() {
    let rejection = read_claude_code_version_rejection(&rejection_body(Some("2.9.0"))).unwrap();
    let hinted: Value =
        serde_json::from_str(&rejection.with_hint("try this")).expect("hinted body is JSON");

    assert_eq!(hinted["type"], "error");
    assert_eq!(hinted["request_id"], REJECTION_REQUEST_ID);
    assert_eq!(hinted["error"]["type"], "invalid_request_error");
    assert_eq!(
        hinted["error"]["details"]["error_code"],
        "claude_code_version_too_old"
    );
    assert!(hinted["error"]["message"]
        .as_str()
        .unwrap()
        .ends_with(" try this"));
}

#[test]
fn a_missing_message_becomes_the_hint() {
    let body = json!({
        "error": { "details": { "error_code": "claude_code_version_too_old" } }
    })
    .to_string();
    let rejection = read_claude_code_version_rejection(&body).unwrap();
    let hinted: Value = serde_json::from_str(&rejection.with_hint("only hint")).unwrap();
    assert_eq!(hinted["error"]["message"], "only hint");
}

#[test]
fn describes_the_override_hint() {
    assert_eq!(
        describe_recovery_hint(&RecoveryHintReason::Override {
            override_version: "2.1.260".into(),
            required_version: Some("2.9.0".into()),
        }),
        "[pi-anthropic-auth] PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION=2.1.260 is sent verbatim; \
         raise it to 2.9.0 or newer, or unset it to let pi-anthropic-auth recover automatically."
    );
    assert_eq!(
        describe_recovery_hint(&RecoveryHintReason::Override {
            override_version: "2.1.260".into(),
            required_version: None,
        }),
        "[pi-anthropic-auth] PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION=2.1.260 is sent verbatim; \
         raise it to the current Claude Code release (npm view @anthropic-ai/claude-code \
         dist-tags), or unset it to let pi-anthropic-auth recover automatically."
    );
}

#[test]
fn describes_the_upgrade_pi_hint() {
    assert_eq!(
        describe_recovery_hint(&RecoveryHintReason::UpgradePi {
            sent_version: "2.1.280".into(),
        }),
        "[pi-anthropic-auth] The billing header already reported 2.1.280; this rejection likely \
         comes from Pi's own claude-cli version. Upgrade Pi."
    );
}

#[test]
fn describes_the_set_override_hint() {
    assert_eq!(
        describe_recovery_hint(&RecoveryHintReason::SetOverride {
            required_version: Some("2.9.0".into()),
        }),
        "[pi-anthropic-auth] Automatic recovery did not succeed; set \
         PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION=2.9.0 and retry."
    );
    assert_eq!(
        describe_recovery_hint(&RecoveryHintReason::SetOverride {
            required_version: None,
        }),
        "[pi-anthropic-auth] Automatic recovery did not succeed; set \
         PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION to the current Claude Code release (npm view \
         @anthropic-ai/claude-code dist-tags) and retry."
    );
}
