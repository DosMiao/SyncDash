//! Building the TLS connector and explaining a handshake failure.
//!
//! A failed handshake is reported with the host and port that failed, because the common causes —
//! an implicit-vs-explicit mismatch, or a server whose certificate does not cover the name the
//! phrase used — are indistinguishable from a generic I/O error otherwise.

use super::super::error::{VfsError, VfsErrorKind, VfsResult};
use super::session::map_ftp_err;
use super::stream::Stream;
use std::sync::{Arc, Mutex};
use suppaftp::FtpError;

/// The TLS client config for `ftps://`, built on the **operating system's** trust store.
///
/// The choice between this and a bundled root set (`webpki-roots`) is the one real decision here,
/// and it follows the precedent `fs::ssh` already set with `known_hosts`: reuse the trust the user
/// has established rather than shipping a second, parallel one. Concretely, the FTPS server on a
/// LAN NAS usually presents a certificate its owner installed themselves — a bundled Mozilla root
/// set rejects exactly that, which is the main case this exists for, while the OS store accepts it
/// *because the user already said so somewhere they can see and revoke*.
///
/// No `insecure_tls` escape hatch: a flag that turns verification off is a flag that ends up set.
/// A certificate this refuses is a certificate to install, and the error says which.
/// An AUTH TLS failure, told as something the operator can act on.
///
/// Two things the generic mapper gets wrong here. A refused certificate is not `Transient` — it
/// will be refused identically on every retry until somebody installs something, so it belongs
/// with a refused password under `Auth`. And `tls_connector` above promises that "a certificate
/// this refuses is a certificate to install, and the error says which"; rustls reports the
/// *reason* but hands back no chain to print, so the message carries the command that shows the
/// operator the exact certificate being refused. Naming the remedy is what makes having no
/// `insecure_tls` flag a reasonable position rather than a dead end.
pub(super) fn map_tls_err(host: &str, port: u16, e: FtpError) -> VfsError {
    let text = e.to_string();
    if !text.contains("invalid peer certificate") && !text.contains("CertificateError") {
        return map_ftp_err("AUTH TLS", e);
    }
    let why = if text.contains("UnknownIssuer") {
        "signed by an issuer this machine does not trust"
    } else if text.contains("Expired") {
        "that has expired"
    } else if text.contains("NotValidForName") {
        "that is not valid for this host"
    } else {
        "this machine's trust store will not accept"
    };
    VfsError::new(
        VfsErrorKind::Auth,
        format!(
            "ftps://{host}:{port} presented a certificate {why} ({text}). There is deliberately no \
             flag to skip verification — install the issuing certificate into this machine's trust \
             store instead, where you can see it and revoke it. To read the certificate being \
             refused: openssl s_client -starttls ftp -connect {host}:{port} -showcerts"
        ),
    )
}

pub(super) fn tls_connector() -> VfsResult<suppaftp::RustlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        // A store with one unparseable certificate in it is still a usable store; refusing the
        // whole connection over one bad entry would be worse than skipping it.
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(VfsError::new(
            VfsErrorKind::Io,
            format!(
                "no trusted root certificates could be read from this machine's store{} — ftps:// cannot verify anything without them",
                loaded
                    .errors
                    .first()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            ),
        ));
    }
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(suppaftp::RustlsConnector::from(std::sync::Arc::new(cfg)))
}

pub(super) struct FtpConn {
    pub(super) stream: Stream,
}

pub(super) type ConnSlot = Arc<Mutex<Option<FtpConn>>>;
