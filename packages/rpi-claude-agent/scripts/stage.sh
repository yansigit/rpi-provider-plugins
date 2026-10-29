#!/usr/bin/env bash
# Stage an installable rpi-claude-agent directory:
#   OUT/librpi_claude_agent.so  OUT/runtime/{main.mjs,package.json,package-lock.json,node_modules}
#
#   packages/rpi-claude-agent/scripts/stage.sh OUT
#
# Builds the release library (honours CARGO_TARGET_DIR) and installs the worker's
# lockfile-pinned dependencies (the Claude Agent SDK and its bundled Claude Code
# executable) with npm ci. Load the result with `rpi --extensions-dir OUT`.
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
package="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="$(cd "$package/../.." && pwd)"
out="$(realpath -m "$1")"

(cd "$root" && cargo build --release --locked -p rpi-claude-agent)
rm -rf "$out"
mkdir -p "$out/runtime"
cp "$package/worker/main.mjs" "$package/worker/package.json" "$package/worker/package-lock.json" "$out/runtime/"
(cd "$out/runtime" && npm ci --ignore-scripts --no-audit --no-fund --omit=dev)
case "$(uname -s)" in Darwin) library=librpi_claude_agent.dylib ;; *) library=librpi_claude_agent.so ;; esac
cp "${CARGO_TARGET_DIR:-$root/target}/release/$library" "$out/"
claude="$(ls "$out"/runtime/node_modules/@anthropic-ai/claude-agent-sdk-*/claude 2>/dev/null | head -1)"
echo "staged rpi-claude-agent at $out"
echo "bundled Claude Code: ${claude:-not found for this platform}"
