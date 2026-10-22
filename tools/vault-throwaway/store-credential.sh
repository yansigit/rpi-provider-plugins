#!/usr/bin/env bash
# Store one Antigravity (Google) OAuth credential in a throwaway vault and grant rpi's
# enrolled consumer read access to the antigravity-native category.
#
# The input is a Pi-format auth.json, the shape Pi and rpi keep on disk, with one provider
# entry keyed "google":
#   {"google": {"type": "oauth", "refresh": "<refresh>|<projectId>",
#               "access": "<access token>", "expires": <epoch ms>}}
# The antigravity refresh adapter reads the packed refresh form, so the project travels in
# the refresh field. ck-auth reads the file; this script never prints a token, a refresh
# value or the file's contents. It checks the file's permissions and key names only.
#
# Usage: store-credential.sh <state-dir> <auth-json-file> --account-id ID [--email EMAIL] [--replace]
#   --account-id  non-secret account identity; rpi refuses a credential without one
#   --email       optional non-secret email shown in listings
#   --replace     overwrite an existing antigravity:google record (keeps its handles)
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

[ $# -ge 2 ] || vt_die "usage: store-credential.sh <state-dir> <auth-json-file> --account-id ID [--email EMAIL] [--replace]"
vt_require_state "$1"
input="$2"
shift 2

account_id=""
email=""
replace=0
while [ $# -gt 0 ]; do
  case "$1" in
    --account-id) account_id="${2:-}"; shift 2 ;;
    --email) email="${2:-}"; shift 2 ;;
    --replace) replace=1; shift ;;
    *) vt_die "unknown option $1" ;;
  esac
done
[ -n "$account_id" ] || vt_die "--account-id is required (rpi refuses a credential with no identity)"

[ -f "$input" ] || vt_die "no such file: $input"
mode="$(stat -c %a "$input")"
[ $(( 0$mode & 077 )) -eq 0 ] || vt_die "$input is mode $mode; make it owner-only (chmod 600) before storing it"

# Structure check only: key names and types, no values.
python3 -I - "$input" <<'PY' || vt_die "$input is not a Pi-format auth.json with a google oauth entry"
import json, sys
with open(sys.argv[1], "rb") as handle:
    entry = json.load(handle).get("google", {})
ok = (
    entry.get("type") == "oauth"
    and isinstance(entry.get("refresh"), str) and entry["refresh"] != ""
    and isinstance(entry.get("access"), str)
    and isinstance(entry.get("expires"), int)
)
sys.exit(0 if ok else 1)
PY

args=(import --source pi --provider google --id antigravity:google --json "$input" --account-id "$account_id")
[ -z "$email" ] || args+=(--email "$email")
[ "$replace" -eq 0 ] || args+=(--replace)
vt_ck_auth "${args[@]}"

vt_ck_auth grant --principal enrolled:antigravity-auth-pi --selector-kind category \
  --selector antigravity-native --operation read

vt_ck_auth list
