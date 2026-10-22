#!/usr/bin/env bash
# Start an isolated, throwaway ck-subc supervisor with the claustrum vault module registered
# under it. Loopback only, ephemeral port, no shared state.
#
# Usage: start.sh <state-dir>
#   <state-dir>  an absolute path whose parent already exists and is private (for example
#                $HOME/.vault-throwaway.XXXXXX from mktemp). It must not exist, or be empty.
#
# Result: the daemon keeps running (detached) until teardown.sh <state-dir>. The last lines
# name the connection file rpi must use. rpi's discovery (rpi-antigravity-auth
# src/vault/discovery.rs) checks SUBC_CONNECTION_FILE first, so that variable is the
# contract. <state-dir>/rpi.env exports it, with the vault directory the driver uses.
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

[ $# -eq 1 ] || vt_die "usage: start.sh <state-dir>"
case "$1" in /*) ;; *) vt_die "the state directory must be an absolute path" ;; esac

for name in $VT_BINARIES; do
  [ -x "$(vt_bin "$name")" ] || vt_die "missing $(vt_bin "$name"); run build.sh first"
done

parent="$(dirname "$1")"
[ -d "$parent" ] || vt_die "parent directory $parent does not exist"
vt_check_ancestors "$parent"
if [ -e "$1" ]; then
  [ -d "$1" ] || vt_die "$1 exists and is not a directory"
  [ -z "$(ls -A "$1")" ] || vt_die "$1 is not empty"
fi
mkdir -m 700 -p "$1"
vt_paths "$(cd "$1" && pwd -P)"
vt_check_ancestors "$VT_STATE"

start_failed() {
  echo "vault-throwaway: start failed; stopping what was started (state kept at $VT_STATE)" >&2
  "$VT_SCRIPTS_DIR/teardown.sh" --keep-state "$VT_STATE" >&2 || true
}
trap 'rc=$?; [ "$rc" -eq 0 ] || start_failed; exit "$rc"' EXIT

mkdir -p "$VT_HOME" "$VT_XDG_CONFIG/cortexkit" "$VT_XDG_DATA" "$VT_XDG_STATE" \
  "$VT_LOG_DIR" "$VT_BIN_STATE" "$VT_STATE/tmp" "$VT_FAKE_DIR" "$VT_RPI_AGENT"
mkdir -m 700 -p "$VT_RUN_DIR" "$VT_KEY_DIR"
chmod 700 "$VT_STATE/tmp" "$VT_FAKE_DIR" "$VT_RPI_AGENT"

printf '{"schema":1,"owner":"tools/vault-throwaway/start.sh","started":"%s"}\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$VT_MARKER"

# Binaries are reached through symlinks inside the state directory, so every process this
# rig starts shows a path under <state-dir>/bin in `ps`, which teardown relies on.
for name in ck-subc ck ck-claustrum ck-auth vault-check; do
  ln -sfn "$(vt_bin "$name")" "$VT_BIN_STATE/$name"
done

# Supervisor configuration: one reserved module, the vault, spawned by ck-subc. The vault
# reads its master key from CK_MASTER_KEY_PATH, which must agree with the key-source record
# that bootstrap writes. The rig drains in 5 s rather than 30 s so teardown is quick; a
# throwaway vault has nothing to flush.
cat >"$VT_XDG_CONFIG/cortexkit/subc.jsonc" <<EOF
{
  "version": 1,
  "port": 0,
  "drain_timeout_ms": 5000,
  "storage": { "backend": "sqlite", "data_home": "$VT_XDG_DATA" },
  "modules": {
    "claustrum": {
      "program": "$VT_BIN_STATE/ck-claustrum",
      "args": [],
      "env": { "CK_MASTER_KEY_PATH": "$VT_KEY_FILE" },
      "enabled": true,
      "reserved": true
    }
  }
}
EOF

if [ ! -f "$VT_KEY_FILE" ]; then
  # Offline bootstrap: the daemon is not running yet, so ck-auth writes the store directly.
  # The key lives outside the data directory, as the operator runbook requires.
  vt_env_run "$VT_BIN_STATE/ck-auth" bootstrap --data-dir "$VT_DATA_DIR" --key-path "$VT_KEY_FILE" \
    >"$VT_LOG_DIR/bootstrap.out" 2>&1 || { cat "$VT_LOG_DIR/bootstrap.out" >&2; vt_die "bootstrap failed"; }
fi
[ -f "$VT_KEY_FILE" ] || vt_die "bootstrap did not create $VT_KEY_FILE"

# Detach the supervisor into its own session so it outlives this script, and send its output
# to a log inside the state directory.
setsid -f "${VT_ENV[@]}" "$VT_BIN_STATE/ck-subc" >"$VT_LOG_DIR/ck-subc.out" 2>&1 </dev/null

deadline=$((SECONDS + 40))
until [ -s "$VT_CONN" ]; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    tail -n 20 "$VT_LOG_DIR/ck-subc.out" >&2
    vt_die "ck-subc did not publish $VT_CONN within 40 s"
  fi
  sleep 0.2
done

# Health: the vault answers the operator status path (a live round trip through the module).
until vt_ck_auth status >"$VT_LOG_DIR/status.out" 2>&1 && grep -q '^vault: ok' "$VT_LOG_DIR/status.out"; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    cat "$VT_LOG_DIR/status.out" >&2
    tail -n 20 "$VT_LOG_DIR/ck-subc.out" >&2
    vt_die "the vault did not report healthy within the start window"
  fi
  sleep 0.5
done

module_line="$(vt_env_run "$VT_BIN_STATE/ck" --subc "$VT_CONN" module list | awk '$1=="claustrum"')"
[ "$(awk '{print $2}' <<<"$module_line")" = "running" ] \
  || vt_die "claustrum is not running under ck-subc: ${module_line:-no row}"

read -r daemon_pid ports < <(python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(d["pid"], " ".join(sorted({str(e["port"]) for e in d["endpoints"]})))
' "$VT_CONN")
echo "$daemon_pid" >"$VT_STATE/ck-subc.pid"

printf 'export SUBC_CONNECTION_FILE=%q\nexport RPI_VAULT_DIR=%q\n' "$VT_CONN" "$VT_VAULT_DIR" >"$VT_ENV_FILE"

echo "started: $VT_STATE"
echo "supervisor pid: $daemon_pid (loopback ports: $ports)"
echo "module: $module_line"
echo "data dir (ck-auth --data-dir): $VT_DATA_DIR"
echo "rpi connection file (SUBC_CONNECTION_FILE): $VT_CONN"
echo "rpi vault directory (driver): $VT_VAULT_DIR"
echo "environment for rpi and the driver: $VT_ENV_FILE"
trap - EXIT
