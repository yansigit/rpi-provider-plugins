//! Process-owned trust matches the pinned Node 24 bundled roots and extra-CA path.
use std::{path::Path, sync::OnceLock};
use tokio_rustls::rustls::{
    self,
    pki_types::{pem::PemObject, CertificateDer},
};

fn append_pem(roots: &mut Vec<CertificateDer<'static>>, bytes: &[u8]) -> Result<(), String> {
    for item in CertificateDer::pem_slice_iter(bytes) {
        let certificate = item.map_err(|error| error.to_string())?;
        // A decoded PEM block is not necessarily a certificate.
        rustls::RootCertStore::empty()
            .add(certificate.clone())
            .map_err(|error| error.to_string())?;
        roots.push(certificate);
    }
    Ok(())
}

fn load_extra(roots: &mut Vec<CertificateDer<'static>>, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    append_pem(roots, &bytes)
}

pub(crate) fn certificates() -> &'static [CertificateDer<'static>] {
    static ROOTS: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = Vec::new();
        append_pem(&mut roots, include_bytes!("../../data/node24-roots.pem"))
            .expect("pinned Node 24 root certificates");
        if safe_environment() {
            if let Some(path) = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|p| !p.is_empty()) {
                if let Err(error) = load_extra(&mut roots, Path::new(&path)) {
                    eprintln!(
                        "Warning: Ignoring extra certs from `{}`, load failed: {error}",
                        path.to_string_lossy()
                    );
                }
            }
        }
        roots
    })
}

fn safe_environment() -> bool {
    #[cfg(unix)]
    // SAFETY: these process identity queries have no pointer arguments.
    unsafe {
        if libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() {
            return false;
        }
        #[cfg(target_os = "linux")]
        if libc::getauxval(libc::AT_SECURE) != 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_and_partial_extra_certificates_are_validated() {
        let mut roots = Vec::new();
        append_pem(&mut roots, include_bytes!("../../data/node24-roots.pem")).unwrap();
        assert_eq!(roots.len(), 121);
        let extra = include_bytes!("../../tests/fixtures/tls/server-cert.pem");
        let mut partial = extra.to_vec();
        partial
            .extend_from_slice(b"\n-----BEGIN CERTIFICATE-----\nYQ==\n-----END CERTIFICATE-----\n");
        assert!(append_pem(&mut roots, &partial).is_err());
        assert_eq!(
            roots.len(),
            122,
            "valid certificates before a malformed block remain trusted"
        );
        assert!(append_pem(&mut roots, b"text without a PEM block").is_ok());
        assert_eq!(roots.len(), 122);
    }
}
