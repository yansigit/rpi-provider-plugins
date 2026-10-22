#!/usr/bin/env bash
# Build everything the throwaway vault harness runs, and print where each binary is.
#
#   ck-subc, ck         subconscious: the supervisor/router daemon and the operator CLI
#   ck-claustrum        claustrum: the credential vault module the supervisor runs
#   ck-auth             claustrum: the operator CLI (`ck auth`)
#   vault-check         this directory's driver, which calls rpi's Antigravity vault client
#
# The reference trees under provider-references are read-only. They are copied into
# $(dirname CARGO_TARGET_DIR)/vault-src first, and every build runs in that copy with
# --locked. The driver builds from this worktree, with its own lockfile, and --locked too.
#
# Usage: build.sh
set -eo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

command -v rsync >/dev/null || vt_die "rsync is required to copy the reference trees"

mkdir -p "$VT_SRC_ROOT"
for name in subconscious claustrum; do
  src="$VT_REFERENCES/$name"
  [ -f "$src/Cargo.lock" ] || vt_die "missing reference tree or lockfile: $src"
  rsync -a --delete "$src/" "$VT_SRC_ROOT/$name/"
done

echo "== building ck-subc and ck (subconscious)"
(cd "$VT_SRC_ROOT/subconscious" && cargo build --locked -p subc-core --bin ck-subc --bin ck)

echo "== building ck-claustrum and ck-auth (claustrum)"
(cd "$VT_SRC_ROOT/claustrum" && cargo build --locked -p credentials-module --bin ck-claustrum --bin ck-auth)

echo "== building vault-check (rpi-antigravity-auth driver)"
cargo build --locked --manifest-path "$VT_HARNESS_MANIFEST" --bin vault-check

echo "== binaries"
for name in $VT_BINARIES; do
  path="$(vt_bin "$name")"
  [ -x "$path" ] || vt_die "build did not produce $path"
  printf '%-13s %s\n' "$name" "$path"
done
echo "== versions"
echo "ck-subc:      $(vt_bin ck-subc) --version -> $("$(vt_bin ck-subc)" --version)"
echo "ck-claustrum: $(vt_bin ck-claustrum) --version -> $("$(vt_bin ck-claustrum)" --version)"
echo "ck-auth:      $(vt_bin ck-auth) --version -> $("$(vt_bin ck-auth)" --version)"
echo "build ok"
