#!/usr/bin/env bash
# Self-test for ensure-runtime-deps.sh: no unbound $HOME, and an installed tree is
# verified against its lockfile instead of being assumed complete.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
package="$(cd "$here/.." && pwd)"
script="$here/ensure-runtime-deps.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/ensure-runtime-deps-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
	echo "FAIL: $*" >&2
	failures=$((failures + 1))
}

# 1. HOME unset must produce an instruction, not an unbound-variable crash.
set +e
output="$(cd "$package" && env -u HOME -u npm_config_cache "$script" --check 2>&1)"
status=$?
set -e
[ "$status" = 1 ] || fail "HOME unset exited $status, expected 1"
case "$output" in
	*"unbound variable"*) fail "HOME unset fell through to an unbound variable: $output" ;;
	*"set npm_config_cache"*) ;;
	*) fail "HOME unset did not explain the cache requirement: $output" ;;
esac

# 2. A complete tree passes --check.
set +e
output="$(cd "$package" && "$script" --check 2>&1)"
status=$?
set -e
[ "$status" = 0 ] || fail "--check failed on the staged tree: $output"
case "$output" in
	*"matches package-lock.json"*) ;;
	*) fail "--check did not verify the tree: $output" ;;
esac

# 3. A tree that is missing a locked package must be reported, not accepted.
root="$scratch/root"
mkdir -p "$root/scripts" "$root/runtime"
cp "$script" "$root/scripts/ensure-runtime-deps.sh"
ln -s "$package/runtime/node_modules" "$root/runtime/node_modules"
node -e '
	const fs = require("node:fs");
	const lock = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
	lock.packages["node_modules/definitely-not-installed"] = {version: "0.0.0"};
	fs.writeFileSync(process.argv[2], JSON.stringify(lock, null, 2));
' "$package/runtime/package-lock.json" "$root/runtime/package-lock.json"
set +e
output="$(cd "$root" && "$scratch/root/scripts/ensure-runtime-deps.sh" --check 2>&1)"
status=$?
set -e
[ "$status" = 1 ] || fail "an incomplete tree exited $status, expected 1"
case "$output" in
	*"definitely-not-installed"*) ;;
	*) fail "an incomplete tree was not reported: $output" ;;
esac

if [ "$failures" -ne 0 ]; then
	echo "ensure-runtime-deps: $failures check(s) failed" >&2
	exit 1
fi
echo "ensure-runtime-deps: self-test passed"
