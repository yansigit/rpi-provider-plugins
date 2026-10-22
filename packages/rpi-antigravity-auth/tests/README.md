# AG-3 differential reference README

`ag3-slot-lock-reference.mjs` and `ag3-menu-reference.ts` drive the **unchanged**
Antigravity checkout at `cortexkit/antigravity-auth@a97f9055017f3c1546afbfe61a19d50b428be3d8`
through the native agent's Node 24.22.0 runtime. They never read a real credential,
never use the real network and never write outside a private scratch directory.

## Runtime pin and hash gate

Before importing any upstream module, both scripts:

1. require `RPI_REFERENCE_DIR`;
2. require Node `v24.22.0`, the `referenceNode` recorded in `ag3-evidence.json`;
3. run `git -C "$RPI_REFERENCE_DIR" rev-parse HEAD` and require the exact
   `reference` commit from the evidence file (`a97f905...`);
4. sha256 every file in the evidence file's `upstreamSha256` map and require an
   exact match.

A missing checkout, a different commit, a modified upstream file, a missing file
or a wrong runtime prints the mismatch and exits non-zero **before** the reference
code is imported. Check mode additionally regenerates and compares the whole
fixture, so a changed upstream cannot silently pass.

## Regenerate and check

```sh
export RPI_REFERENCE_DIR=/path/to/provider-references/antigravity
export RPI_PRIVATE_TMP="$(mktemp -d "$HOME/ag3-private.XXXXXX")"

# Slot-lock faults and competing processes.
node tests/ag3-slot-lock-reference.mjs --write
node tests/ag3-slot-lock-reference.mjs

# Complete /antigravity menu walks.
node --experimental-transform-types tests/ag3-menu-reference.ts --write
node --experimental-transform-types tests/ag3-menu-reference.ts
```

Two consecutive check-mode runs must produce byte-identical documents. The
fixtures are compared by the Rust tests `ag3_slot_lock.rs`, `ag3_menu.rs` and
`ag3_cancellation.rs`.

Current fixture sizes: 36 lease cases, 5 credential-slot cases and 1
competing-process case in `ag3-slot-lock.json`; 90 complete menu walks in
`ag3-menu.json` (35 of them notify).

## Normalizations (applied identically on both sides)

These are the only substitutions. Everything else is compared byte for byte;
unexpected values fail rather than being hidden.

| Token | Where | Reference rule | Native rule |
| --- | --- | --- | --- |
| `<OWNER>` | Lease snapshots | At key `ownerId`, replace a string exactly equal to the ownerId captured from that case's acquire. Corrupt and foreign ids are untouched. | `lease_normalize()` applies the same key-and-exact-value test. |
| `<OWNER>` | Competing-process successor | Replace the fresh successor's random ownerId unconditionally; `expiresAt`, child exit, contender result and leftover entries stay exact. | The same fixed object is built after acquisition. |
| `<DIR>` | Lease error messages | `message.replaceAll(directory, '<DIR>')`, where `directory` is that case's private scratch directory. Error name and the rest of the message stay exact. | `error.to_string().replace(scratch_root, '<DIR>')` on the same scratch root. |
| `<auth-identity>` | Credential-slot snapshot text | Replace `sha256(JSON.stringify(authPath))`, the digest the pinned slot writes as its marker. No path is stored in the fixture. | Replace `Sha256(json!(auth).to_string())` in file text; the same exact digest only. |
| absent | Credential-slot snapshot | `lstat`/`read` with `ENOENT` **or** `ENOTDIR` produces `null`; every other error is thrown. | `ErrorKind::NotFound` or `raw_os_error() == ENOTDIR` produces `Value::Null`; every other error panics. |
| `0x07` salt | Menu item ids | The single 16-byte `crypto.randomBytes(16)` draw is fixed to 16 bytes of `0x07`. | `Menu { salt: [7; 16] }`. |

`ENOTDIR` is the only platform-specific absence mapping: Linux reports it when an
ancestor of `vault/stash.json` was replaced by a regular file. Mapping only that
error (and `ENOENT`) to "absent", while propagating all others, is what keeps the
stash-parent-is-a-file scenario comparable without masking a permission or IO
failure. The menu reference has no value normalization; its UI trace, callback
order, exact notification strings and empty invocation-level `notifications`
array are compared unchanged.

See `ag3-evidence.json` (`normalizations`, `toolchains`, `checks`) for the machine
readable record.
