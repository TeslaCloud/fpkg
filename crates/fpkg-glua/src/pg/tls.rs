//! TLS for the connections that `sslmode` asks to encrypt, with rustls on ring, as ureq sets it
//! up elsewhere in fpkg. `prefer` and `require` encrypt without checking the server's
//! certificate, as libpq does without a root certificate; `verify-ca` checks its chain and
//! `verify-full` its name too, against the Mozilla roots of webpki-roots (libpq's
//! `sslrootcert=system`), since no root certificate file is read.

use std::net::{IpAddr, TcpStream};
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
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

/// Checks the chain of the certificate and not the name in it (`verify-ca`).
#[derive(Debug)]
struct ChainOnly(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for ChainOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

fn config(check: Check) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(ALL_VERSIONS)
        .map_err(|e| format!("SSL error: {e}"))?;
    let roots = || {
        Arc::new(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
    };
    let webpki = || {
        WebPkiServerVerifier::builder_with_provider(roots(), provider.clone())
            .build()
            .map_err(|e| format!("SSL error: {e}"))
    };
    let builder = match check {
        Check::Nothing => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unchecked(provider.clone()))),
        Check::Chain => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(ChainOnly(webpki()?))),
        Check::Full => builder.with_webpki_verifier(webpki()?),
    };
    Ok(builder.with_no_client_auth())
}

/// Encrypts the connection to `host` (for the name in the certificate and SNI), at `addr`,
/// once the server has said yes to it. Errors are libpq's "SSL error: ..." lines.
pub(crate) fn handshake(
    socket: TcpStream,
    host: &str,
    addr: IpAddr,
    check: Check,
) -> Result<Tls, String> {
    let config = Arc::new(config(check)?);
    let name = ServerName::try_from(host.to_string()).unwrap_or(ServerName::IpAddress(addr.into()));
    let conn = ClientConnection::new(config, name).map_err(|e| format!("SSL error: {e}"))?;
    let mut tls = StreamOwned::new(conn, socket);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| ssl_error(&e))?;
    }
    Ok(tls)
}

/// The message of an error during the handshake.
fn ssl_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "timeout expired".into(),
        std::io::ErrorKind::UnexpectedEof => "SSL SYSCALL error: EOF detected".into(),
        _ => format!("SSL error: {}", super::wire::os_text(e)),
    }
}
