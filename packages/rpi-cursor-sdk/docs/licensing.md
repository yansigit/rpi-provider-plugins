# Licensing and SDK redistribution

This package is an integration fork of
`fitchmultz/pi-cursor-sdk@f337dc4bb7cea936c52e69a7fe08e7e376c8467b`. It retains
the upstream MIT-licensed adapter and its license notice.

| Component | License | Distribution |
| --- | --- | --- |
| `worker/` adapter sources | MIT | retained in the repository and archive notices |
| `runtime/LICENSE` | MIT (upstream `pi-cursor-sdk`) | shipped in every prebuilt archive |
| `@earendil-works/pi-*` 1.3.0 peers | MIT | installed from npm via `runtime/package-lock.json`, not vendored |
| `@cursor/sdk@1.0.37` and its platform package | Anysphere terms, not MIT | **never** copied into an archive; installed from npm by the user |
| Hono and MCP dependencies | MIT | installed from npm, not vendored |

## Why the SDK is external

The pinned SDK's `LICENSE.md` reserves rights to Anysphere and points at the
Cursor Terms of Service. The distribution review in
`pi-rust/docs/provider-parity/cursor-sdk-distribution.md` found no independent
redistribution grant, so rpi release archives keep the proprietary SDK and its
platform binaries out of every archive.

The archive ships only:

- the MIT adapter runtime (compiled and bundled) and its `LICENSE`;
- `runtime/package.json` and `runtime/package-lock.json`, which pin
  `@cursor/sdk` to exactly `1.0.37` but contain no SDK implementation.

The install documentation requires a post-install npm step to fetch the SDK under
the user's own Cursor terms. Offline execution is only possible after that step.

## Proving no SDK bytes are vendored

`distribution/cursor-sdk-1.0.37.sha256.json` records the SHA-256 of every file in
the npm-installed `@cursor/sdk@1.0.37` and, from the Linux packaging host,
`@cursor/sdk-linux-x64@1.0.37`. The packaging reference script and the Python
acceptance test compare every staged asset hash against that set. No asset may
match an SDK file, and no asset path may pass through `node_modules`. Platform
packages for other operating systems are not enumerated, but the structural
checks (no `node_modules` path, no native library extension, no `.node` file)
apply to every target archive.

The check is byte identity, not filename matching: an arbitrary reimplementation
of the adapter is allowed, a copied SDK file is not. The archive also rejects
native library extensions on sidecars, so the SDK's platform binaries cannot be
smuggled in under another name.

## Updating the SDK

Changing `@cursor/sdk` requires a new lockfile (with `runtime/package.json` and
`runtime/package-lock.json` updated together), a new distribution review, and a
regenerated `distribution/cursor-sdk-<version>.sha256.json`. Do not edit the
recorded hash set to make a build pass.
