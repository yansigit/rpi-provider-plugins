#!/usr/bin/env bash
# End-to-end dry run of the throwaway CortexKit vault with FAKE data only.
#
#   build -> start an isolated ck-subc with the claustrum module
#   -> rpi's Antigravity client proposes an enrollment (auth.enroll_propose)
#   -> operator approves it (ck-auth enroll approve)
#   -> rpi's client polls and receives its token (auth.enroll_poll), enrollment approved
#   -> operator stores a FAKE Antigravity OAuth record and grants the enrolled name read
#   -> rpi selects that account ("Use this account") and makes one custody admission
#      (credential.list_scoped + credential.get_scoped); the bearer it receives must equal
#      the fake token written by this script
#   -> negative control: the enrollment is revoked and the same admission must be refused
#   -> teardown: every process and listening socket is gone; the state directory is deleted
#
# Every rpi step goes through the package's own vault code (rpi_antigravity_auth::vault)
# via tools/vault-throwaway/harness. The data is generated here; no real account, token,
# network service or ~/.pi / ~/.config/cortexkit path is involved.
#
# Usage: dry-run.sh
# Exit status 0 only if every check passes and the teardown is clean.
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }
json_field() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

step "build"
"$VT_SCRIPTS_DIR/build.sh" >"$VT_SRC_ROOT/dry-run-build.log" 2>&1 \
  || { tail -n 40 "$VT_SRC_ROOT/dry-run-build.log" >&2; fail "build.sh failed"; }
pass "build.sh (log: $VT_SRC_ROOT/dry-run-build.log)"

STATE_IN="$(mktemp -d "$HOME/.vault-throwaway.XXXXXX")"
chmod 700 "$STATE_IN"
vt_paths "$(cd "$STATE_IN" && pwd -P)"
completed=0
cleanup() {
  rc=$?
  if [ "$completed" -eq 0 ]; then
    echo "dry run did not finish (exit $rc); stopping the rig and keeping $VT_STATE" >&2
    "$VT_SCRIPTS_DIR/teardown.sh" --keep-state "$VT_STATE" >&2 || true
  fi
  exit "$rc"
}
trap cleanup EXIT

step "start an isolated ck-subc with the claustrum module"
"$VT_SCRIPTS_DIR/start.sh" "$VT_STATE"
pass "supervisor healthy and claustrum registered (state $VT_STATE)"

# Fake data only: a Pi-format auth.json entry for google, with the antigravity packed
# refresh form and an access token valid for 24 h (so nothing is ever refreshed remotely).
mkdir -p "$VT_FAKE_DIR"
chmod 700 "$VT_FAKE_DIR"
python3 -I - "$VT_FAKE_DIR" <<'PY'
import json, os, secrets, sys, time
directory = sys.argv[1]
token = "fake-antigravity-access-dryrun-" + secrets.token_hex(12)
expires = int((time.time() + 24 * 3600) * 1000)
auth = {"google": {"type": "oauth", "refresh": "fake-refresh-dryrun|fake-project-dryrun",
                   "access": token, "expires": expires}}
files = {"auth-pi-fake.json": json.dumps(auth),
         "antigravity-access-token.txt": token + "\n"}
for name, text in files.items():
    fd = os.open(os.path.join(directory, name), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)
PY
pass "fake Antigravity credential and fixture token written (owner-only, not printed)"

rpi() { (cd "$VT_STATE" && vt_env_run "$VT_BIN_STATE/vault-check" "$@"); }

step "rpi proposes its Antigravity enrollment"
out="$(rpi enroll "$VT_VAULT_DIR")" || fail "rpi enroll (propose) failed"
state="$(json_field 'd["status"]["state"]' <<<"$out")"
request_id="$(json_field 'd["status"].get("requestId", "")' <<<"$out")"
[ "$state" = "pending" ] || fail "expected pending after propose, got: $out"
[ -n "$request_id" ] || fail "no request id in: $out"
pass "rpi proposed antigravity-auth-pi; request $request_id is pending"

step "operator lists and approves the request"
pending="$("$VT_SCRIPTS_DIR/approve.sh" "$VT_STATE" pending)"
[ "$pending" = "$request_id" ] || fail "pending requests are [$pending], expected [$request_id]"
"$VT_SCRIPTS_DIR/approve.sh" "$VT_STATE" list
"$VT_SCRIPTS_DIR/approve.sh" "$VT_STATE" approve "$request_id"
pass "operator approved the request (approval admits the name; no token is minted here)"

step "rpi polls and receives its enrollment token"
out="$(rpi enroll "$VT_VAULT_DIR")" || fail "rpi enroll (poll) failed"
state="$(json_field 'd["status"]["state"]' <<<"$out")"
generation="$(json_field 'd["status"].get("tokenGeneration")' <<<"$out")"
[ "$state" = "approved" ] || fail "expected approved after poll, got: $out"
[ "$generation" = "1" ] || fail "expected token generation 1, got: $out"
pass "rpi enrollment approved (token generation $generation)"

step "operator stores the fake Antigravity credential and grants the enrolled name"
"$VT_SCRIPTS_DIR/store-credential.sh" "$VT_STATE" "$VT_FAKE_DIR/auth-pi-fake.json" \
  --account-id fake-account-dryrun --email fake-user@example.test
pass "antigravity:google stored from a file; antigravity-native granted read"

step "rpi selects the account and makes one custody admission"
out="$(rpi use "$VT_VAULT_DIR")" || fail "rpi use failed"
route="$(json_field 'd["selectedRouteId"]' <<<"$out")"
case "$route" in vault:antigravity:google*) ;; *) fail "unexpected route: $out" ;; esac
pass "rpi selected route $route"

out="$(rpi fetch "$VT_VAULT_DIR" "$VT_FAKE_DIR/antigravity-access-token.txt")" \
  || fail "custody fetch failed"
matches="$(json_field 'd["matchesFixture"]' <<<"$out")"
project="$(json_field 'd["admission"]["projectId"]' <<<"$out")"
identity="$(json_field 'd["admission"]["accountIdentity"]' <<<"$out")"
[ "$matches" = "True" ] || fail "custody delivered a bearer that does not match the fixture"
[ "$project" = "fake-project-dryrun" ] || fail "unexpected project in admission: $project"
[ "$identity" = "fake-account-dryrun" ] || fail "unexpected account identity in admission: $identity"
pass "rpi received the fake bearer through custody (sha256 $(json_field 'd["bearerSha256"]' <<<"$out"))"

step "vault-side evidence"
events="$(vt_ck_auth events)"
grep -q "scoped_first_use .*enrolled:antigravity-auth-pi" <<<"$events" \
  || fail "no scoped_first_use row for the enrolled principal"
pass "ck-auth events records scoped_first_use for enrolled:antigravity-auth-pi"
vt_ck_auth verify-audit | grep -q "audit chain verified: intact" \
  || fail "audit chain is not intact"
pass "ck-auth verify-audit: chain intact"

step "negative control: revoked enrollment is refused"
"$VT_SCRIPTS_DIR/approve.sh" "$VT_STATE" revoke
if err="$(rpi fetch "$VT_VAULT_DIR" "$VT_FAKE_DIR/antigravity-access-token.txt" 2>&1)"; then
  fail "custody delivered a bearer after revoke"
fi
grep -q "not_found" <<<"$err" || fail "unexpected refusal after revoke: $err"
pass "custody refused the revoked enrollment: $err"

step "teardown"
"$VT_SCRIPTS_DIR/teardown.sh" "$VT_STATE"
completed=1
[ ! -e "$VT_STATE" ] || fail "state directory still exists after teardown"
pass "teardown clean; state directory removed"

echo
echo "dry run: PASS (all checks, fake data only)"
