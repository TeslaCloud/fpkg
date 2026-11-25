//! How the certificate of a database server is checked, for the TLS of the `pg` and the
//! `mysqloo` modules, with rustls on ring: not at all, or its chain against a set of roots, and
//! then its names too. A certificate of the server that is itself one of the roots is taken as
//! OpenSSL takes it, whatever it says about being an authority, once its dates (and, when the
//! names are checked, its names) are right. The names are those of its subjectAltName. Each
//! module words the errors in the way of the client library it stands in for.

use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme};

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

/// Whether an error of a certificate is about its names.
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

/// Checks the chain of the certificate, and with `names` the name in it; a certificate that is
/// one of the roots is taken as its own chain.
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

/// A verifier that takes any certificate.
pub(crate) fn unchecked(provider: Arc<CryptoProvider>) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Unchecked(provider))
}

/// A verifier that checks the chain against `roots`, and with `names` the names. `own` are
/// the certificates that a server may present as their own chain (those of a root file).
pub(crate) fn checked(
    roots: RootCertStore,
    own: Vec<CertificateDer<'static>>,
    names: bool,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::client::VerifierBuilderError> {
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build()?;
    Ok(Arc::new(Checked { webpki, own, names }))
}
