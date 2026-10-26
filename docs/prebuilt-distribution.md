# Prebuilt extension distribution protocol v1

This protocol maps an extension's Cargo package name and **package version** to
its prebuilt native library. A repository may publish many extensions with
independent versions; the workspace version and latest repository Release are
not substitutes for the requested package version.

## Release workflow

This repository does not publish to crates.io. Its packages are `publish = false`,
and each release is made by hand:

1. Tag the released commit as `<package>-v<version>` and create a GitHub Release
   for that tag.
2. Run the `release-assets.yml` workflow manually with `source_ref` set to that
   commit and `package_releases` set to a JSON array of the tags.

The workflow builds the requested libraries from the exact commit on Windows x64,
Linux x64/ARM64, and macOS Intel/Apple Silicon. It attaches each package's
archives to its Release, then publishes the `rpi-extension.json` manifest
(download URLs, SHA-256 hashes, target triples, library names and SDK ABI) last,
after all five platforms are present. Release builds need the repository secrets
`RPI_ANTIGRAVITY_OAUTH_CLIENT_ID` and `RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET`; the
workflow stops before compiling if either is empty.

Host developers can implement prebuilt installation using the discovery
protocol below and its [JSON schema](rpi-extension.schema.json). Use the
requested package's versioned Release manifest; packages in a repository do not
all share one version.

## Discovery

1. Resolve the package and requested version through crates.io. With no version
   specified, select the latest non-yanked stable version. Use the package's
   `repository` metadata to locate the GitHub repository; do not infer an owner
   from the package name. The official package catalog also includes
   `repository` and `prebuilt_manifest` for each recorded version.
   For example, `GET https://crates.io/api/v1/crates/rpi-goal` exposes
   `crate.repository` as `https://github.com/pi-rust/rpi-package` and provides
   versions in `versions[].num` with their `yanked` flags.
2. For a repository at `https://github.com/<owner>/<repo>`, request:

   ```text
   https://github.com/<owner>/<repo>/releases/download/<package>-v<version>/rpi-extension.json
   ```

3. Require `schema_version == 1` and check that `name`, `version`, `repository`
   and `tag` match the resolved package. Match an exact Rust target triple from
   `targets`, including the libc/ABI variant. GNU Linux archives cannot be
   substituted for musl, Windows MSVC for GNU, or x86_64 for aarch64.
4. Check the recorded plugin ABI against the host's supported ABI. `sdk.version`
   is the actual SDK dependency compiled into the library, not the manifest's
   minimum version requirement. The host's existing entrypoint validation must
   also check ABI and API struct size when loading the extension.
5. Download the selected archive, check its byte length and SHA-256, and
   extract only the declared library. Check the extracted library's SHA-256,
   then install it using the host's normal transactional extension installer.

The distribution is source-agnostic once discovered: multiple packages can
resolve to the same repository, and third-party repositories can implement the
same protocol. A host may provide an explicit repository override or trusted
registry mapping for extensions that have not been published to crates.io.

`rpi-extension.json` is uploaded **after all platform archives are available**.
A missing manifest means the requested version has no complete prebuilt release
yet. Report that state or retry later; never silently install another package's
latest bundle, downgrade the version, or compile source without the user's
selected fallback policy. Checksums provide download integrity; the repository
selected during discovery remains the publisher trust boundary.

## Package Release layout

Example for `rpi-goal` version `0.1.7` in this repository:

```text
Release tag: rpi-goal-v0.1.7
  rpi-extension.json
  rpi-goal-0.1.7-x86_64-pc-windows-msvc.zip
  rpi-goal-0.1.7-x86_64-unknown-linux-gnu.tar.gz
  rpi-goal-0.1.7-aarch64-unknown-linux-gnu.tar.gz
  rpi-goal-0.1.7-x86_64-apple-darwin.tar.gz
  rpi-goal-0.1.7-aarch64-apple-darwin.tar.gz
```

Each package archive is flat and contains that extension's library and, when
declared by the package, optional sidecar assets. The manifest's `assets` list
declares their paths and SHA-256 hashes; when present (including an empty list),
the archive must contain exactly the library and those assets. Cargo library
target names determine file names, including `lib` on Unix; consumers must read
`library.path` rather than derive it from the package name. The host currently
accepts at most 128 archive entries and 256 MiB expanded total per archive;
individual sidecars are additionally limited to 128 MiB by the packaging
pipeline. Sidecars must be safe relative paths and may not collide with the
host's `rpi-extension.json` manifest.

Example manifest showing one target (real releases include all five):

```json
{
  "schema_version": 1,
  "name": "rpi-goal",
  "version": "0.1.7",
  "repository": "https://github.com/pi-rust/rpi-package",
  "tag": "rpi-goal-v0.1.7",
  "source_commit": "0123456789abcdef0123456789abcdef01234567",
  "sdk": { "version": "0.3.20", "plugin_abi": 4 },
  "targets": {
    "x86_64-pc-windows-msvc": {
      "url": "https://github.com/pi-rust/rpi-package/releases/download/rpi-goal-v0.1.7/rpi-goal-0.1.7-x86_64-pc-windows-msvc.zip",
      "format": "zip",
      "size": 123456,
      "sha256": "<64 lowercase hexadecimal characters>",
      "library": {
        "path": "rpi_goal.dll",
        "sha256": "<64 lowercase hexadecimal characters>"
      },
      "assets": [
        { "path": "runtime/main.mjs", "sha256": "<64 lowercase hexadecimal characters>" }
      ]
    }
  }
}
```

The machine-readable contract is [rpi-extension.schema.json](rpi-extension.schema.json).
Unknown fields may be ignored; unknown schema versions must be rejected.

## Bundles and build provenance

There is no aggregate bundle in this repository; each package Release is
self-contained. All five builds use the same exact Git commit. Linux GNU builds use Ubuntu
22.04; normal runtime dependencies such as glibc, ALSA and OpenSSL still apply.
The supported targets are Windows x64 MSVC, Linux x64/ARM64 GNU, and macOS
Intel/Apple Silicon. Windows ARM64, Linux musl and 32-bit platforms are not
currently built. The loader performs the final host compatibility check.

## Retrying an asset build

The build artifacts are kept for 14 days. Before it uploads the discovery
manifest, the publish job verifies the source commit, package identity, SDK
identity and checksums of every platform. A release whose manifest is already
published keeps its assets on retry, and an unfinished release can finish
uploading safely. To recover a failed build, rerun the failed jobs or run
`release-assets.yml` again with the same commit and tags.
