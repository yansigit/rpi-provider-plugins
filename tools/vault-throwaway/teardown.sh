#!/usr/bin/env bash
# Stop everything a start.sh state directory started, prove that nothing is left, then
# delete the state directory.
#
# Usage: teardown.sh [--keep-state] <state-dir>
#   --keep-state  stop and verify, but leave the state directory for inspection
#
# "Nothing left" means: no process whose argv[0] is under <state-dir>/bin (the supervisor and
# the vault module), no listening socket owned by those processes, and no listener on the
# loopback ports recorded in the connection file. If anything survives, the state directory
# is kept and the script exits non-zero.
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

keep=0
if [ "${1:-}" = "--keep-state" ]; then
  keep=1
  shift
fi
vt_require_state "${1:-}"

ports=""
if [ -s "$VT_CONN" ]; then
  ports="$(python3 -I -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(" ".join(sorted({str(e["port"]) for e in d.get("endpoints", [])})))
except Exception:
    pass
' "$VT_CONN")"
fi

pids="$(vt_state_pids | tr '\n' ' ')"
if [ -n "${pids// /}" ]; then
  echo "stopping: $pids"
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
  deadline=$((SECONDS + 20))
  while [ -n "$(vt_state_pids)" ] && [ "$SECONDS" -lt "$deadline" ]; do
    sleep 0.3
  done
  survivors="$(vt_state_pids | tr '\n' ' ')"
  if [ -n "${survivors// /}" ]; then
    echo "still running after SIGTERM, sending SIGKILL: $survivors" >&2
    # shellcheck disable=SC2086
    kill -KILL $survivors 2>/dev/null || true
    sleep 1
  fi
fi

problems=0
remaining="$(vt_state_pids | tr '\n' ' ')"
if [ -n "${remaining// /}" ]; then
  echo "processes survive teardown: $remaining" >&2
  problems=1
fi

# Listeners: any socket owned by a pid of ours, or any listener on the recorded ports.
owned="$(vt_listeners_of "$pids")"
if [ -n "$owned" ]; then
  echo "listening sockets still owned by this rig:" >&2
  echo "$owned" >&2
  problems=1
fi
for port in $ports; do
  if ss -H -ltn 2>/dev/null | awk -v p=":$port" '$4 ~ (p "$") {found=1} END {exit !found}'; then
    echo "port $port is still listening after teardown" >&2
    problems=1
  fi
done

if [ "$problems" -ne 0 ]; then
  echo "teardown NOT clean; state kept at $VT_STATE for inspection" >&2
  exit 1
fi

if [ "$keep" -eq 1 ]; then
  echo "teardown clean (state kept): $VT_STATE"
  exit 0
fi

rm -rf -- "$VT_STATE"
echo "teardown clean: no vault-throwaway processes, no listening sockets on ports [$ports]; state removed: $VT_STATE"
