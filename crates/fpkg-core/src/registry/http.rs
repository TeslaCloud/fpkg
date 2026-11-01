//! The registry HTTP client, on `ureq`: conditional requests, retries, the
//! `X-Fpkg-Api` check and the error envelope.

use std::time::Duration;

use serde::Deserialize;

use super::{
    API_VERSION, ApiError, Conditional, Identity, NetOptions, PER_PAGE, PackageDoc, Registry, RegistryError, Result,
    SearchPage, Token, VersionMeta,
};
use crate::PackageId;
use crate::version::Version;

/// Responses are read into memory up to this size: the archive limit plus
/// room for a JSON document of many versions.
const MAX_BODY: u64 = crate::archive::MAX_COMPRESSED + 1024 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(8);
const MAX_RETRY_AFTER: u64 = 30;

#[derive(Debug)]
pub struct HttpRegistry {
    /// The registry's root, `https://fluxpkg.dev/`.
    root: url::Url,
    /// `<root>api/v1/`.
    api: url::Url,
    agent: ureq::Agent,
    retries: u32,
}

enum Method {
    Get,
    Put,
    Post,
    Delete,
}

struct Reply {
    status: u16,
    etag: Option<String>,
    sha256: Option<String>,
    /// `X-Fpkg-Api`.
    api: Option<String>,
    retry_after: Option<String>,
    /// How many earlier attempts failed and were retried.
    retried: u32,
    body: Vec<u8>,
}

/// Is plain HTTP acceptable for this URL (loopback only)?
pub fn is_loopback(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

impl HttpRegistry {
    pub fn new(url: &str, net: &NetOptions) -> Result<HttpRegistry> {
        let u = url::Url::parse(url).map_err(|e| RegistryError::Invalid(format!("invalid registry URL '{url}': {e}")))?;
        match u.scheme() {
            "https" => {}
            "http" if is_loopback(&u) => {}
            "http" => return Err(RegistryError::Insecure(url.to_string())),
            other => return Err(RegistryError::Invalid(format!("unsupported scheme '{other}' in '{url}'"))),
        }
        let root = Self::root_of(&u);
        let api = root.join("api/v1/").map_err(|e| RegistryError::Invalid(e.to_string()))?;
        // No overall deadline, so large archives on slow links finish; each
        // connect, read or write must progress within the timeout.
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(net.timeout))
            .timeout_resolve(Some(net.timeout))
            .timeout_per_read(Some(net.timeout))
            .timeout_per_write(Some(net.timeout))
            .timeout_recv_response(Some(net.timeout))
            .user_agent(format!("fpkg/{}", crate::FPKG_VERSION))
            .build();
        Ok(HttpRegistry { root, api, agent: config.into(), retries: net.retries })
    }

    /// The registry root of a URL that may name the API base (`.../api/v1/`).
    pub fn root_of(u: &url::Url) -> url::Url {
        let mut root = u.clone();
        root.set_query(None);
        root.set_fragment(None);
        let mut path = root.path().trim_end_matches('/').to_string();
        if let Some(p) = path.strip_suffix("/api/v1") {
            path = p.to_string();
        }
        root.set_path(&format!("{path}/"));
        root
    }

    pub fn url(&self) -> &str {
        self.root.as_str()
    }

    fn endpoint(&self, segments: &[&str]) -> url::Url {
        let mut u = self.api.clone();
        if let Ok(mut s) = u.path_segments_mut() {
            s.pop_if_empty().extend(segments);
        }
        u
    }

    fn transport(&self, u: &url::Url, e: impl std::fmt::Display) -> RegistryError {
        RegistryError::Transport { url: u.to_string(), message: e.to_string() }
    }

    fn once(
        &self,
        method: &Method,
        u: &url::Url,
        headers: &[(&str, String)],
        body: Option<&(String, Vec<u8>)>,
        auth: Option<&Token>,
    ) -> std::result::Result<Reply, ureq::Error> {
        macro_rules! with_headers {
            ($req:expr) => {{
                let mut r = $req.header("Accept", "application/json").header("X-Fpkg-Api", API_VERSION);
                for (k, v) in headers {
                    r = r.header(*k, v.as_str());
                }
                if let Some(t) = auth {
                    r = r.header("Authorization", format!("Bearer {}", t.secret()));
                }
                r
            }};
        }
        let s = u.as_str();
        let mut resp = match method {
            Method::Get => with_headers!(self.agent.get(s)).call()?,
            Method::Delete => with_headers!(self.agent.delete(s)).call()?,
            Method::Put | Method::Post => {
                let req = match method {
                    Method::Put => self.agent.put(s),
                    _ => self.agent.post(s),
                };
                let req = with_headers!(req);
                match body {
                    Some((ct, bytes)) => req.header("Content-Type", ct.as_str()).send(&bytes[..])?,
                    None => req.send_empty()?,
                }
            }
        };
        let status = resp.status().as_u16();
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let api = header("x-fpkg-api");
        let etag = header("etag");
        let sha256 = header("x-fpkg-sha256");
        let retry_after = header("retry-after");
        let body = if status == 304 { Vec::new() } else { resp.body_mut().with_config().limit(MAX_BODY).read_to_vec()? };
        Ok(Reply { status, etag, sha256, api, retry_after, retried: 0, body })
    }

    /// Sends with retries: transport errors, `429` (honouring `Retry-After`)
    /// and `502`/`503`/`504` are retried with exponential backoff.
    fn request(
        &self,
        method: Method,
        u: url::Url,
        headers: &[(&str, String)],
        body: Option<(String, Vec<u8>)>,
        auth: Option<&Token>,
    ) -> Result<Reply> {
        if auth.is_some() && u.scheme() != "https" && !is_loopback(&u) {
            return Err(RegistryError::Insecure(u.to_string()));
        }
        let mut attempt = 0u32;
        loop {
            let delay = MAX_BACKOFF.min(Duration::from_millis(500) * 2u32.saturating_pow(attempt));
            match self.once(&method, &u, headers, body.as_ref(), auth) {
                // Retry on the status alone: an edge proxy's 502/503/504 or a
                // 429 need not carry X-Fpkg-Api.
                Ok(r) if matches!(r.status, 429 | 502 | 503 | 504) && attempt < self.retries => {
                    let wait = match r.status {
                        429 => r
                            .retry_after
                            .as_deref()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .map(|s| Duration::from_secs(s.min(MAX_RETRY_AFTER)))
                            .unwrap_or(delay),
                        _ => delay,
                    };
                    std::thread::sleep(wait);
                }
                // 2xx and 4xx must carry X-Fpkg-Api; a 304 may come from a cache
                // that drops headers, a 5xx from in front of the Worker.
                Ok(r) if (200..500).contains(&r.status) && r.status != 304 && r.api.as_deref() != Some(API_VERSION) => {
                    let message = match r.api.as_deref() {
                        None | Some("") => format!("the response (HTTP {}) has no X-Fpkg-Api header", r.status),
                        Some(got) => {
                            format!("the registry speaks API version {got}, this fpkg speaks {API_VERSION}; upgrade fpkg")
                        }
                    };
                    return Err(RegistryError::BadApi { url: self.root.to_string(), message });
                }
                Ok(mut r) => {
                    r.retried = attempt;
                    return Ok(r);
                }
                Err(e) if attempt < self.retries && retryable(&e) => std::thread::sleep(delay),
                Err(e) => return Err(self.transport(&u, e)),
            }
            attempt += 1;
        }
    }

    fn get(&self, segments: &[&str], headers: &[(&str, String)], auth: Option<&Token>) -> Result<Reply> {
        self.request(Method::Get, self.endpoint(segments), headers, None, auth)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, r: &Reply) -> Result<T> {
        serde_json::from_slice(&r.body)
            .map_err(|e| RegistryError::Invalid(format!("{}: unexpected response: {e}", self.root)))
    }
}

fn retryable(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed
    )
}

#[derive(Deserialize)]
struct Envelope {
    error: EnvelopeError,
}

#[derive(Deserialize)]
struct EnvelopeError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: Vec<serde_json::Value>,
}

fn detail_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => {
            let field = ["field", "path", "name"].iter().find_map(|k| o.get(*k).and_then(|x| x.as_str()));
            let msg = ["message", "reason", "error"].iter().find_map(|k| o.get(*k).and_then(|x| x.as_str()));
            match (field, msg) {
                (Some(f), Some(m)) => format!("{f}: {m}"),
                (None, Some(m)) => m.to_string(),
                _ => v.to_string(),
            }
        }
        other => other.to_string(),
    }
}

/// The typed error of an error response (`{ "error": { code, message, details } }`).
fn api_error(r: &Reply) -> RegistryError {
    let mut e = match serde_json::from_slice::<Envelope>(&r.body) {
        Ok(env) => ApiError::new(
            r.status,
            if env.error.code.is_empty() { "unknown" } else { &env.error.code },
            if env.error.message.is_empty() { format!("HTTP {}", r.status) } else { env.error.message },
        )
        .with_details(env.error.details.iter().map(detail_text).collect()),
        Err(_) => {
            let text = String::from_utf8_lossy(&r.body);
            let text: String = text.chars().take(200).collect();
            ApiError::new(r.status, &format!("http_{}", r.status), if text.trim().is_empty() { format!("HTTP {}", r.status) } else { text })
        }
    };
    e.retry_after = r.retry_after.as_deref().and_then(|s| s.trim().parse().ok());
    RegistryError::Api(e)
}

fn ok_or_error(r: Reply) -> Result<Reply> {
    if (200..300).contains(&r.status) || r.status == 304 { Ok(r) } else { Err(api_error(&r)) }
}

/// A `multipart/form-data` body with the `metadata` and `archive` parts.
fn multipart(meta: &[u8], archive: &[u8], filename: &str) -> (String, Vec<u8>) {
    let mut n = 0u32;
    let boundary = loop {
        let b = format!("fpkg-{}-{n}", &crate::sha256_hex(archive)[..24]);
        let needle = b.as_bytes();
        if !archive.windows(needle.len()).any(|w| w == needle) && !meta.windows(needle.len()).any(|w| w == needle) {
            break b;
        }
        n += 1;
    };
    let mut body = Vec::with_capacity(archive.len() + meta.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    body.extend_from_slice(meta);
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"archive\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(archive);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OwnersReply {
    List(Vec<OwnerEntry>),
    Wrapped { owners: Vec<OwnerEntry> },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OwnerEntry {
    Login(String),
    Object { login: String },
}

impl Registry for HttpRegistry {
    fn package(&self, id: &PackageId) -> Result<Option<PackageDoc>> {
        match self.package_conditional(id, None)? {
            Conditional::Modified { doc, .. } => Ok(Some(doc)),
            Conditional::NotFound => Ok(None),
            Conditional::NotModified => Err(RegistryError::Invalid("304 without If-None-Match".into())),
        }
    }

    fn package_conditional(&self, id: &PackageId, etag: Option<&str>) -> Result<Conditional> {
        let headers: Vec<(&str, String)> = etag.map(|e| ("If-None-Match", e.to_string())).into_iter().collect();
        let r = self.get(&["packages", id.as_str()], &headers, None)?;
        match r.status {
            304 => Ok(Conditional::NotModified),
            404 => Ok(Conditional::NotFound),
            _ => {
                let r = ok_or_error(r)?;
                let doc: PackageDoc = self.json(&r)?;
                if doc.id != id.as_str() {
                    return Err(RegistryError::Invalid(format!("asked for package {id}, the registry sent {}", doc.id)));
                }
                Ok(Conditional::Modified { doc, etag: r.etag })
            }
        }
    }

    fn version(&self, id: &PackageId, v: &Version) -> Result<Option<VersionMeta>> {
        let r = self.get(&["packages", id.as_str(), "versions", &v.to_string()], &[], None)?;
        if r.status == 404 {
            return Ok(None);
        }
        self.json(&ok_or_error(r)?).map(Some)
    }

    fn archive(&self, id: &PackageId, v: &Version) -> Result<Vec<u8>> {
        let r = ok_or_error(self.get(&["packages", id.as_str(), "versions", &v.to_string(), "archive"], &[], None)?)?;
        let actual = crate::sha256_hex(&r.body);
        if let Some(expected) = &r.sha256
            && !expected.trim().eq_ignore_ascii_case(&actual)
        {
            return Err(RegistryError::Checksum { what: format!("{id} {v} (X-Fpkg-Sha256)"), expected: expected.clone(), actual });
        }
        Ok(r.body)
    }

    fn publish(&self, meta: &VersionMeta, archive: &[u8], auth: &Token) -> Result<VersionMeta> {
        let json = serde_json::to_vec(&meta.for_publish()).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        let body = multipart(&json, archive, &format!("{}-{}.zip", meta.id, meta.version));
        let u = self.endpoint(&["packages", &meta.id, "versions", &meta.version]);
        let r = self.request(Method::Put, u, &[], Some(body), Some(auth))?;
        if r.status == 409 && r.retried > 0 {
            // A retried upload whose first attempt reached the server: if the
            // version now holds these very bytes, that attempt published it.
            let sha = crate::sha256_hex(archive);
            if let (Ok(id), Ok(v)) = (meta.package_id(), meta.parsed_version())
                && let Ok(Some(existing)) = self.version(&id, &v)
                && existing.dist.as_ref().is_some_and(|d| d.sha256.eq_ignore_ascii_case(&sha))
            {
                return Ok(existing);
            }
        }
        self.json(&ok_or_error(r)?)
    }

    fn yank(&self, id: &PackageId, v: &Version, yanked: bool, auth: &Token) -> Result<()> {
        let action = if yanked { "yank" } else { "unyank" };
        let u = self.endpoint(&["packages", id.as_str(), "versions", &v.to_string(), action]);
        ok_or_error(self.request(Method::Post, u, &[], None, Some(auth))?).map(|_| ())
    }

    fn owners(&self, id: &PackageId) -> Result<Vec<String>> {
        let r = ok_or_error(self.get(&["packages", id.as_str(), "owners"], &[], None)?)?;
        let entries = match self.json::<OwnersReply>(&r)? {
            OwnersReply::List(l) => l,
            OwnersReply::Wrapped { owners } => owners,
        };
        Ok(entries
            .into_iter()
            .map(|e| match e {
                OwnerEntry::Login(l) => l,
                OwnerEntry::Object { login } => login,
            })
            .collect())
    }

    fn set_owner(&self, id: &PackageId, login: &str, add: bool, auth: &Token) -> Result<()> {
        let u = self.endpoint(&["packages", id.as_str(), "owners", login]);
        let method = if add { Method::Put } else { Method::Delete };
        ok_or_error(self.request(method, u, &[], None, Some(auth))?).map(|_| ())
    }

    fn search(&self, query: &str, page: u32) -> Result<SearchPage> {
        let mut u = self.endpoint(&["search"]);
        u.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("page", &page.max(1).to_string())
            .append_pair("per_page", &PER_PAGE.to_string());
        let r = ok_or_error(self.request(Method::Get, u, &[], None, None)?)?;
        self.json::<SearchPage>(&r).map(SearchPage::normalized)
    }

    fn me(&self, auth: &Token) -> Result<Identity> {
        let r = ok_or_error(self.get(&["me"], &[], Some(auth))?)?;
        self.json(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_and_endpoints() {
        let r = HttpRegistry::new("https://fluxpkg.dev", &NetOptions::default()).unwrap();
        assert_eq!(r.url(), "https://fluxpkg.dev/");
        assert_eq!(r.endpoint(&["packages", "cable"]).as_str(), "https://fluxpkg.dev/api/v1/packages/cable");
        let r = HttpRegistry::new("http://127.0.0.1:8787/api/v1/", &NetOptions::default()).unwrap();
        assert_eq!(r.url(), "http://127.0.0.1:8787/");
        assert!(matches!(HttpRegistry::new("http://example.com/", &NetOptions::default()), Err(RegistryError::Insecure(_))));
        assert!(HttpRegistry::new("http://localhost:8787/", &NetOptions::default()).is_ok());
    }

    #[test]
    fn error_envelope() {
        let r = Reply {
            status: 422,
            etag: None,
            sha256: None,
            api: Some("1".into()),
            retry_after: None,
            retried: 0,
            body: br#"{"error":{"code":"invalid_archive","message":"bad","details":["a",{"field":"version","message":"b"}]}}"#.to_vec(),
        };
        let e = api_error(&r);
        assert_eq!(e.code(), Some("invalid_archive"));
        assert_eq!(e.details(), &["a".to_string(), "version: b".to_string()]);
        assert_eq!(e.kind(), Some(super::super::ApiErrorKind::Validation));
    }

    #[test]
    fn multipart_body() {
        let (ct, body) = multipart(b"{}", b"PK\x03\x04data", "x-1.0.0.zip");
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"metadata\""));
        assert!(text.contains("name=\"archive\"; filename=\"x-1.0.0.zip\""));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }
}
