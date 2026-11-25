//! TLS for the connections that `sslmode` asks to encrypt, with rustls on ring, as ureq sets it
//! up elsewhere in fpkg. `verify-ca` checks the chain of the server's certificate and
//! `verify-full` its name too, against the certificates of the `sslrootcert` file (PEM), or the
//! Mozilla roots of webpki-roots without one (libpq's `sslrootcert=system`). The other modes
//! check the chain when an `sslrootcert` file exists, as libpq does, and nothing otherwise. A
//! certificate of the server that is itself in the file is taken as OpenSSL takes it, whatever
//! it says about being an authority, once its dates (and for `verify-full` its names) are right.
//! The names are those of its subjectAltName: the common name is not read, where libpq falls
//! back to it.
//! Failures read as libpq's: `SSL error: certificate verify failed`, `server certificate for
//! "a" does not match host name "b"`, and the errors about the root certificate file.

use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ALL_VERSIONS, CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct,
    RootCertStore, SignatureScheme, StreamOwned,
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

/// A DER element: its tag, its contents and what follows it.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().fold(0usize, |l, b| l << 8 | *b as usize);
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Seconds since 1970 of an X.509 time: `UTCTime` (tag 0x17) or `GeneralizedTime` (0x18).
fn seconds(tag: u8, text: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(text).ok()?;
    let (year, rest) = match tag {
        0x17 => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, s.get(2..)?)
        }
        0x18 => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    if rest.len() != 11 || !rest.ends_with('Z') {
        return None;
    }
    let part = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    let (month, day) = (part(0)?, part(2)?);
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + part(4)? * 3600 + part(6)? * 60 + part(8)?)
}

/// The `notBefore` and `notAfter` of a certificate, in seconds since 1970.
fn validity(der: &[u8]) -> Option<(i64, i64)> {
    let (0x30, certificate, _) = element(der)? else {
        return None;
    };
    let (0x30, tbs, _) = element(certificate)? else {
        return None;
    };
    let (tag, _, mut rest) = element(tbs)?;
    if tag != 0xa0 {
        rest = tbs;
    }
    for _ in 0..3 {
        rest = element(rest)?.2;
    }
    let (0x30, validity, _) = element(rest)? else {
        return None;
    };
    let (from_tag, from, rest) = element(validity)?;
    let (until_tag, until, _) = element(rest)?;
    Some((seconds(from_tag, from)?, seconds(until_tag, until)?))
}

/// Takes the certificate of a server that is itself one of the roots, as OpenSSL does: its
/// dates have to be right, and with `names` its names too.
fn own_root(
    end_entity: &CertificateDer<'_>,
    server_name: &ServerName<'_>,
    now: UnixTime,
    names: bool,
) -> Result<ServerCertVerified, rustls::Error> {
    let bad = || rustls::Error::InvalidCertificate(CertificateError::BadEncoding);
    let (from, until) = validity(end_entity.as_ref()).ok_or_else(bad)?;
    let now = now.as_secs() as i64;
    if now < from {
        return Err(rustls::Error::InvalidCertificate(
            CertificateError::NotValidYet,
        ));
    }
    if now > until {
        return Err(rustls::Error::InvalidCertificate(CertificateError::Expired));
    }
    if names {
        let cert = webpki::EndEntityCert::try_from(end_entity).map_err(|_| bad())?;
        cert.verify_is_valid_for_subject_name(server_name)
            .map_err(|e| match e {
                webpki::Error::CertNotValidForName(context) => {
                    rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext {
                        expected: context.expected,
                        presented: context.presented,
                    })
                }
                _ => rustls::Error::InvalidCertificate(CertificateError::NotValidForName),
            })?;
    }
    Ok(ServerCertVerified::assertion())
}

fn name_error(e: &rustls::Error) -> bool {
    matches!(
        e,
        rustls::Error::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
        )
    )
}

/// Takes any certificate, and checks only that the handshake is signed by it.
#[derive(Debug)]
struct Unchecked(Arc<CryptoProvider>);

impl ServerCertVerifier for Unchecked {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Checks the chain of the certificate, and with `names` the name in it (`verify-full`); a
/// certificate that is one of the roots of the file is taken as its own chain.
#[derive(Debug)]
struct Checked {
    webpki: Arc<WebPkiServerVerifier>,
    own: Vec<CertificateDer<'static>>,
    names: bool,
}

impl ServerCertVerifier for Checked {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let result = match self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Err(e)
                if !name_error(&e)
                    && self.own.iter().any(|c| c.as_ref() == end_entity.as_ref()) =>
            {
                own_root(end_entity, server_name, now, self.names)
            }
            other => other,
        };
        match result {
            Err(e) if !self.names && name_error(&e) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

fn config(verify: &Verify) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(ALL_VERSIONS)
        .map_err(|e| format!("SSL error: {e}"))?;
    let checked = |names: bool| -> Result<Arc<Checked>, String> {
        let (store, own) = root_store(&verify.roots)?;
        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone())
            .build()
            .map_err(|e| format!("SSL error: {e}"))?;
        Ok(Arc::new(Checked { webpki, own, names }))
    };
    let builder = match verify.check {
        Check::Nothing => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unchecked(provider.clone()))),
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
