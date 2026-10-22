//! Process-owned trust matches the pinned Node 24 bundled roots, the system and
//! OpenSSL stores Node's switches select, and the extra-CA path.
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
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

/// OpenSSL's default verify paths skip unreadable or non-certificate entries.
fn append_system_pem(roots: &mut Vec<CertificateDer<'static>>, bytes: &[u8]) {
    for item in CertificateDer::pem_slice_iter(bytes) {
        if let Ok(certificate) = item {
            if rustls::RootCertStore::empty()
                .add(certificate.clone())
                .is_ok()
            {
                roots.push(certificate);
            }
        }
    }
}

fn load_extra(roots: &mut Vec<CertificateDer<'static>>, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    append_pem(roots, &bytes)
}

/// `NODE_OPTIONS` tokens that select an alternate certificate store.
fn node_options() -> Vec<String> {
    std::env::var("NODE_OPTIONS")
        .map(|value| value.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// `--use-system-ca` adds the OS store; `NODE_USE_SYSTEM_CA` accepts only `1`.
fn system_ca() -> bool {
    std::env::var_os("NODE_USE_SYSTEM_CA").is_some_and(|value| value == "1")
        || node_options().iter().any(|token| token == "--use-system-ca")
}

/// `--use-openssl-ca` replaces the bundled roots with OpenSSL's default store.
fn openssl_ca() -> bool {
    node_options().iter().any(|token| token == "--use-openssl-ca")
}

fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

fn system_file() -> Option<PathBuf> {
    std::env::var_os("SSL_CERT_FILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            first_existing(&[
                "/usr/lib/ssl/cert.pem",
                "/etc/ssl/certs/ca-certificates.crt",
                "/etc/pki/tls/certs/ca-bundle.crt",
                "/etc/ssl/cert.pem",
            ])
        })
}

fn system_dir() -> Option<PathBuf> {
    std::env::var_os("SSL_CERT_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| first_existing(&["/usr/lib/ssl/certs", "/etc/ssl/certs", "/etc/pki/tls/certs"]))
}

/// The OpenSSL default verify paths: one CA file plus one hashed CA directory.
fn load_system(roots: &mut Vec<CertificateDer<'static>>) {
    if let Some(file) = system_file() {
        if let Ok(bytes) = std::fs::read(&file) {
            append_system_pem(roots, &bytes);
        }
    }
    if let Some(dir) = system_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    append_system_pem(roots, &bytes);
                }
            }
        }
    }
}

pub(crate) fn certificates() -> &'static [CertificateDer<'static>] {
    static ROOTS: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = Vec::new();
        if openssl_ca() {
            load_system(&mut roots);
        } else {
            append_pem(&mut roots, include_bytes!("../../data/node24-roots.pem"))
                .expect("pinned Node 24 root certificates");
            if system_ca() {
                load_system(&mut roots);
            }
        }
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
