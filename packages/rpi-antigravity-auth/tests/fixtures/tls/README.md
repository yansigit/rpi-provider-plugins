# Loopback TLS fixtures

These are public, disposable test credentials for `localhost`, `127.0.0.1` and
`::1`. The test client trusts this certificate explicitly; the production client
does not. PEM files serve the unchanged Node reference; DER files serve Rust TLS.
No account credential or production key is included.
