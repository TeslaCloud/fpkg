//! Registry conformance: one scenario against a `FileRegistry`, and against
//! `FPKG_TEST_REGISTRY` with `FPKG_TOKEN` when set. Per-run id tags allow reruns.

#[path = "../support/mock_registry.rs"]
mod mock_registry;

use std::path::{Path, PathBuf};

use fpkg_core::archive::{self, PackOptions};
use fpkg_core::registry::{
    ApiErrorKind, Conditional, FileRegistry, HttpRegistry, NetOptions, Registry, RegistryError, Token, VersionMeta,
};
use fpkg_core::spec::{self, Env, SpecLocation};
use fpkg_core::{PackageId, Version};

struct Target {
    name: &'static str,
    registry: Box<dyn Registry>,
    token: Token,
    /// The server checks tokens; a file registry does not.
    enforces_auth: bool,
    /// Owners must be known users (the server); a file registry takes any login.
    knows_users: bool,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/flux-ce").canonicalize().unwrap()
}

/// A package directory, its evaluated metadata under `id`, and its archive.
struct Fixture {
    meta: VersionMeta,
    bytes: Vec<u8>,
    sha256: String,
}

fn fixture(dir: &Path, id: &str, deps: &[(&str, &str)]) -> Fixture {
    spec::set_evaluator(spec::Evaluator::Process {
        program: PathBuf::from(env!("CARGO_BIN_EXE_fpkg-eval-spec")),
        args: Vec::new(),
    });
    let root = fixtures();
    let dir_name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let loc = SpecLocation {
        project_root: root.clone(),
        dir: dir.to_path_buf(),
        lua_path: format!("flux/packages/{dir_name}/"),
        id: dir_name,
        project_id: "flux".into(),
        is_project_manifest: false,
    };
    let m = spec::load(&loc, Env::Production).unwrap();
    let mut meta = VersionMeta::from_manifest(&m);
    meta.id = id.to_string();
    meta.dependencies = deps
        .iter()
        .map(|(d, c)| fpkg_core::registry::DepMeta { id: d.to_string(), constraint: c.to_string() })
        .collect();
    let packed = archive::pack(dir, id, &PackOptions::default()).unwrap();
    Fixture { meta, sha256: packed.sha256.clone(), bytes: packed.bytes }
}

/// A copy of a fixture package with its version literal changed.
fn with_version(src: &Path, version: &str, scratch: &Path) -> PathBuf {
    let dir = scratch.join(src.file_name().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(src).unwrap();
        let to = dir.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
    let spec = dir.join("packagespec.lua");
    let text = std::fs::read_to_string(&spec).unwrap();
    let re = text.lines().find(|l| l.trim_start().starts_with("s.version")).unwrap().to_string();
    std::fs::write(&spec, text.replace(&re, &format!("  s.version     = '{version}'"))).unwrap();
    dir
}

fn tag() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("c{:x}{:x}", (nanos & 0xffff_ffff_ffff) as u64, std::process::id() & 0xfff)
}

fn pid(s: &str) -> PackageId {
    PackageId::new(s).unwrap()
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[track_caller]
fn expect_status<T: std::fmt::Debug>(r: Result<T, RegistryError>, statuses: &[u16], what: &str) -> RegistryError {
    match r {
        Ok(x) => panic!("{what}: expected HTTP {statuses:?}, got success {x:?}"),
        Err(e) => {
            assert!(
                e.status().is_some_and(|s| statuses.contains(&s)),
                "{what}: expected HTTP {statuses:?}, got {e:?}"
            );
            assert!(e.code().is_some_and(|c| !c.is_empty()), "{what}: the error envelope has no code: {e:?}");
            e
        }
    }
}

fn scenario(t: &Target) {
    let r = t.registry.as_ref();
    let tag = tag();
    let scratch = tempfile::tempdir().unwrap();
    let sfs_id = format!("{tag}_sfs");
    let cable_id = format!("{tag}_cable");
    let packages = fixtures().join("packages");
    let sfs = fixture(&packages.join("sfs"), &sfs_id, &[]);
    let cable = fixture(&packages.join("cable"), &cable_id, &[(&sfs_id, "~> 7.0")]);
    let sfs71_dir = with_version(&packages.join("sfs"), "7.1.0", scratch.path());
    let sfs71 = fixture(&sfs71_dir, &sfs_id, &[]);
    println!("[{}] tag {tag}", t.name);

    // Identity.
    let me = r.me(&t.token).expect("GET /me");
    assert!(!me.login.is_empty());

    // Unknown packages.
    let unknown = pid(&format!("{tag}_nothing"));
    assert_eq!(r.package(&unknown).unwrap(), None, "GET /packages/<unknown> is 404 -> None");
    assert_eq!(r.version(&unknown, &v("1.0.0")).unwrap(), None);
    let e = expect_status(r.archive(&unknown, &v("1.0.0")), &[404], "archive of an unknown package");
    assert_eq!(e.code(), Some("not_found"));
    assert_eq!(e.kind(), Some(ApiErrorKind::NotFound));
    expect_status(r.owners(&unknown), &[404], "owners of an unknown package");

    // A dependency that does not exist: 422 with details.
    let e = expect_status(r.publish(&cable.meta, &cable.bytes, &t.token), &[422], "publish with a missing dependency");
    assert!(!e.details().is_empty(), "422 without details[]: {e:?}");
    assert!(e.details().iter().any(|d| d.contains(&sfs_id)), "details do not name the dependency: {e:?}");

    // Publish, then 409 on republish.
    let published = r.publish(&sfs.meta, &sfs.bytes, &t.token).expect("publish sfs 7.0.9");
    assert_eq!(published.id, sfs_id);
    assert_eq!(published.version, "7.0.9");
    assert_eq!(published.name, "Srlion's Fast Serializer");
    assert!(!published.yanked);
    let dist = published.dist.as_ref().expect("the publish response has dist");
    assert_eq!(dist.sha256, sfs.sha256);
    assert_eq!(dist.size, sfs.bytes.len() as u64);
    assert!(!dist.url.is_empty());
    expect_status(r.publish(&sfs.meta, &sfs.bytes, &t.token), &[409], "republish of sfs 7.0.9");

    let mut cable_meta = cable.meta.clone();
    let published = r.publish(&cable_meta, &cable.bytes, &t.token).expect("publish cable 1.0.0");
    assert_eq!(published.dependencies.len(), 1);
    assert_eq!(published.dependencies[0].id, sfs_id);
    assert_eq!(published.dependencies[0].constraint, "~> 7.0");
    r.publish(&sfs71.meta, &sfs71.bytes, &t.token).expect("publish sfs 7.1.0");

    // 422: the archive and the request disagree.
    let mut wrong_version = sfs71.meta.clone();
    wrong_version.version = "7.2.0".into();
    let e = expect_status(r.publish(&wrong_version, &sfs71.bytes, &t.token), &[422], "version literal mismatch");
    assert!(!e.details().is_empty(), "{e:?}");
    let mut wrong_name = sfs71.meta.clone();
    wrong_name.version = "7.3.0".into();
    let sfs73 = fixture(&with_version(&packages.join("sfs"), "7.3.0", &scratch.path().join("b")), &sfs_id, &[]);
    wrong_name.name = "Not The Name".into();
    let e = expect_status(r.publish(&wrong_name, &sfs73.bytes, &t.token), &[422], "name literal mismatch");
    assert!(!e.details().is_empty(), "{e:?}");
    let other_prefix = archive::pack(&packages.join("sfs"), &format!("{tag}_other"), &PackOptions::default()).unwrap();
    let mut meta73 = sfs73.meta.clone();
    meta73.version = "7.3.0".into();
    let e = expect_status(r.publish(&meta73, &other_prefix.bytes, &t.token), &[422], "members outside <id>/");
    assert!(!e.details().is_empty(), "{e:?}");
    let e = expect_status(r.publish(&meta73, b"not a zip at all", &t.token), &[400, 422], "an archive that is not a zip");
    if e.status() == Some(422) {
        assert!(!e.details().is_empty(), "{e:?}");
    }
    let mut bad_dep = meta73.clone();
    bad_dep.dependencies = vec![fpkg_core::registry::DepMeta { id: sfs_id.clone(), constraint: "bogus".into() }];
    expect_status(r.publish(&bad_dep, &sfs73.bytes, &t.token), &[400, 422], "an invalid constraint");

    // Read back.
    let doc = r.package(&pid(&sfs_id)).unwrap().expect("package document");
    assert_eq!(doc.id, sfs_id);
    assert!(doc.revision >= 2, "revision {}", doc.revision);
    assert!(doc.owners.contains(&me.login), "owners {:?}", doc.owners);
    assert_eq!(doc.latest.as_deref(), Some("7.1.0"));
    assert_eq!(doc.versions.len(), 2, "{:?}", doc.versions.keys());
    let m = &doc.versions["7.0.9"];
    assert_eq!(m.dist.as_ref().unwrap().sha256, sfs.sha256);
    assert_eq!(m.kind, "package");
    assert_eq!(m.published_by.as_deref(), Some(me.login.as_str()));
    assert!(m.published_at.is_some());
    let one = r.version(&pid(&sfs_id), &v("7.0.9")).unwrap().expect("version metadata");
    assert_eq!(one.dist.as_ref().unwrap().sha256, sfs.sha256);
    assert_eq!(one.authors, vec!["Srlion"]);
    let bytes = r.archive(&pid(&sfs_id), &v("7.0.9")).unwrap();
    assert_eq!(bytes, sfs.bytes, "the archive comes back byte for byte");
    assert_eq!(r.version(&pid(&sfs_id), &v("9.9.9")).unwrap(), None);
    expect_status(r.archive(&pid(&sfs_id), &v("9.9.9")), &[404], "archive of an unknown version");

    // ETag and 304.
    let (doc1, etag1) = match r.package_conditional(&pid(&sfs_id), None).unwrap() {
        Conditional::Modified { doc, etag } => (doc, etag.expect("the package document has an ETag")),
        other => panic!("expected 200, got {other:?}"),
    };
    assert_eq!(doc1.revision, doc.revision);
    assert!(matches!(r.package_conditional(&pid(&sfs_id), Some(&etag1)).unwrap(), Conditional::NotModified), "If-None-Match with the current ETag is 304");
    assert!(matches!(r.package_conditional(&pid(&sfs_id), Some("\"stale\"")).unwrap(), Conditional::Modified { .. }));
    assert!(matches!(r.package_conditional(&unknown, Some(&etag1)).unwrap(), Conditional::NotFound));

    // Yank and unyank.
    r.yank(&pid(&sfs_id), &v("7.1.0"), true, &t.token).expect("yank 7.1.0");
    let yanked = r.package(&pid(&sfs_id)).unwrap().unwrap();
    assert!(yanked.versions["7.1.0"].yanked);
    assert_eq!(yanked.latest.as_deref(), Some("7.0.9"), "latest skips yanked versions");
    assert!(yanked.revision > doc.revision, "a yank bumps the revision");
    let etag2 = match r.package_conditional(&pid(&sfs_id), Some(&etag1)).unwrap() {
        Conditional::Modified { etag, .. } => etag.unwrap(),
        other => panic!("after a yank the old ETag must not match: {other:?}"),
    };
    assert_ne!(etag1, etag2);
    assert!(r.version(&pid(&sfs_id), &v("7.1.0")).unwrap().unwrap().yanked);
    assert_eq!(r.archive(&pid(&sfs_id), &v("7.1.0")).unwrap(), sfs71.bytes, "yanked versions stay downloadable");
    r.yank(&pid(&sfs_id), &v("7.1.0"), false, &t.token).expect("unyank 7.1.0");
    let back = r.package(&pid(&sfs_id)).unwrap().unwrap();
    assert!(!back.versions["7.1.0"].yanked);
    assert_eq!(back.latest.as_deref(), Some("7.1.0"));
    expect_status(r.yank(&pid(&sfs_id), &v("9.9.9"), true, &t.token), &[404], "yank of an unknown version");

    // Owners.
    assert_eq!(r.owners(&pid(&sfs_id)).unwrap(), vec![me.login.clone()]);
    let e = expect_status(r.set_owner(&pid(&sfs_id), &me.login, false, &t.token), &[400, 403, 409, 422], "removing the last owner");
    assert_ne!(e.kind(), Some(ApiErrorKind::NotFound));
    assert_eq!(r.owners(&pid(&sfs_id)).unwrap(), vec![me.login.clone()]);
    if t.knows_users {
        expect_status(
            r.set_owner(&pid(&sfs_id), &format!("{tag}-no-such-user"), true, &t.token),
            &[404, 422],
            "adding an unknown user as owner",
        );
    } else {
        let friend = format!("{tag}-friend");
        let before = r.package(&pid(&sfs_id)).unwrap().unwrap().revision;
        r.set_owner(&pid(&sfs_id), &friend, true, &t.token).unwrap();
        assert!(r.owners(&pid(&sfs_id)).unwrap().contains(&friend));
        assert!(r.package(&pid(&sfs_id)).unwrap().unwrap().revision > before, "owner changes bump the revision");
        r.set_owner(&pid(&sfs_id), &friend, false, &t.token).unwrap();
        assert_eq!(r.owners(&pid(&sfs_id)).unwrap(), vec![me.login.clone()]);
    }

    // Search: prefix and substring on id, name and summary.
    let page = r.search(&tag, 1).unwrap();
    let ids: Vec<&str> = page.packages.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&sfs_id.as_str()) && ids.contains(&cable_id.as_str()), "search {tag}: {ids:?}");
    let hit = page.packages.iter().find(|h| h.id == sfs_id).unwrap();
    assert_eq!(hit.version.as_deref(), Some("7.1.0"), "search shows the newest version");
    assert_eq!(hit.name, "Srlion's Fast Serializer");
    let page = r.search(&format!("{tag}_cab"), 1).unwrap();
    assert_eq!(page.packages.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec![cable_id.as_str()]);
    let page = r.search(&format!("{tag}zzz-nothing"), 1).unwrap();
    assert!(page.packages.is_empty());
    let far = r.search(&tag, 50).unwrap();
    assert!(far.packages.is_empty(), "page 50 of two results is empty");

    // Authentication (the server only).
    if t.enforces_auth {
        let bad = Token::new("fpk_invalid_conformance_token");
        expect_status(r.me(&bad), &[401], "GET /me with a bad token");
        cable_meta.version = "1.0.1".into();
        expect_status(r.publish(&cable_meta, &cable.bytes, &bad), &[401], "publish with a bad token");
        expect_status(r.yank(&pid(&sfs_id), &v("7.0.9"), true, &bad), &[401], "yank with a bad token");
        expect_status(r.set_owner(&pid(&sfs_id), "someone", true, &bad), &[401], "owner add with a bad token");
        assert!(!r.package(&pid(&sfs_id)).unwrap().unwrap().versions["7.0.9"].yanked);
    }
    println!("[{}] conformance scenario passed", t.name);
}

#[test]
fn file_registry() {
    let dir = tempfile::tempdir().unwrap();
    let reg = FileRegistry::new(dir.path()).with_login("conformance");
    scenario(&Target {
        name: "file",
        registry: Box::new(reg),
        token: Token::none(),
        enforces_auth: false,
        knows_users: false,
    });
}

#[test]
fn http_registry() {
    let (Ok(url), Ok(token)) = (std::env::var("FPKG_TEST_REGISTRY"), std::env::var("FPKG_TOKEN")) else {
        println!("note: FPKG_TEST_REGISTRY and FPKG_TOKEN are not both set; skipping the HTTP conformance run");
        return;
    };
    let net = NetOptions { retries: 1, timeout: std::time::Duration::from_secs(60) };
    let reg = HttpRegistry::new(&url, &net).expect("FPKG_TEST_REGISTRY");
    println!("conformance against {url}");
    scenario(&Target {
        name: "http",
        registry: Box::new(reg),
        token: Token::new(token),
        enforces_auth: true,
        knows_users: true,
    });
}

/// The scenario over HTTP against the reference server of tests/support,
/// which also checks that a 503 is retried.
#[test]
fn http_registry_against_reference_server() {
    let dir = tempfile::tempdir().unwrap();
    let server = mock_registry::MockRegistry::start(dir.path(), "conformance", "fpk_mock_token");
    let net = NetOptions { retries: 3, timeout: std::time::Duration::from_secs(30) };
    let reg = HttpRegistry::new(&server.url, &net).unwrap();
    server.fail_next.store(1, std::sync::atomic::Ordering::SeqCst);
    // An edge 503 without X-Fpkg-Api is retried, not taken for a foreign server.
    server.fail_bare.store(1, std::sync::atomic::Ordering::SeqCst);
    scenario(&Target {
        name: "http-reference",
        registry: Box::new(reg),
        token: Token::new(server.token.clone()),
        enforces_auth: true,
        knows_users: true,
    });
}

/// A server that does not send X-Fpkg-Api is refused.
#[test]
fn http_registry_requires_the_api_header() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut s, &mut buf);
            let _ = std::io::Write::write_all(&mut s, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    let reg = HttpRegistry::new(&format!("http://127.0.0.1:{port}/"), &NetOptions::default()).unwrap();
    let e = reg.package(&pid("sfs")).unwrap_err();
    assert!(matches!(e, RegistryError::BadApi { .. }), "{e:?}");
}
