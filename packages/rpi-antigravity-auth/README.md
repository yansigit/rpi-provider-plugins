# rpi-antigravity-auth

Work in progress: the native provider is not installable or parity complete.
The implemented catalog/routing module matches the pinned Pi integration, including
its `google` identity, six chat models, displayed zero prices, tier defaults and
retired model classification. Authentication, streaming, vault custody and commands
are still being implemented; see [PARITY.md](PARITY.md).

The reference is `cortexkit/antigravity-auth@a97f9055017f3c1546afbfe61a19d50b428be3d8`.
Its MIT notice is retained in [LICENSE.upstream](LICENSE.upstream).

Regenerate the catalog and 20,376 routing fixtures using the reference's locked Bun
1.4.2, a checkout at that exact revision and the established Pi 1.3.0 integration:

```sh
bun scripts/catalog-reference.ts /path/to/antigravity /path/to/integrated \
  tests/fixtures/catalog.json src/catalog.json
cargo test -p rpi-antigravity-auth --locked
cargo +1.78.0 test -p rpi-antigravity-auth --locked
```

The generator isolates its paths and refuses network access. Catalog registration
runs from the original packed extension with Pi 1.3.0; routing calls the original
source helper with its unchanged locked dependencies. The fixtures do not establish
host registration, authentication order, transport or inference acceptance.
