#!/usr/bin/env bash
# Install the runtime's pinned Node dependencies for tests and local staging.
#
# A fresh worktree ships `runtime/package-lock.json` but no `runtime/node_modules`,
# so `runtime/main.mjs` dies with ERR_MODULE_NOT_FOUND and every packaged-worker
# test (tests/r02_native_loading.rs, tests/c_tools-reference.mjs, the ignored
# tests/worker.rs suite) fails or hangs. This script installs exactly the locked
# tree without network access, so it works in offline CI and air-gapped lanes.
#
#   packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh            # runtime/
#   packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh --worker   # runtime/ + worker/
#   packages/rpi-cursor-sdk/scripts/ensure-runtime-deps.sh --check    # verify, install nothing
#
# Requires an npm cache (`$npm_config_cache/_cacache`, defaulting to `$HOME/.npm`)
# populated by an earlier online install of the same lockfile; the script fails with
# that instruction when the cache is missing instead of reporting a bare
# ETARGET/ENOTCACHED. Every installed tree is verified against its lockfile, so a
# partial or script-skipped install cannot pass silently.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
npm_flags=(ci --offline --ignore-scripts --no-audit --no-fund)
targets=(runtime)
check_only=0
for argument in "$@"; do
	case "$argument" in
		--worker) targets+=(worker) ;;
		--check) check_only=1 ;;
		-h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}"; exit 0 ;;
		*) echo "ensure-runtime-deps: unknown argument: $argument" >&2; exit 2 ;;
	esac
done

command -v npm >/dev/null || { echo "ensure-runtime-deps: npm is not on PATH (Node 24+ is required)" >&2; exit 1; }
# `set -u` made the old unconditional $HOME reference a hard failure in minimal
# environments, so both cache sources are optional and checked explicitly.
if [ -n "${npm_config_cache:-}" ]; then
	cache="$npm_config_cache"
elif [ -n "${HOME:-}" ]; then
	cache="$HOME/.npm"
else
	echo "ensure-runtime-deps: set npm_config_cache (or HOME) so the offline npm cache can be found" >&2
	exit 1
fi

# Every package the lockfile records must be on disk. `--ignore-scripts` installs
# the tree without running lifecycle scripts, and an interrupted `npm ci` leaves a
# partial tree, so presence of node_modules alone proves nothing.
verify_tree() {
	local target="$1" directory="$2" missing
	if [ ! -f "$directory/node_modules/.package-lock.json" ]; then
		echo "ensure-runtime-deps: $target/node_modules/.package-lock.json is missing; the install is incomplete" >&2
		return 1
	fi
	if ! missing="$(cd "$directory" && node -e '
		const fs = require("node:fs"), path = require("node:path");
		const lock = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
		for (const [key, meta] of Object.entries(lock.packages || {})) {
			if (!key || meta.optional) continue;
			if (!fs.existsSync(path.join(process.cwd(), key))) console.log(key);
		}
	' "$directory/package-lock.json")"; then
		echo "ensure-runtime-deps: could not read $target/package-lock.json" >&2
		return 1
	fi
	if [ -n "$missing" ]; then
		echo "ensure-runtime-deps: $target is missing packages named by its lockfile:" >&2
		printf '%s\n' "$missing" | sed 's/^/  /' >&2
		return 1
	fi
	echo "ensure-runtime-deps: $target/node_modules matches package-lock.json"
}

for target in "${targets[@]}"; do
	directory="$root/$target"
	[ -f "$directory/package-lock.json" ] || { echo "ensure-runtime-deps: missing $target/package-lock.json" >&2; exit 1; }
	if [ "$check_only" = 1 ]; then
		[ -d "$directory/node_modules" ] || { echo "ensure-runtime-deps: $target/node_modules is missing; run this script without --check" >&2; exit 1; }
		verify_tree "$target" "$directory"
		continue
	fi
	if [ ! -d "$cache/_cacache" ]; then
		echo "ensure-runtime-deps: no npm cache at $cache/_cacache; run 'npm ci' once with network access in $target first" >&2
		exit 1
	fi
	echo "ensure-runtime-deps: npm ${npm_flags[*]} in $target"
	# Keep the cache and every temporary file inside the caller's environment, so
	# parallel lanes never share an install directory or a shared $TMPDIR.
	scratch="${TMPDIR:-$directory/.npm-tmp}"
	mkdir -p "$scratch"
	if ! (cd "$directory" && npm_config_cache="$cache" TMPDIR="$scratch" npm "${npm_flags[@]}"); then
		echo "ensure-runtime-deps: offline install failed for $target." >&2
		echo "ensure-runtime-deps: the lockfile's packages are not in $cache/_cacache;" >&2
		echo "ensure-runtime-deps: populate it once with network access ('npm ci') and retry." >&2
		exit 1
	fi
	if [ "$scratch" = "$directory/.npm-tmp" ]; then
		rmdir "$scratch" 2>/dev/null || true
	fi
	verify_tree "$target" "$directory"
done
