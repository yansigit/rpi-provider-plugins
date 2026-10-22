#!/usr/bin/env bash
# Operator wrapper for rpi's Antigravity enrollment requests (ck-auth enroll ...), against a
# throwaway vault started by start.sh. Approval admits a NAME and mints nothing: rpi's own
# poll mints the token, authenticated by the request secret only rpi holds.
#
# Usage: approve.sh <state-dir> <command> [args]
#   pending                       print the request ids awaiting approval, one per line
#   list                          ck-auth enroll list
#   approve <request-id>          approve one pending request for the expected name
#                                 (antigravity-auth-pi); refuses any other proposed name
#   deny <request-id>             ck-auth enroll deny
#   revoke                        revoke the antigravity-auth-pi enrollment (negative control)
#   grant                         grant enrolled:antigravity-auth-pi read on antigravity-native
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

EXPECTED_NAME="antigravity-auth-pi"

vt_require_state "${1:-}"
shift
command="${1:-}"
[ -n "$command" ] || vt_die "usage: approve.sh <state-dir> <pending|list|approve|deny|revoke|grant> [args]"
shift

case "$command" in
  pending)
    vt_ck_auth enroll list | awk '$2=="pending" {print $1}'
    ;;
  list)
    vt_ck_auth enroll list
    ;;
  approve)
    request_id="${1:-}"
    [ -n "$request_id" ] || vt_die "approve needs a request id (see: approve.sh <state-dir> pending)"
    row="$(vt_ck_auth enroll list | awk -v id="$request_id" '$1==id && $2=="pending"')"
    [ -n "$row" ] || vt_die "$request_id is not a pending request"
    proposed="$(awk '{print $4}' <<<"$row")"
    [ "$proposed" = "$EXPECTED_NAME" ] \
      || vt_die "$request_id proposes '$proposed', not '$EXPECTED_NAME'; refusing to approve a name this rig did not expect"
    vt_ck_auth enroll approve --request-id "$request_id"
    ;;
  deny)
    request_id="${1:-}"
    [ -n "$request_id" ] || vt_die "deny needs a request id"
    vt_ck_auth enroll deny --request-id "$request_id"
    ;;
  revoke)
    vt_ck_auth enroll revoke --name "$EXPECTED_NAME"
    ;;
  grant)
    vt_ck_auth grant --principal "enrolled:$EXPECTED_NAME" --selector-kind category \
      --selector antigravity-native --operation read
    ;;
  *)
    vt_die "unknown command '$command'"
    ;;
esac
