#!/usr/bin/env bash
# Stage an installable rpi-cursor-lite directory:
#   OUT/librpi_cursor_lite.so  OUT/runtime/{main.mjs,dist,shared,...}  OUT/runtime/node_modules (incl. @cursor/sdk)
#
#   packages/rpi-cursor-lite/scripts/stage.sh OUT
#
# Builds the release library (honours CARGO_TARGET_DIR), compiles and bundles the
# shared worker with rpi-cursor-sdk's own stage-package-runtime.mjs, and copies the
# lockfile-pinned runtime dependencies. Missing node_modules are installed with
# rpi-cursor-sdk/scripts/ensure-runtime-deps.sh, which is offline and needs a warm
# npm cache. Load the result with `rpi --extension OUT/librpi_cursor_lite.so`.
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
sdk="$root/packages/rpi-cursor-sdk"
out="$(realpath -m "$1")"

if [ ! -d "$sdk/worker/node_modules" ] || [ ! -d "$sdk/runtime/node_modules" ]; then
	"$sdk/scripts/ensure-runtime-deps.sh" --worker
fi
(cd "$root" && cargo build --release --locked -p rpi-cursor-lite)
(cd "$sdk/worker" && node scripts/build.mjs)
# Wipes OUT, then writes OUT/runtime and OUT/runtime-assets.json.
node "$sdk/scripts/stage-package-runtime.mjs" --out "$out"
case "$(uname -s)" in Darwin) library=librpi_cursor_lite.dylib ;; *) library=librpi_cursor_lite.so ;; esac
cp "${CARGO_TARGET_DIR:-$root/target}/release/$library" "$out/"
cp -R "$sdk/runtime/node_modules" "$out/runtime/node_modules"
echo "staged rpi-cursor-lite at $out"
