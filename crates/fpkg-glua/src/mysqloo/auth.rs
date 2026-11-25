//! The answers of the authentication plugins that libmariadb has built in for MySQLOO:
//! `mysql_native_password` and `caching_sha2_password` scramble the password with the nonce
//! of the server; `caching_sha2_password` and `sha256_password` send it in the clear over TLS
//! or a Unix socket and otherwise encrypted with the public key of the server (RSA with OAEP
//! padding over SHA-1, as OpenSSL pads it for libmariadb). A key from the server is refused
//! before any arithmetic when it is larger than OpenSSL takes (a modulus above 16384 bits, an
//! exponent above 64 bits) or cannot be one (an even modulus, an exponent of 0 or 1).

use num_bigint::BigUint;
use ring::rand::{SecureRandom, SystemRandom};
use sha1::{Digest, Sha1};
use sha2::Sha256;

/// `mysql_native_password`: SHA1(password) XOR SHA1(nonce + SHA1(SHA1(password))), nothing
/// for an empty password.
pub(crate) fn native(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = Sha1::digest(password);
    let stage2 = Sha1::digest(stage1);
    let mut h = Sha1::new();
    h.update(&nonce[..nonce.len().min(20)]);
    h.update(stage2);
    let mix = h.finalize();
    stage1.iter().zip(mix.iter()).map(|(a, b)| a ^ b).collect()
}

/// `caching_sha2_password`: SHA256(password) XOR SHA256(SHA256(SHA256(password)) + nonce),
/// nothing for an empty password.
pub(crate) fn caching_sha2(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = Sha256::digest(password);
    let stage2 = Sha256::digest(stage1);
    let mut h = Sha256::new();
    h.update(stage2);
    h.update(&nonce[..nonce.len().min(20)]);
    let mix = h.finalize();
    stage1.iter().zip(mix.iter()).map(|(a, b)| a ^ b).collect()
}

/// The password with its terminating zero byte, each byte XOR the nonce, encrypted with the
/// public key in `pem` (a `PUBLIC KEY` or `RSA PUBLIC KEY` block).
pub(crate) fn encrypt_password(
    password: &[u8],
    nonce: &[u8],
    pem: &[u8],
) -> Result<Vec<u8>, String> {
    let mut plain = password.to_vec();
    plain.push(0);
    if !nonce.is_empty() {
        for (i, b) in plain.iter_mut().enumerate() {
            *b ^= nonce[i % nonce.len()];
        }
    }
    let (n, e) = public_key(pem).ok_or(NO_KEY)?;
    let mut seed = [0u8; 20];
    SystemRandom::new()
        .fill(&mut seed)
        .map_err(|_| "the system gave no random numbers to encrypt the password with")?;
    oaep_encrypt(&n, &e, &plain, &seed)
}

/// libmariadb's message for a public key that it cannot use.
const NO_KEY: &str = "Couldn't read RSA public key from server";

/// The largest PEM block of a public key that is read.
const MAX_PEM: usize = 16 * 1024;

/// The largest modulus (16384 bits, as OpenSSL takes) and exponent (64 bits) of a key.
const MAX_MODULUS: usize = 2048;
const MAX_EXPONENT: usize = 8;

/// Whether a key is one to encrypt with: an odd modulus of at most 16384 bits and an exponent
/// of at most 64 bits above 1, both without leading zero bytes.
pub(crate) fn usable_key(n: &[u8], e: &[u8]) -> bool {
    let odd = n.last().is_some_and(|b| b & 1 == 1);
    let lead = |v: &[u8]| v.first().is_some_and(|b| *b != 0);
    let above_one = e.len() > 1 || e.first().is_some_and(|b| *b > 1);
    odd && lead(n) && n.len() <= MAX_MODULUS && lead(e) && e.len() <= MAX_EXPONENT && above_one
}

/// RSA encryption with OAEP padding over SHA-1 and MGF1 (RFC 8017), with the given seed. The
/// key is checked (`usable_key`) before any arithmetic.
pub(crate) fn oaep_encrypt(
    n: &[u8],
    e: &[u8],
    message: &[u8],
    seed: &[u8; 20],
) -> Result<Vec<u8>, String> {
    if !usable_key(n, e) {
        return Err(NO_KEY.into());
    }
    let n_int = BigUint::from_bytes_be(n);
    let k = n_int.bits().div_ceil(8) as usize;
    if message.len() + 2 * 20 + 2 > k {
        return Err("the password is too long for the public key of the server".into());
    }
    let mut db = Vec::with_capacity(k - 21);
    db.extend_from_slice(&Sha1::digest(b""));
    db.resize(k - message.len() - 22, 0);
    db.push(1);
    db.extend_from_slice(message);
    let db_mask = mgf1(seed, k - 21);
    let masked_db: Vec<u8> = db.iter().zip(db_mask).map(|(a, b)| a ^ b).collect();
    let seed_mask = mgf1(&masked_db, 20);
    let mut em = Vec::with_capacity(k);
    em.push(0);
    em.extend(seed.iter().zip(seed_mask).map(|(a, b)| a ^ b));
    em.extend_from_slice(&masked_db);
    let c = BigUint::from_bytes_be(&em).modpow(&BigUint::from_bytes_be(e), &n_int);
    let bytes = c.to_bytes_be();
    let mut out = vec![0u8; k - bytes.len()];
    out.extend_from_slice(&bytes);
    Ok(out)
}

fn mgf1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut counter = 0u32;
    while out.len() < len {
        let mut h = Sha1::new();
        h.update(seed);
        h.update(counter.to_be_bytes());
        out.extend_from_slice(&h.finalize());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// The modulus and the exponent of a PEM public key: SubjectPublicKeyInfo (`PUBLIC KEY`) or
/// PKCS #1 (`RSA PUBLIC KEY`).
pub(crate) fn public_key(pem: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if pem.len() > MAX_PEM {
        return None;
    }
    let text = std::str::from_utf8(pem).ok()?;
    let begin = text.find("-----BEGIN ")?;
    let body_start = begin + text[begin..].find('\n')? + 1;
    let body_end = body_start + text[body_start..].find("-----END ")?;
    let b64: String = text[body_start..body_end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    use base64::Engine;
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let mut outer = Der(&der);
    let mut seq = Der(outer.take(0x30)?);
    if seq.0.first() == Some(&0x30) {
        seq.take(0x30)?;
        let bits = seq.take(0x03)?;
        let mut inner = Der(bits.get(1..)?);
        let mut key = Der(inner.take(0x30)?);
        let (n, e) = (key.integer()?, key.integer()?);
        return usable_key(&n, &e).then_some((n, e));
    }
    let (n, e) = (seq.integer()?, seq.integer()?);
    usable_key(&n, &e).then_some((n, e))
}

/// A reader of DER: one tag-length-value after another.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    fn take(&mut self, tag: u8) -> Option<&'a [u8]> {
        let (&t, rest) = self.0.split_first()?;
        if t != tag {
            return None;
        }
        let (&first, mut rest) = rest.split_first()?;
        let len = if first < 0x80 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return None;
            }
            let len = rest[..n].iter().fold(0usize, |a, b| (a << 8) | *b as usize);
            rest = &rest[n..];
            len
        };
        if rest.len() < len {
            return None;
        }
        let (value, after) = rest.split_at(len);
        self.0 = after;
        Some(value)
    }

    fn integer(&mut self) -> Option<Vec<u8>> {
        let v = self.take(0x02)?;
        let start = v.iter().position(|b| *b != 0).unwrap_or(v.len());
        Some(v[start..].to_vec())
    }
}
