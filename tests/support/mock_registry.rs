//! A reference registry HTTP server backed by a `FileRegistry`, so tests can run
//! over `HttpRegistry` without the Worker. One request per connection.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use fpkg_core::registry::{Conditional, FileRegistry, Registry, RegistryError, Token, VersionMeta};
use fpkg_core::{PackageId, Version};

pub struct MockRegistry {
    pub url: String,
    pub token: String,
    pub login: String,
    /// Requests still to be answered with 503 (to exercise retries).
    pub fail_next: Arc<AtomicU32>,
    /// Requests still to be answered with a bare 503 that has no
    /// X-Fpkg-Api, as a proxy in front of the registry would send.
    pub fail_bare: Arc<AtomicU32>,
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn json(status: u16, v: &impl serde::Serialize) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: serde_json::to_vec(v).unwrap(),
    }
}

fn error(status: u16, code: &str, message: &str, details: &[String]) -> Response {
    let mut e = serde_json::json!({ "error": { "code": code, "message": message } });
    if !details.is_empty() {
        e["error"]["details"] = serde_json::json!(details);
    }
    json(status, &e)
}

fn from_registry(e: RegistryError) -> Response {
    match e {
        RegistryError::Api(a) => error(a.status, &a.code, &a.message, &a.details),
        other => error(500, "internal", &other.to_string(), &[]),
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(x);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect();
    Some(Request { method, path: path.to_string(), query, headers, body })
}

fn write_response(stream: &mut TcpStream, r: Response) {
    let reason = match r.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        304 => "Not Modified",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\nX-Fpkg-Api: 1\r\nConnection: close\r\nContent-Length: {}\r\n", r.status, r.body.len());
    for (k, v) in &r.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&r.body);
    let _ = stream.flush();
}

/// The `metadata` and `archive` parts of a multipart/form-data body.
fn multipart(content_type: &str, body: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let boundary = content_type.split("boundary=").nth(1)?.trim_matches('"');
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    let mut positions = Vec::new();
    let mut i = 0;
    while i + d.len() <= body.len() {
        if &body[i..i + d.len()] == d {
            positions.push(i);
            i += d.len();
        } else {
            i += 1;
        }
    }
    let (mut meta, mut archive) = (None, None);
    for w in positions.windows(2) {
        let part = &body[w[0] + d.len()..w[1]];
        let part = part.strip_prefix(b"\r\n")?;
        let split = part.windows(4).position(|x| x == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&part[..split]).to_string();
        let mut content = &part[split + 4..];
        if content.ends_with(b"\r\n") {
            content = &content[..content.len() - 2];
        }
        if head.contains("name=\"metadata\"") {
            meta = Some(content.to_vec());
        } else if head.contains("name=\"archive\"") {
            archive = Some(content.to_vec());
        }
    }
    Some((meta?, archive?))
}

struct State {
    reg: FileRegistry,
    token: String,
    login: String,
    base: String,
}

impl State {
    fn authorized(&self, req: &Request) -> bool {
        req.headers.get("authorization").map(|a| a.as_str()) == Some(&format!("Bearer {}", self.token))
    }

    fn with_docs(&self, mut m: VersionMeta) -> VersionMeta {
        m.docs = Some(format!("{}docs/{}/{}/", self.base, m.id, m.version));
        m
    }

    fn handle(&self, req: Request) -> Response {
        let unauthorized = || error(401, "unauthorized", "a valid token is required", &[]);
        let Some(rest) = req.path.strip_prefix("/api/v1/") else {
            return error(404, "not_found", "no such route", &[]);
        };
        let seg: Vec<String> = rest.split('/').map(decode).collect();
        let seg: Vec<&str> = seg.iter().map(String::as_str).collect();
        let id = |s: &str| PackageId::new(s).map_err(|e| error(404, "not_found", &e.to_string(), &[]));
        let ver = |s: &str| Version::parse(s).map_err(|e| error(404, "not_found", &e.to_string(), &[]));
        let token = Token::none();
        let r = (|| -> Result<Response, Response> {
            Ok(match (req.method.as_str(), seg.as_slice()) {
                ("GET", ["me"]) => {
                    if !self.authorized(&req) {
                        return Err(unauthorized());
                    }
                    json(200, &serde_json::json!({ "login": self.login, "token_name": "mock" }))
                }
                ("GET", ["search"]) => {
                    let q = req.query.get("q").cloned().unwrap_or_default();
                    let page = req.query.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
                    json(200, &self.reg.search(&q, page).map_err(from_registry)?)
                }
                ("GET", ["packages", p]) => {
                    let pid = id(p)?;
                    let inm = req.headers.get("if-none-match").map(String::as_str);
                    match self.reg.package_conditional(&pid, inm).map_err(from_registry)? {
                        Conditional::NotFound => error(404, "not_found", &format!("package '{p}' does not exist"), &[]),
                        Conditional::NotModified => Response { status: 304, headers: vec![("ETag".into(), inm.unwrap_or("").into())], body: vec![] },
                        Conditional::Modified { mut doc, etag } => {
                            for m in doc.versions.values_mut() {
                                *m = self.with_docs(m.clone());
                            }
                            let mut r = json(200, &doc);
                            r.headers.push(("ETag".into(), etag.unwrap_or_default()));
                            r.headers.push(("Cache-Control".into(), "public, max-age=60, stale-while-revalidate=600".into()));
                            r
                        }
                    }
                }
                ("GET", ["packages", p, "owners"]) => {
                    json(200, &serde_json::json!({ "owners": self.reg.owners(&id(p)?).map_err(from_registry)? }))
                }
                ("PUT" | "DELETE", ["packages", p, "owners", login]) => {
                    if !self.authorized(&req) {
                        return Err(unauthorized());
                    }
                    let add = req.method == "PUT";
                    if add && *login != self.login {
                        return Err(error(404, "not_found", &format!("no user '{login}'"), &[]));
                    }
                    self.reg.set_owner(&id(p)?, login, add, &token).map_err(from_registry)?;
                    json(200, &serde_json::json!({ "owners": self.reg.owners(&id(p)?).map_err(from_registry)? }))
                }
                ("GET", ["packages", p, "versions", v]) => match self.reg.version(&id(p)?, &ver(v)?).map_err(from_registry)? {
                    Some(m) => json(200, &self.with_docs(m)),
                    None => error(404, "not_found", &format!("{p} {v} does not exist"), &[]),
                },
                ("GET", ["packages", p, "versions", v, "archive"]) => {
                    let bytes = self.reg.archive(&id(p)?, &ver(v)?).map_err(from_registry)?;
                    let sha = fpkg_core::sha256_hex(&bytes);
                    Response {
                        status: 200,
                        headers: vec![
                            ("Content-Type".into(), "application/zip".into()),
                            ("ETag".into(), format!("\"{sha}\"")),
                            ("X-Fpkg-Sha256".into(), sha),
                        ],
                        body: bytes,
                    }
                }
                ("POST", ["packages", p, "versions", v, action @ ("yank" | "unyank")]) => {
                    if !self.authorized(&req) {
                        return Err(unauthorized());
                    }
                    self.reg.yank(&id(p)?, &ver(v)?, *action == "yank", &token).map_err(from_registry)?;
                    json(200, &serde_json::json!({ "ok": true }))
                }
                ("PUT", ["packages", p, "versions", v]) => {
                    if !self.authorized(&req) {
                        return Err(unauthorized());
                    }
                    let ct = req.headers.get("content-type").cloned().unwrap_or_default();
                    let Some((meta, archive)) = multipart(&ct, &req.body) else {
                        return Err(error(400, "bad_request", "expected multipart metadata and archive", &[]));
                    };
                    let meta: VersionMeta = serde_json::from_slice(&meta)
                        .map_err(|e| error(422, "invalid_metadata", "metadata is not valid", &[e.to_string()]))?;
                    if meta.id != *p || meta.version != *v {
                        return Err(error(422, "mismatch", "the URL and the metadata disagree", &["id/version".into()]));
                    }
                    let published = self.reg.publish(&meta, &archive, &token).map_err(from_registry)?;
                    json(201, &self.with_docs(published))
                }
                _ => error(404, "not_found", "no such route", &[]),
            })
        })();
        r.unwrap_or_else(|e| e)
    }
}

impl MockRegistry {
    /// Serves `dir` as a registry on 127.0.0.1. `login` owns what it publishes.
    pub fn start(dir: &Path, login: &str, token: &str) -> MockRegistry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/");
        let state = State { reg: FileRegistry::new(dir).with_login(login), token: token.into(), login: login.into(), base: url.clone() };
        let fail_next = Arc::new(AtomicU32::new(0));
        let fail = fail_next.clone();
        let fail_bare = Arc::new(AtomicU32::new(0));
        let bare = fail_bare.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some(req) = read_request(&mut stream) else { continue };
                if bare.load(Ordering::SeqCst) > 0 {
                    bare.fetch_sub(1, Ordering::SeqCst);
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 11\r\nConnection: close\r\n\r\nupstream ko");
                    continue;
                }
                if fail.load(Ordering::SeqCst) > 0 {
                    fail.fetch_sub(1, Ordering::SeqCst);
                    write_response(&mut stream, error(503, "unavailable", "try again", &[]));
                    continue;
                }
                let resp = state.handle(req);
                write_response(&mut stream, resp);
            }
        });
        MockRegistry { url, token: token.into(), login: login.into(), fail_next, fail_bare }
    }
}
