# Reviewed compatibility sources

These private integration copies retain their original MIT notices and are not
published as replacements for upstream crates. `UPSTREAM.json` in each directory
records the revision, original file hashes, and local changes.

| Component | Version | Revision |
| --- | --- | --- |
| cortexkit-claustrum-client | 0.5.0 | cortexkit/claustrum `64fe590ab5f81c43c801d70421111107ff8f1cf5` |
| subc-client-rs | 0.30.5 | cortexkit/subconscious `3f9b1083257a0a0e27f6ff47e5c624bda190fbfe` |
| subc-os | 0.1.16 | published crate, cortexkit/subconscious `668dfbde461d30aa63580dd032c91cf3f1b75f5f` |
| private-files native | 0.2.1 | cortexkit/subconscious `3f9b1083257a0a0e27f6ff47e5c624bda190fbfe` |
| private-files legacy native | 0.1.0 | cortexkit/subconscious `eeab1627627b8ce7498b0bc633bdba83ea343fa4` |

The enrolled TypeScript protocol is stricter than the supervised Rust credential
resolver. Antigravity validates that enrolled contract before calling the shared
Rust opaque-payload decoder; it does not substitute supervised resolution for
consumer enrollment. The unchanged producer enrollment fixture accompanies the
Claustrum source.

Rust 1.78 compatibility uses equivalent option/remainder expressions and
`os_pipe` 1.2.3 in place of newer standard-library pipe APIs. Test scratch folders
use `tempfile`; assertions and producer fixtures are retained. Standalone locks
pin compatible dependencies. Run their suites separately because these copies
are excluded from the package workspace:

```sh
cargo +1.78.0 test --manifest-path vendor/claustrum-client/Cargo.toml --no-default-features --locked
cargo +1.78.0 test --manifest-path vendor/subc-client-rs/Cargo.toml --lib --locked
cargo +1.78.0 test --manifest-path vendor/subc-os/Cargo.toml --lib launch_nonce --locked
```

Passing these component suites does not establish completed vault integration,
platform qualification, filesystem permissions or live producer acceptance.

The two private-files copies are intentionally separate: pinned common-auth and
Claustrum token publication reach different Windows directory policies. Their
Win32 policy code and original C test suites are retained; `RPI_NATIVE` replaces
only N-API dispatch/completion with synchronous owned results, preserving secret
buffer erasure. Antigravity compiles these helpers into its native Windows DLL.
Rust 1.78 Windows compatibility also uses `GetFullPathNameW` and explicit
`size_of` imports in subc-os. Cross-compilation does not count as Windows execution.
