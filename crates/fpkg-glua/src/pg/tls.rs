//! TLS for the connections that `sslmode` asks to encrypt, with rustls on ring, as ureq sets it
//! up elsewhere in fpkg. `verify-ca` checks the chain of the server's certificate and
//! `verify-full` its name too, against the certificates of the `sslrootcert` file (PEM), or the
//! Mozilla roots of webpki-roots without one (libpq's `sslrootcert=system`). The other modes
//! check the chain when an `sslrootcert` file exists, as libpq does, and nothing otherwise. The
//! checks themselves are those of `crate::verify`, which the mysqloo module shares: a
//! certificate of the server that is itself in the file is taken as OpenSSL takes it, and the
//! names are those of its subjectAltName (the common name is not read, where libpq falls back
//! to it).
//! Failures read as libpq's: `SSL error: certificate verify failed`, `server certificate for
//! "a" does not match host name "b"`, and the errors about the root certificate file.

use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{
    ALL_VERSIONS, CertificateError, ClientConfig, ClientConnection, RootCertStore, StreamOwned,
};

/// An encrypted connection.
pub(crate) type Tls = StreamOwned<ClientConnection, TcpStream>;

/// How much of the server's certificate is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Check {
    Nothing,
    Chain,
    Full,
}

/// Where the certificates that a chain has to end in come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Roots {
    /// The Mozilla roots of webpki-roots.
    Builtin,
    /// A PEM file (`sslrootcert`).
    File(PathBuf),
}

/// What is checked of the server's certificate, and against what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verify {
    pub check: Check,
    pub roots: Roots,
}

/// The certificates of the roots, and those of the file once more (for a server that presents
/// one of them), or libpq's error about the file.
fn root_store(roots: &Roots) -> Result<(RootCertStore, Vec<CertificateDer<'static>>), String> {
    let path = match roots {
        Roots::Builtin => {
            let store = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            return Ok((store, Vec::new()));
        }
        Roots::File(path) => path,
    };
    let shown = path.display();
    let unreadable = |why: &dyn std::fmt::Display| {
        format!("could not read root certificate file \"{shown}\": {why}")
    };
    if path.is_dir() {
        return Err(unreadable(&"no certificate or crl found"));
    }
    let pem = match std::fs::read(path) {
        Ok(pem) => pem,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "root certificate file \"{shown}\" does not exist\nEither provide the file, use the system's trusted roots with sslrootcert=system, or change sslmode to disable server certificate verification."
            ));
        }
        Err(e) => return Err(unreadable(&super::wire::os_text(&e))),
    };
    let mut store = RootCertStore::empty();
    let mut own = Vec::new();
    for cert in CertificateDer::pem_slice_iter(&pem) {
        let cert = cert.map_err(|e| unreadable(&e))?;
        store
            .add(cert.clone())
            .map_err(|_| unreadable(&"a certificate in it is not one that can be read"))?;
        own.push(cert);
    }
    if store.is_empty() {
        return Err(unreadable(&"no certificate or crl found"));
    }
    Ok((store, own))
}

fn config(verify: &Verify) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(ALL_VERSIONS)
        .map_err(|e| format!("SSL error: {e}"))?;
    let checked = |names: bool| {
        let (store, own) = root_store(&verify.roots)?;
        crate::verify::checked(store, own, names, provider.clone())
            .map_err(|e| format!("SSL error: {e}"))
    };
    let builder = match verify.check {
        Check::Nothing => builder
            .dangerous()
            .with_custom_certificate_verifier(crate::verify::unchecked(provider.clone())),
        Check::Chain => builder
            .dangerous()
            .with_custom_certificate_verifier(checked(false)?),
        Check::Full => builder
            .dangerous()
            .with_custom_certificate_verifier(checked(true)?),
    };
    Ok(builder.with_no_client_auth())
}

/// Encrypts the connection to `host` (for the name in the certificate and SNI), at `addr`,
/// once the server has said yes to it. Errors are libpq's.
pub(crate) fn handshake(
    socket: TcpStream,
    host: &str,
    addr: IpAddr,
    verify: &Verify,
) -> Result<Tls, String> {
    let config = Arc::new(config(verify)?);
    let name = ServerName::try_from(host.to_string()).unwrap_or(ServerName::IpAddress(addr.into()));
    let conn = ClientConnection::new(config, name).map_err(|e| format!("SSL error: {e}"))?;
    let mut tls = StreamOwned::new(conn, socket);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| ssl_error(&e, host))?;
    }
    Ok(tls)
}

/// The message of an error during the handshake, in libpq's words where it has them.
fn ssl_error(e: &std::io::Error, host: &str) -> String {
    let rustls_error = e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match rustls_error {
        Some(rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext {
            presented,
            ..
        })) => name_mismatch(presented, host),
        Some(rustls::Error::InvalidCertificate(CertificateError::NotValidForName)) => {
            name_mismatch(&[], host)
        }
        Some(rustls::Error::InvalidCertificate(_)) => "SSL error: certificate verify failed".into(),
        Some(other) => format!("SSL error: {other}"),
        None => match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                "timeout expired".into()
            }
            std::io::ErrorKind::UnexpectedEof => "SSL SYSCALL error: EOF detected".into(),
            _ => format!("SSL error: {}", super::wire::os_text(e)),
        },
    }
}

/// libpq's message for a certificate whose names do not include the host. rustls gives the
/// names of the certificate as `DnsName("a")` and `IpAddress(1.2.3.4)`; libpq counts those two
/// kinds.
pub(crate) fn name_mismatch(presented: &[String], host: &str) -> String {
    let names: Vec<&str> = presented
        .iter()
        .filter_map(|n| {
            n.strip_prefix("DnsName(\"")
                .and_then(|n| n.strip_suffix("\")"))
                .or_else(|| {
                    n.strip_prefix("IpAddress(")
                        .and_then(|n| n.strip_suffix(')'))
                })
        })
        .collect();
    match &names[..] {
        [] => format!(
            "the server certificate has no subjectAltName for a host name, and fpkg's pg module does not read its common name: reissue it with a subjectAltName for \"{host}\", or use sslmode=verify-ca"
        ),
        [first] => {
            format!("server certificate for \"{first}\" does not match host name \"{host}\"")
        }
        [first, rest @ ..] => format!(
            "server certificate for \"{first}\" (and {} other name{}) does not match host name \"{host}\"",
            rest.len(),
            if rest.len() == 1 { "" } else { "s" }
        ),
    }
}
