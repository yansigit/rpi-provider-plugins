# Prebuilt distribution

`rpi-cursor-sdk` publishes a prebuilt GitHub Release per version. The archive
places one native library beside a private Node runtime; the proprietary
`@cursor/sdk` is **not** part of the archive and is installed from npm after the
extension is installed.

## Archive layout

Every target archive is flat and strict. It contains exactly:

```text
librpi_cursor_sdk.so            (or rpi_cursor_sdk.dll / librpi_cursor_sdk.dylib)
runtime/main.mjs
runtime/dist/rpi/peer.js
runtime/dist/rpi/paths.js
runtime/dist/rpi/runtime.js
runtime/dist/rpi/transport-thread.js
runtime/dist/cursor-sensitive-text.js
runtime/shared/*.mjs
runtime/shared/*.d.mts
runtime/package.json
runtime/package-lock.json
runtime/LICENSE
```

That is 22 entries (one library plus 21 sidecars). The host rejects undeclared
entries, duplicate names, links and special files, and caps an archive at 128
entries. The worker is therefore compiled with `tsc` for development but the
release staging step bundles its entry modules with esbuild into five files:
142 compiled modules would exceed the host's entry limit. Relative imports are
inlined; bare imports (including `@cursor/sdk`, Hono and the Pi peers) stay
external and are resolved from `runtime/node_modules` at execution time.

`runtime/main.mjs` is unchanged: it still imports `./dist/rpi/peer.js`,
`./dist/rpi/paths.js`, `./dist/cursor-sensitive-text.js` and dynamically imports
`./dist/rpi/runtime.js`. The transport worker keeps its own bundle so
`new URL("./transport-thread.js", import.meta.url)` resolves beside `peer.js`.

## Manifest

`[package.metadata.rpi]` in `Cargo.toml` opts this crate into prebuilt releases
and names the generated asset manifest:

```toml
[package.metadata.rpi]
prebuilt = true
assets = ".package/runtime-assets.json"
```

`.github/scripts/prebuilt-assets.py` reads that manifest, requires every declared
SHA-256 to match the staged bytes, refuses `node_modules` paths and native
library extensions, enforces the 128-entry cap, and adds the declarations to the
per-target distribution:

```json
"assets": [
  { "path": "runtime/main.mjs", "sha256": "..." },
  { "path": "runtime/dist/rpi/runtime.js", "sha256": "..." }
]
```

Archives are written deterministically (zero timestamps, fixed modes), read back
and verified before publication. The manifest is published last, after all five
platform archives exist.

## Building the assets

The release workflow builds the worker before packaging:

```sh
cd packages/rpi-cursor-sdk/worker
npm ci --ignore-scripts --no-audit --no-fund
npm run build
cd ..
node scripts/stage-package-runtime.mjs
```

`scripts/stage-package-runtime.mjs` writes `.package/runtime/` (ignored by git)
and `.package/runtime-assets.json`. It resolves esbuild from the worker's
installed dependencies (pinned as an explicit `devDependency` so a transitive
update cannot remove it) and fails if the compiled worker is missing.

## Releasing without crates.io

This crate is `publish = false`: it is distributed only as a GitHub Release, but
its release still needs the same `<package>-v<version>` tag as every published
extension. `release-plz release` skips `cargo publish` for it and still creates
the tag and GitHub Release. Because the crate is never in the cargo registry,
release-plz cannot confirm the version from the registry, so after a release run
check that `rpi-cursor-sdk-v<version>` exists on the release commit. If it does
not, create the tag and empty release manually and let the asset workflow fill
them in:

```sh
commit="$(git rev-parse HEAD)"          # the released, already-tested commit
version="$(cargo metadata --no-deps --format-version 1 --locked \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "rpi-cursor-sdk"))')"
tag="rpi-cursor-sdk-v${version}"

git tag "$tag" "$commit"
git push origin "$tag"
gh release create "$tag" --verify-tag \
  --title "rpi-cursor-sdk v${version}" \
  --notes "Prebuilt Cursor extension assets are attached by release-assets.yml."

# Then build and upload the five platform archives plus rpi-extension.json.
gh workflow run release-assets.yml \
  -f source_ref="$commit" \
  -f package_releases="[\"$tag\"]" \
  -f bundle_tag="rpi-packages-${commit:0:12}"
```

For a package-only retry (no aggregate bundle), add `-f packages_only=true`; the
backfill workflow requires the tag to point at `source_ref`, because
`select-release-tags.py` only selects package tags whose peeled commit equals the
built revision. Do not push a package tag later at a different commit: the
release job then finds no matching package and uploads nothing.

## Install-time npm step

The Cursor SDK is proprietary and is not redistributed. After installing the
extension, the SDK and its peers must be installed into the installed runtime
directory with the packaged lockfile:

```sh
cd "$RPI_CODING_AGENT_DIR/extensions/runtime"   # default: ~/.rpi/agent/extensions/runtime
npm ci --omit=dev --ignore-scripts --no-audit --no-fund
```

The exact `@cursor/sdk@1.0.37` dependency, its platform package and the MIT
Pi 1.3.0 peers come from `runtime/package-lock.json`. No network call is made by
rpi itself; the SDK is only used once it has been installed. Offline execution
requires this step to have completed.

`rpi uninstall rpi-cursor-sdk` removes the library and the files recorded in
`prebuilt.assets`. It intentionally removes only recorded paths, so a
post-install `runtime/node_modules` tree is left behind: delete it manually if a
fully clean removal is required. Upgrades re-run `npm ci` in the same directory.

## Licenses

- `runtime/LICENSE` is the retained upstream `pi-cursor-sdk` MIT license and is
  shipped in every archive.
- The adapter sources under `worker/` remain MIT; `upstream.json` records their
  Git blob identity and `docs/licensing.md` explains the boundary.
- `@cursor/sdk` ships under Anysphere's terms, not MIT. Its `LICENSE.md`,
  version pin and legal review are recorded but the package is never copied into
  an archive. `distribution/cursor-sdk-1.0.37.sha256.json` records the SHA-256 of
  every npm-installed SDK file so packaging can prove no SDK bytes are vendored.

## Verification

```sh
# Reproducible asset manifest (check mode; --write regenerates the fixture)
(cd packages/rpi-cursor-sdk && node tests/r01-package-reference.mjs)

# Packaged runtime opens and the retained dispatcher suite passes (2 tests)
(cd packages/rpi-cursor-sdk && node --test tests/r01-package-runtime.mjs)

# Archive contents, no SDK bytes, scripted install/uninstall roundtrip (9 tests)
python3 packages/rpi-cursor-sdk/tests/r01_package_test.py -v

# Offline invariants: manifest, tracked hashes, SDK pin, catalog, evidence
cargo test -p rpi-cursor-sdk --locked --test r01_package

# Packaging module unit tests (20 tests)
python3 .github/scripts/test_prebuilt_assets.py
```

Evidence: `tests/r01-package-evidence.json`. The scripted roundtrip models the
published host rules; the real `rpi` binary is not used because a prebuilt
install needs the release network and the host is a read-only reference
worktree.
