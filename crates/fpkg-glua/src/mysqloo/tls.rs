//! TLS for the connections that `setSSLSettings` asks to encrypt, with rustls on ring as
//! elsewhere in fpkg. As libmariadb 3.2 does by default, the name in the server's certificate
//! is not checked; its chain is checked against the `ca` file when one is given, and not at
//! all without one, with the checks that the pg module uses too (`crate::verify`), so that a
//! server certificate that is itself in the file is taken as OpenSSL takes it. A client
//! certificate is sent when `key` and `cert` are both given.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ALL_VERSIONS, ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// The files of `setSSLSettings`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settings {
    pub key: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub ca: Option<PathBuf>,
}

/// The certificates of a PEM file.
fn certificates(path: &PathBuf) -> Result<Vec<CertificateDer<'static>>, String> {
    let shown = path.display();
    CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("{shown}: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{shown}: {e}"))
}

fn config(settings: &Settings) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(ALL_VERSIONS)
        .map_err(|e| e.to_string())?;
    let verifier = match &settings.ca {
        None => crate::verify::unchecked(provider),
        Some(ca) => {
            let own = certificates(ca)?;
            if own.is_empty() {
                return Err(format!("{}: no certificate in it", ca.display()));
            }
            let mut roots = RootCertStore::empty();
            for cert in &own {
                roots.add(cert.clone()).map_err(|e| e.to_string())?;
            }
            crate::verify::checked(roots, own, false, provider).map_err(|e| e.to_string())?
        }
    };
    let builder = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    match (&settings.cert, &settings.key) {
        (Some(cert), Some(key)) => {
            let chain = certificates(cert)?;
            let key =
                PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| e.to_string())
        }
        _ => Ok(builder.with_no_client_auth()),
    }
}

/// Encrypts the connection to `host` once the server has been asked to. Errors are the text
/// that libmariadb puts after "SSL connection error: ".
pub(crate) fn handshake<S: Read + Write>(
    socket: S,
    host: &str,
    settings: &Settings,
) -> Result<StreamOwned<ClientConnection, S>, String> {
    let config = Arc::new(config(settings)?);
    let name = ServerName::try_from(host.to_string())
        .unwrap_or_else(|_| ServerName::try_from("localhost").expect("a valid name"));
    let conn = ClientConnection::new(config, name).map_err(|e| e.to_string())?;
    let mut tls = StreamOwned::new(conn, socket);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| reason(&e))?;
    }
    Ok(tls)
}

/// The reason of an error of a TLS stream: the TLS error when there is one (an alert of the
/// server, a certificate that is refused), the system's otherwise.
pub(crate) fn reason(e: &std::io::Error) -> String {
    match tls_error(e) {
        Some(tls) => tls.to_string(),
        None => e.to_string(),
    }
}

/// The TLS error that an error of a stream carries, if it is one.
pub(crate) fn tls_error(e: &std::io::Error) -> Option<&rustls::Error> {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
}
