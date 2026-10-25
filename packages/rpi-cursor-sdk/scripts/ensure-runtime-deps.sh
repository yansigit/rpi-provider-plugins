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
# Requires an npm cache (`~/.npm/_cacache`) populated by an earlier online
# install of the same lockfile; the script fails with that instruction when the
# cache is missing instead of reporting a bare ETARGET/ENOTCACHED.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
npm_flags=(ci --offline --ignore-scripts --no-audit --no-fund)
targets=(runtime)
check_only=0
for argument in "$@"; do
	case "$argument" in
		--worker) targets+=(worker) ;;
		--check) check_only=1 ;;
		-h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}"; exit 0 ;;
		*) echo "ensure-runtime-deps: unknown argument: $argument" >&2; exit 2 ;;
	esac
done

command -v npm >/dev/null || { echo "ensure-runtime-deps: npm is not on PATH (Node 24+ is required)" >&2; exit 1; }
cache="${npm_config_cache:-$HOME/.npm}"

for target in "${targets[@]}"; do
	directory="$root/$target"
	[ -f "$directory/package-lock.json" ] || { echo "ensure-runtime-deps: missing $target/package-lock.json" >&2; exit 1; }
	if [ "$check_only" = 1 ]; then
		[ -d "$directory/node_modules" ] || { echo "ensure-runtime-deps: $target/node_modules is missing; run this script without --check" >&2; exit 1; }
		echo "ensure-runtime-deps: $target/node_modules present"
		continue
	fi
	if [ ! -d "$cache/_cacache" ]; then
		echo "ensure-runtime-deps: no npm cache at $cache/_cacache; run 'npm ci' once with network access in $target first" >&2
		exit 1
	fi
	echo "ensure-runtime-deps: npm ${npm_flags[*]} in $target"
	# Keep the cache and every temporary file inside the caller's environment, so
	# parallel lanes never share an install directory.
	if ! (cd "$directory" && npm "${npm_flags[@]}"); then
		echo "ensure-runtime-deps: offline install failed for $target." >&2
		echo "ensure-runtime-deps: the lockfile's packages are not in $cache/_cacache;" >&2
		echo "ensure-runtime-deps: populate it once with network access ('npm ci') and retry." >&2
		exit 1
	fi
done
