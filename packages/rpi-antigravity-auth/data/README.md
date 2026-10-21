Native locale ordering uses [rpi-node-collation](../../../crates/rpi-node-collation/data/README.md), including its pinned ICU data, generator and licenses.

`node24-roots.pem` is the exact 121-certificate bundled trust list from the pinned
Node 24.22.0 reference, also verified against `nodejs/node@v24.22.0`'s
`src/node_root_certs.h`. Its Node notice is in `NODE-LICENSE`. Reproduce it using
that Node binary: `node -p 'require("node:tls").rootCertificates.join("\n")' > data/node24-roots.pem`.
The acceptance evidence records its SHA256. Trust-list updates require a deliberate
reference pin change and TLS acceptance rerun.
