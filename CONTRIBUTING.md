# Contributing

Changes are welcome as pull requests against `main`. Commits follow Conventional
Commits with a body that explains the change.

## Checks

CI runs these on every pull request (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)).
Run them before you push:

```sh
cargo +1.78.0 check --locked --all-targets \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider      # with RUSTFLAGS=-Dwarnings
cargo +1.95.0 test --locked \
  -p rpi-antigravity-auth -p rpi-cursor-sdk -p rpi-multiprovider -- --skip real_dll
xargs rustfmt --edition 2021 --check < .github/rustfmt-files.txt
python3 -m unittest discover -s .github/scripts
```

The Cursor tests need the worker built and staged first; the README shows the
commands. Rust 1.78 compatibility is required, so do not use APIs newer than that
(for example `LazyLock`, `is_none_or` or `&raw`).

Format only the files that already pass `rustfmt`. The file list is
`.github/rustfmt-files.txt`; the other provider files have formatting drift that
predates this repository. Add a file to the list when you format it.

Do not add `#[ignore]` to a test to get a green run, and do not weaken an
assertion to make a test pass. Existing `#[ignore]` attributes state their reason.

## Rules for changes

- Never commit OAuth client values, tokens or machine-specific paths. Use the
  environment variables listed below.
- Do not hand-edit generated evidence: `docs/provider-parity-sdk-cohort.json`, the
  files under `docs/evidence/`, and the `*-evidence.json` files under `packages/`
  and `tests/`. Regenerate them with the script that owns them, and check that the
  script's `--check` mode passes.
- Every behavior change needs a test. A guard script or test needs a mutation check
  that shows it fails when the guarded behavior is broken.

## Local-only parity inputs

Some tests and evidence scripts read inputs that are not in this repository. They
are either `#[ignore]`d with a reason or skip when the input is missing, so CI
does not need them. To run them, provide the inputs below.

| Input | Used by | How to provide it |
| --- | --- | --- |
| Reference checkout `provider-references` (the Pi 1.3.0 bundle and the three upstream extensions, read-only) | Cursor reference scripts, `tools/combined`, `tools/parity-rpc`, Antigravity reference scripts | `RPI_PROVIDER_REFERENCES`, default `../provider-references` next to this repository. `tools/combined` also reads `PARITY_PI_CLI`, `PARITY_PI_ANTIGRAVITY`, `PARITY_PI_CURSOR` and `PARITY_PI_MULTIPROVIDER`. |
| Cursor upstream file for the journal test | `packages/rpi-cursor-sdk/tests/journal_reference.rs` (`#[ignore]`) | `RPI_CURSOR_REFERENCE`, default `../provider-references/cursor` |
| Antigravity reference directory and host | `packages/rpi-antigravity-auth/tests` and `scripts/vault-*-reference.ts` | `RPI_REFERENCE_DIR` (the reference checkout), `RPI_REFERENCE_HOST`, and for the OAuth reference generators `RPI_REFERENCE_OAUTH_CLIENT_ID` and `RPI_REFERENCE_OAUTH_CLIENT_SECRET` |
| Built rpi host (the pinned pi-rust fork at `6064a76`) | Real-host tests (`real_dll_*`, `loaded_*`), `tools/combined`, `tools/parity-rpc` | `RPI_TEST_HOST` for the test binaries, `PARITY_RPI_BIN` for the tools. `RPI_TEST_PROVIDER_LIBRARY`, `RPI_TEST_ANTIGRAVITY_LIBRARY`, `RPI_TEST_CURSOR_LIBRARY` and `RPI_TEST_MULTIPROVIDER_LIBRARY` name the built libraries, and `RPI_TEST_RUSTC` names the compiler for the fresh-DLL tests. |
| Lane target directories and a host checkout for the I02 evidence | `.github/scripts/refresh-i02-evidence.py` | `RPI_I02_TARGETS` (default `../.targets`) and `RPI_I02_HOST_CHECKOUT` (default `../pi-rust--host-e9c0526`) |
| Lane environment file `.parity-env.sh` | The verification runbook | Not part of this repository. It is a local file that sets the Node 24 path, the `en_US.UTF-8` locale, and git URL rewrites that point the host git pins at a local host checkout. |
| Temporary directory for the Antigravity reference scripts | `packages/rpi-antigravity-auth/scripts/vault-*-reference.ts` | `PROVIDER_TEST_TMP`, default `~/provider-test-tmp` |
| Node.js 24 and the built, staged Cursor worker | Cursor worker and runtime tests | The README build steps |
| Antigravity OAuth client values | Release builds only (repository secrets), and local builds that exercise sign-in | `RPI_ANTIGRAVITY_OAUTH_CLIENT_ID` and `RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET` at build time. Do not commit them. |

The verification runbook, [`docs/provider-parity-verification.md`](docs/provider-parity-verification.md),
lists the exact commands and the logs that each gate produces.
