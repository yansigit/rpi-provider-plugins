# rpi-provider-plugins

Three provider plugins for the [rpi](https://github.com/yansigit/pi-rust) host,
written as native Rust libraries. Each one ports a Pi extension, and each package's
README and parity ledger records how far the port has got.

| Package | Ports | Status |
| --- | --- | --- |
| `rpi-antigravity-auth` | [cortexkit/antigravity-auth](https://github.com/cortexkit/antigravity-auth) by Jens (Antigravity provider, OAuth sign-in, account vault) | Not parity complete. See [`PARITY.md`](packages/rpi-antigravity-auth/PARITY.md). |
| `rpi-multiprovider` | [monotykamary/pi-multiprovider](https://github.com/monotykamary/pi-multiprovider) 0.11.3 by the pi-multiprovider contributors (multi-account routing, virtual providers) | Partial integration. See [its README](packages/rpi-multiprovider/README.md). |
| `rpi-cursor-sdk` | [fitchmultz/pi-cursor-sdk](https://github.com/fitchmultz/pi-cursor-sdk) by Mitch Fultz (Cursor Agents SDK adapter). The Node worker is retained. | Not yet installable or parity complete. See [its README](packages/rpi-cursor-sdk/README.md). |

The upstream sources, their revisions and their licenses are listed in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Pi itself, by Mario Zechner, is
the behavioral reference for all three.

## The host

The plugins load into the rpi host built from the pi-rust fork at
[yansigit/pi-rust](https://github.com/yansigit/pi-rust) revision
`6064a7616217ebb2d90014787bf4f45ce9e597dc`. Each package pins `rpi-plugin-sdk`
to that revision in its `Cargo.toml`, and the test-only `rpi-ai` dependency of
`rpi-multiprovider` is pinned the same way. The plugin API these crates use is
being proposed upstream; until it lands, build against the pinned fork.

## Building

You need Rust 1.78.0 (the minimum supported version) and 1.95.0 (the version the
tests run on), plus Node.js 24 for the Cursor worker. Cargo fetches the pinned
git dependency on the first build.

```sh
# Native libraries
cargo +1.95.0 build --release --locked \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider

# Cursor worker and its runtime dependencies (Node.js 24)
(cd packages/rpi-cursor-sdk/worker && npm ci --ignore-scripts && npm run build)
(cd packages/rpi-cursor-sdk/runtime && npm ci --ignore-scripts)
(cd packages/rpi-cursor-sdk && node tests/stage-runtime.mjs)
```

`node scripts/stage-package-runtime.mjs` in `packages/rpi-cursor-sdk` produces the
sidecar asset set for a release archive under `.package/`. It is not needed to
build or test.

To run the tests:

```sh
cargo +1.95.0 test --locked \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider -- --skip real_dll
```

The `real_dll` tests load the library into a built rpi host, which is set with
`RPI_TEST_HOST`, so they are skipped here. Tests that need a reference checkout
are either `#[ignore]`d with the reason or skip when their inputs are unset. See
[CONTRIBUTING.md](CONTRIBUTING.md).

To install a package into rpi from this checkout:

```sh
rpi install rpi-multiprovider --path packages/rpi-multiprovider
```

## Antigravity OAuth client

`rpi-antigravity-auth` needs an OAuth client identity, and this repository does
not contain one. The package reads two values from the build environment, or
from the runtime environment when they are not set at build time:

- `RPI_ANTIGRAVITY_OAUTH_CLIENT_ID`
- `RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET`

```sh
RPI_ANTIGRAVITY_OAUTH_CLIENT_ID=<client id> \
RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET=<client secret> \
  cargo build --release --locked -p rpi-antigravity-auth
```

Values set at build time are compiled into the library, so treat that artifact as
sensitive. Without both values, sign-in and token refresh fail before any request
is sent, and the error names both variables. CI and the tests do not need them.
Release builds read them from repository secrets and stop before compiling if
either is empty.

## Verification

The parity gates, their evidence, and the commands that produce them are described
in [`docs/provider-parity-verification.md`](docs/provider-parity-verification.md).
Those gates need inputs that are not in this repository, listed in
[CONTRIBUTING.md](CONTRIBUTING.md).

## Repository layout

- `packages/` - the three plugin crates
- `crates/rpi-node-collation` - the collation data that Antigravity and
  Multiprovider use for locale ordering
- `vendor/` - private integration copies of cortexkit crates, see
  [`vendor/README.md`](vendor/README.md)
- `tools/` - parity harnesses for the combined and RPC comparisons
- `docs/` - evidence, the prebuilt distribution protocol and its schema, and the
  SDK cohort record
- `catalog/packages.json` - the package catalog entries
- `.github/` - CI and release workflows

## License

MIT, see [LICENSE](LICENSE). Third-party components keep their own licenses, as
described in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
