#!/usr/bin/env bash
# Shared helpers for the throwaway CortexKit vault harness. Sourced by the other scripts;
# never executed directly.
#
# Isolation model (see README.md): every daemon and operator command runs under `env -i`
# with HOME, XDG_CONFIG_HOME, XDG_DATA_HOME, XDG_STATE_HOME and XDG_RUNTIME_DIR pointing
# inside one state directory. Nothing reads or writes ~/.pi, ~/.config/cortexkit, or a
# real CortexKit install. Daemons bind loopback only.

VT_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
VT_MARKER_NAME=".vault-throwaway-state"

# Lane environment: Node 24 first on PATH, LC_ALL, and a private Cargo target dir.
VT_PARITY_ENV="${VT_PARITY_ENV:-/home/user/Projects/.parity-env.sh}"
if [ ! -f "$VT_PARITY_ENV" ]; then
  echo "vault-throwaway: missing lane environment file $VT_PARITY_ENV" >&2
  return 1 2>/dev/null || exit 1
fi
# shellcheck disable=SC1090
source "$VT_PARITY_ENV"
lane_env vault
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

VT_TARGET_DIR="$CARGO_TARGET_DIR"
VT_BIN_DIR="$VT_TARGET_DIR/debug"
VT_SRC_ROOT="$(dirname "$VT_TARGET_DIR")/vault-src"
VT_REFERENCES="${VT_PROVIDER_REFERENCES:-/home/user/Projects/provider-references}"
VT_HARNESS_MANIFEST="$VT_SCRIPTS_DIR/harness/Cargo.toml"
VT_BINARIES="ck-subc ck ck-claustrum ck-auth vault-check"

vt_die() {
  echo "vault-throwaway: $*" >&2
  exit 1
}

vt_bin() {
  printf '%s/%s\n' "$VT_BIN_DIR" "$1"
}

# Set every path a state directory uses. Pure string derivation: no filesystem access.
vt_paths() {
  VT_STATE="$1"
  VT_HOME="$VT_STATE/home"
  VT_XDG_CONFIG="$VT_STATE/xdg/config"
  VT_XDG_DATA="$VT_STATE/xdg/data"
  VT_XDG_STATE="$VT_STATE/xdg/state"
  VT_RUN_DIR="$VT_STATE/run"
  VT_CONN="$VT_RUN_DIR/subc-connection.json"
  VT_KEY_DIR="$VT_STATE/keys"
  VT_KEY_FILE="$VT_KEY_DIR/master.key"
  VT_DATA_DIR="$VT_XDG_DATA/cortexkit/claustrum"
  VT_BIN_STATE="$VT_STATE/bin"
  VT_LOG_DIR="$VT_STATE/logs"
  VT_FAKE_DIR="$VT_STATE/fake"
  VT_RPI_AGENT="$VT_STATE/agent"
  VT_VAULT_DIR="$VT_RPI_AGENT/antigravity-auth-vault"
  VT_ENV_FILE="$VT_STATE/rpi.env"
  VT_MARKER="$VT_STATE/$VT_MARKER_NAME"
  # The one environment every process in this rig runs under (see the isolation model).
  VT_ENV=(
    env -i
    PATH=/usr/local/bin:/usr/bin:/bin
    HOME="$VT_HOME"
    XDG_CONFIG_HOME="$VT_XDG_CONFIG"
    XDG_DATA_HOME="$VT_XDG_DATA"
    XDG_STATE_HOME="$VT_XDG_STATE"
    XDG_RUNTIME_DIR="$VT_RUN_DIR"
    TMPDIR="$VT_STATE/tmp"
    LC_ALL=en_US.UTF-8
    SUBC_CONNECTION_FILE="$VT_CONN"
  )
}

# Refuse to operate on anything that is not a state directory created by start.sh.
vt_require_state() {
  [ -n "${1:-}" ] || vt_die "a state directory argument is required"
  local state
  state="$(cd "$1" 2>/dev/null && pwd -P)" || vt_die "no such state directory: $1"
  [ -f "$state/$VT_MARKER_NAME" ] || vt_die "$state is not a vault-throwaway state directory (no marker)"
  vt_paths "$state"
}

# Run a command with a clean environment rooted inside the state directory.
# SUBC_CONNECTION_FILE is set explicitly so no discovery rung can reach another daemon.
vt_env_run() {
  "${VT_ENV[@]}" "$@"
}

# Operator CLI: ck-auth against this state's daemon and vault, offline if it is stopped.
vt_ck_auth() {
  vt_env_run "$VT_BIN_STATE/ck-auth" "$@" \
    --subc "$VT_CONN" --data-dir "$VT_DATA_DIR" --key-path "$VT_KEY_FILE"
}

# Every process whose argv[0] lives under this state's bin/ directory, one pid per line.
# Matching argv[0] (not the whole command line) keeps our own shells and greps out of it.
vt_state_pids() {
  local prefix="$VT_BIN_STATE/" pid argv0
  for dir in /proc/[0-9]*; do
    pid="${dir#/proc/}"
    [ "$pid" = "$$" ] && continue
    argv0="$(head -c 4096 "$dir/cmdline" 2>/dev/null | tr '\0' '\n' | head -n 1)"
    case "$argv0" in
      "$prefix"*) echo "$pid" ;;
    esac
  done
}

# Listening TCP sockets owned by the given pids, as "addr:port pid" lines.
vt_listeners_of() {
  local pids="$1" line
  [ -n "$pids" ] || return 0
  ss -H -ltnp 2>/dev/null | while read -r line; do
    for pid in $pids; do
      case "$line" in
        *"pid=$pid,"*|*"pid=$pid)"*) echo "$line" ;;
      esac
    done
  done
}

# Refuse a state directory whose ancestors let another local user replace the connection
# file. ck-subc enforces the same rule; checking first gives a clear message.
vt_check_ancestors() {
  local dir="$1" mode
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    mode=$(stat -c %a "$dir")
    # group- or other-writable directory on the path to the connection file
    if [ $(( 0$mode & 022 )) -ne 0 ]; then
      vt_die "$dir is mode $mode (group/other writable); choose a state directory under a private path such as \$HOME/.vault-throwaway.XXXXXX"
    fi
    dir="$(dirname "$dir")"
  done
}
