//! Node 24 / OpenSSL certificate-failure diagnostics for the AGY transport.
//!
//! Node reports a TLS chain failure with the OpenSSL reason and a hint, e.g.
//! `self-signed certificate; if the root CA is installed locally, try running
//! Node.js with --use-system-ca`. rustls reports `UnknownIssuer` for both a
//! self-signed leaf and a leaf issued by an unknown CA, so the end-entity
//! certificate is captured during the handshake to choose the original text.
use std::{
    io,
    sync::{Arc, Mutex},
};
use tokio_rustls::rustls::{
    self,
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    CertificateError, DigitallySignedStruct, Error as TlsError, SignatureScheme,
};

const SYSTEM_CA_HINT: &str =
    "; if the root CA is installed locally, try running Node.js with --use-system-ca";

/// Delegates to the pinned verifier and records the presented leaf certificate.
#[derive(Debug)]
pub(crate) struct CapturingVerifier {
    delegate: Arc<dyn ServerCertVerifier>,
    end_entity: Mutex<Option<CertificateDer<'static>>>,
}
impl CapturingVerifier {
    pub(crate) fn new(delegate: Arc<dyn ServerCertVerifier>) -> Arc<Self> {
        Arc::new(Self {
            delegate,
            end_entity: Mutex::new(None),
        })
    }
    /// The leaf certificate the peer presented, if the handshake reached it.
    pub(crate) fn end_entity(&self) -> Option<CertificateDer<'static>> {
        self.end_entity.lock().unwrap().clone()
    }
}
impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        *self.end_entity.lock().unwrap() = Some(end_entity.clone().into_owned());
        self.delegate.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.delegate.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.delegate.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.delegate.supported_verify_schemes()
    }
}

/// Build the pinned webpki verifier the diagnostic wrapper delegates to.
pub(crate) fn verifier(
    roots: Arc<rustls::RootCertStore>,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Arc<dyn ServerCertVerifier> {
    WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .expect("pinned roots build a verifier")
}

/// Original Node 24 message for a chain failure, or `None` when the failure is
/// not one Node reports with an OpenSSL reason.
pub(crate) fn message(
    error: &io::Error,
    end_entity: Option<&CertificateDer<'static>>,
) -> Option<String> {
    let tls = error.get_ref()?.downcast_ref::<TlsError>()?;
    match tls {
        TlsError::InvalidCertificate(CertificateError::UnknownIssuer) => {
            let self_signed = end_entity
                .and_then(|certificate| names(certificate.as_ref()))
                .is_some_and(|(issuer, subject)| issuer == subject);
            Some(format!(
                "{}{SYSTEM_CA_HINT}",
                if self_signed {
                    "self-signed certificate"
                } else {
                    "unable to verify the first certificate"
                }
            ))
        }
        _ => None,
    }
}

/// Read one DER TLV: tag, content offset, and one-past-the-end offset.
fn read_tlv(bytes: &[u8], start: usize) -> Option<(u8, usize, usize)> {
    let tag = *bytes.get(start)?;
    let mut cursor = start + 1;
    let first = *bytes.get(cursor)?;
    cursor += 1;
    let length = if first & 0x80 == 0 {
        first as usize
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut value = 0usize;
        for _ in 0..count {
            value = (value << 8) | *bytes.get(cursor)? as usize;
            cursor += 1;
        }
        value
    };
    let content = cursor;
    let end = content.checked_add(length)?;
    if end > bytes.len() {
        return None;
    }
    Some((tag, content, end))
}

/// The DER-encoded issuer and subject names of a certificate.
fn names(certificate: &[u8]) -> Option<(&[u8], &[u8])> {
    let (tag, outer, _) = read_tlv(certificate, 0)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, mut cursor, _) = read_tlv(certificate, outer)?;
    if tag != 0x30 {
        return None;
    }
    // Optional explicit version [0].
    let (tag, _, end) = read_tlv(certificate, cursor)?;
    if tag == 0xa0 {
        cursor = end;
    }
    for expected in [0x02, 0x30] {
        let (tag, _, end) = read_tlv(certificate, cursor)?;
        if tag != expected {
            return None;
        }
        cursor = end;
    }
    let (tag, _, issuer_end) = read_tlv(certificate, cursor)?;
    if tag != 0x30 {
        return None;
    }
    let issuer = &certificate[cursor..issuer_end];
    cursor = issuer_end;
    let (tag, _, end) = read_tlv(certificate, cursor)?;
    if tag != 0x30 {
        return None;
    }
    cursor = end;
    let (tag, _, subject_end) = read_tlv(certificate, cursor)?;
    if tag != 0x30 {
        return None;
    }
    Some((issuer, &certificate[cursor..subject_end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn issuer_and_subject_distinguish_self_signed_certificates() {
        let self_signed = include_bytes!("../../tests/fixtures/tls/server-cert.der");
        let (issuer, subject) = names(self_signed).unwrap();
        assert_eq!(issuer, subject);
        let leaf = include_bytes!("../../tests/fixtures/tls/leaf-cert.der");
        let (issuer, subject) = names(leaf).unwrap();
        assert_ne!(issuer, subject);
        assert!(names(b"not a certificate").is_none());
        assert!(names(&[]).is_none());
    }
}
