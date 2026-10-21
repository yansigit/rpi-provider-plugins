use rpi_antigravity_auth::transport::network::Client;
use std::sync::Arc;
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
    TlsAcceptor,
};
pub fn tls() -> (Client, TlsAcceptor) {
    let certificate =
        CertificateDer::from(include_bytes!("../fixtures/tls/server-cert.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("../fixtures/tls/server-key.der").to_vec(),
    ));
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(crypto.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = rustls::ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    (
        Client::with_tls(client),
        TlsAcceptor::from(Arc::new(server)),
    )
}
