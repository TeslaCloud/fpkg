use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::registry::{Conditional, DepMeta, Dist, Identity, PackageDoc, SearchPage, Token, VersionMeta};
use crate::spec::{DepKind, Env, Evaluation, LuaData};

/// An in-memory registry: id -> [(version, yanked, deps)].
#[derive(Default)]
struct Mem(BTreeMap<String, PackageDoc>);

impl Mem {
    fn add(&mut self, id: &str, version: &str, yanked: bool, deps: &[(&str, &str)]) -> &mut Self {
        let doc = self.0.entry(id.to_string()).or_insert_with(|| PackageDoc {
            id: id.into(),
            revision: 1,
            owners: vec!["luna".into()],
            latest: None,
            versions: BTreeMap::new(),
        });
        doc.versions.insert(
            version.to_string(),
            VersionMeta {
                id: id.into(),
                name: id.into(),
                version: version.into(),
                kind: "package".into(),
                summary: None,
                description: None,
                authors: vec![],
                license: None,
                website: None,
                globals: vec![],
                serverside: false,
                clientside: false,
                dependencies: deps.iter().map(|(d, c)| DepMeta { id: d.to_string(), constraint: c.to_string() }).collect(),
                dist: Some(Dist { sha256: crate::sha256_hex(format!("{id}-{version}").as_bytes()), size: 1, url: String::new() }),
                docs: None,
                yanked,
                published_at: None,
                published_by: None,
                allow_missing_dependencies: false,
            },
        );
        self
    }
}

impl Registry for Mem {
    fn package(&self, id: &PackageId) -> crate::registry::Result<Option<PackageDoc>> {
        Ok(self.0.get(id.as_str()).cloned())
    }
    fn version(&self, id: &PackageId, v: &Version) -> crate::registry::Result<Option<VersionMeta>> {
        Ok(self.0.get(id.as_str()).and_then(|d| d.get(v).cloned()))
    }
    fn archive(&self, _: &PackageId, _: &Version) -> crate::registry::Result<Vec<u8>> {
        unimplemented!("not used by the resolver")
    }
    fn publish(&self, _: &VersionMeta, _: &[u8], _: &Token) -> crate::registry::Result<VersionMeta> {
        unimplemented!("not used by the resolver")
    }
    fn yank(&self, _: &PackageId, _: &Version, _: bool, _: &Token) -> crate::registry::Result<()> {
        unimplemented!("not used by the resolver")
    }
    fn owners(&self, _: &PackageId) -> crate::registry::Result<Vec<String>> {
        Ok(vec![])
    }
    fn set_owner(&self, _: &PackageId, _: &str, _: bool, _: &Token) -> crate::registry::Result<()> {
        Ok(())
    }
    fn search(&self, _: &str, _: u32) -> crate::registry::Result<SearchPage> {
        Ok(SearchPage { packages: vec![], page: 1, per_page: 20, total: 0 })
    }
    fn me(&self, _: &Token) -> crate::registry::Result<Identity> {
        Ok(Identity { login: "luna".into(), token_name: None })
    }
    fn package_conditional(&self, id: &PackageId, _: Option<&str>) -> crate::registry::Result<Conditional> {
        Ok(match self.package(id)? {
            Some(doc) => Conditional::Modified { doc, etag: None },
            None => Conditional::NotFound,
        })
    }
}

fn manifest(id: &str, version: &str, deps: &[(&str, &str)]) -> Manifest {
    let mut ev = Evaluation { described: true, ..Default::default() };
    ev.fields.insert("name".into(), LuaData::String(id.into()));
    ev.fields.insert("version".into(), LuaData::String(version.into()));
    ev.depends = deps.iter().map(|(d, c)| (d.to_string(), Some(c.to_string()))).collect();
    Manifest::from_evaluation(id, id, ev).unwrap()
}

fn deps(list: &[(&str, &str)]) -> Vec<Dependency> {
    list.iter().map(|(d, c)| Dependency { id: d.to_string(), constraint: Constraint::parse(c).unwrap(), kind: DepKind::Package }).collect()
}

const URL: &str = "https://fluxpkg.dev/";

fn run(
    root: &[(&str, &str)],
    ws: &[Manifest],
    reg: &Mem,
    lock: Option<&Lockfile>,
    release: Release,
) -> Result<Lockfile, ResolveError> {
    let root = deps(root);
    let workspace: BTreeMap<String, Manifest> = ws.iter().map(|m| (m.id.clone(), m.clone())).collect();
    let paths = BTreeMap::new();
    let v = Version::new(0, 9, 0);
    resolve(&ResolveInput {
        project: ("flux", &v),
        root_deps: &root,
        workspace: &workspace,
        paths: &paths,
        lock,
        release,
        registry: Some((reg, URL)),
    })
}

fn version_of(l: &Lockfile, id: &str) -> String {
    l.get(id).unwrap().version.to_string()
}

#[test]
fn resolves_flux_ce_workspace_without_a_registry() {
    crate::spec::test_in_process();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/flux-ce");
    let project = crate::project::Project::new(root.canonicalize().unwrap());
    let rm = crate::spec::load(&project.root_location(), Env::Production).unwrap();
    let mut ws = BTreeMap::new();
    for p in project.packages().unwrap() {
        ws.insert(p.id.clone(), crate::spec::load(&project.package_location(&p.id), Env::Production).unwrap());
    }
    let paths = BTreeMap::new();
    let lock = resolve(&ResolveInput {
        project: ("flux", &rm.version),
        root_deps: &rm.dependencies,
        workspace: &ws,
        paths: &paths,
        lock: None,
        release: Release::None,
        registry: None,
    })
    .unwrap();
    assert_eq!(lock.packages.len(), 16);
    assert!(lock.packages.iter().all(|p| p.source == Source::Workspace && p.sha256.is_none()));
    assert_eq!(lock.get("bolt").unwrap().dependencies, vec!["active_record", "active_ui", "cami", "flow"]);
    assert_eq!(version_of(&lock, "cami"), "20211019.0.0");
}

#[test]
fn newest_matching_and_transitive() {
    let mut reg = Mem::default();
    reg.add("sfs", "7.0.8", false, &[]).add("sfs", "7.0.9", false, &[]).add("sfs", "8.0.0", false, &[]);
    reg.add("cable", "1.0.0", false, &[("sfs", "~> 7.0")]);
    let l = run(&[("cable", "*")], &[], &reg, None, Release::None).unwrap();
    assert_eq!(version_of(&l, "cable"), "1.0.0");
    assert_eq!(version_of(&l, "sfs"), "7.0.9");
    let c = l.get("cable").unwrap();
    assert_eq!(c.source, Source::Registry(URL.into()));
    assert_eq!(c.dependencies, vec!["sfs"]);
    assert_eq!(c.sha256.as_deref().map(str::len), Some(64));
    // No constraint: the newest non-yanked version.
    let l = run(&[("sfs", "*")], &[], &reg, None, Release::None).unwrap();
    assert_eq!(version_of(&l, "sfs"), "8.0.0");
}

#[test]
fn prereleases_only_when_needed() {
    let mut reg = Mem::default();
    reg.add("x", "1.0.0", false, &[]).add("x", "2.0.0-beta", false, &[]);
    assert_eq!(version_of(&run(&[("x", "*")], &[], &reg, None, Release::None).unwrap(), "x"), "1.0.0");
    assert_eq!(version_of(&run(&[("x", ">= 2.0.0-alpha")], &[], &reg, None, Release::None).unwrap(), "x"), "2.0.0-beta");
    // A range that names no pre-release never selects one.
    let mut reg = Mem::default();
    reg.add("y", "1.0.0", false, &[]).add("y", "1.1.0-beta", false, &[]);
    assert_eq!(version_of(&run(&[("y", "~> 1.0")], &[], &reg, None, Release::None).unwrap(), "y"), "1.0.0");
    let mut only_pre = Mem::default();
    only_pre.add("z", "1.1.0-beta", false, &[]);
    assert!(run(&[("z", "~> 1.0")], &[], &only_pre, None, Release::None).is_err());
    assert!(run(&[("z", "< 2.0")], &[], &only_pre, None, Release::None).is_err());
    // `*` takes a pre-release only when there is no release at all.
    assert_eq!(version_of(&run(&[("z", "*")], &[], &only_pre, None, Release::None).unwrap(), "z"), "1.1.0-beta");
    assert_eq!(version_of(&run(&[("z", "~> 1.1.0-alpha")], &[], &only_pre, None, Release::None).unwrap(), "z"), "1.1.0-beta");
}

#[test]
fn lock_preference_and_update() {
    let mut reg = Mem::default();
    reg.add("sfs", "7.0.8", false, &[]);
    let old = run(&[("sfs", "~> 7.0")], &[], &reg, None, Release::None).unwrap();
    reg.add("sfs", "7.1.0", false, &[]);
    let kept = run(&[("sfs", "~> 7.0")], &[], &reg, Some(&old), Release::None).unwrap();
    assert_eq!(version_of(&kept, "sfs"), "7.0.8");
    let all = run(&[("sfs", "~> 7.0")], &[], &reg, Some(&old), Release::All).unwrap();
    assert_eq!(version_of(&all, "sfs"), "7.1.0");
    let some = run(&[("sfs", "~> 7.0")], &[], &reg, Some(&old), Release::Ids(["sfs".to_string()].into())).unwrap();
    assert_eq!(version_of(&some, "sfs"), "7.1.0");
    // A locked version the constraint does not allow is released.
    let moved = run(&[("sfs", ">= 7.1")], &[], &reg, Some(&old), Release::None).unwrap();
    assert_eq!(version_of(&moved, "sfs"), "7.1.0");
}

#[test]
fn yanked_pin_survives_but_new_resolutions_skip_it() {
    let mut reg = Mem::default();
    reg.add("sfs", "7.0.8", false, &[]).add("sfs", "7.0.9", false, &[]);
    let lock = run(&[("sfs", "~> 7.0")], &[], &reg, None, Release::None).unwrap();
    assert_eq!(version_of(&lock, "sfs"), "7.0.9");
    reg.0.get_mut("sfs").unwrap().versions.get_mut("7.0.9").unwrap().yanked = true;
    let kept = run(&[("sfs", "~> 7.0")], &[], &reg, Some(&lock), Release::None).unwrap();
    assert_eq!(version_of(&kept, "sfs"), "7.0.9");
    let fresh = run(&[("sfs", "~> 7.0")], &[], &reg, None, Release::None).unwrap();
    assert_eq!(version_of(&fresh, "sfs"), "7.0.8");
    let updated = run(&[("sfs", "~> 7.0")], &[], &reg, Some(&lock), Release::All).unwrap();
    assert_eq!(version_of(&updated, "sfs"), "7.0.8");
}

#[test]
fn conflict_is_explained() {
    let mut reg = Mem::default();
    reg.add("active_ui", "1.0.0", false, &[]).add("active_ui", "2.0.0", false, &[]);
    reg.add("client_settings", "2.0.0", false, &[("active_ui", "~> 2.0")]);
    let bolt = manifest("bolt", "1.0.0", &[("active_ui", "~> 1.0")]);
    let e = run(&[("bolt", "*"), ("client_settings", "*")], &[bolt], &reg, None, Release::None).unwrap_err();
    let msg = e.to_string();
    assert!(matches!(e, ResolveError::NoSolution(_)));
    // Pubgrub generalises a package with one candidate to all its versions.
    assert!(msg.contains("bolt depends on active_ui ~> 1.0"), "{msg}");
    assert!(!msg.contains(" *"), "no raw range artefacts: {msg}");
    assert!(msg.contains("client_settings 2.0.0 depends on active_ui ~> 2.0"), "{msg}");
}

#[test]
fn workspace_versions_are_fixed() {
    let reg = Mem::default();
    let cable = manifest("cable", "1.0.0", &[]);
    let e = run(&[("cable", "~> 2.0")], &[cable], &reg, None, Release::None).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("flux 0.9.0 wants cable ~> 2.0, but cable is a local package at 1.0.0"), "{msg}");
}

#[test]
fn missing_package_is_named() {
    let reg = Mem::default();
    let e = run(&[("nope", "*")], &[], &reg, None, Release::None).unwrap_err();
    assert!(e.to_string().contains("nope is not a local package and not on the registry https://fluxpkg.dev/"), "{e}");
}

#[test]
fn cycles_are_refused() {
    let mut reg = Mem::default();
    reg.add("a", "1.0.0", false, &[("b", "*")]).add("b", "1.0.0", false, &[("c", "*")]).add("c", "1.0.0", false, &[("a", "*")]);
    let e = run(&[("a", "*")], &[], &reg, None, Release::None).unwrap_err();
    match e {
        ResolveError::Cycle(c) => assert_eq!(c, vec!["a", "b", "c", "a"]),
        other => panic!("{other}"),
    }
    let me = manifest("me", "1.0.0", &[("me", "*")]);
    assert!(matches!(run(&[], &[me], &reg, None, Release::None), Err(ResolveError::Cycle(_))));
    let x = manifest("x", "1.0.0", &[("y", "*")]);
    let y = manifest("y", "1.0.0", &[("x", "*")]);
    assert!(matches!(run(&[], &[x, y], &reg, None, Release::None), Err(ResolveError::Cycle(_))));
}

#[test]
fn offline_lists_missing_ids() {
    let reg = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let fr = crate::registry::FileRegistry::new(reg.path());
    let url = fr.url().to_string();
    let cached = crate::registry::CachedRegistry::new(fr, cache.path(), &url, true);
    let root = deps(&[("sfs", "*"), ("cable", "*")]);
    let (ws, paths) = (BTreeMap::new(), BTreeMap::new());
    let v = Version::new(1, 0, 0);
    let e = resolve(&ResolveInput {
        project: ("p", &v),
        root_deps: &root,
        workspace: &ws,
        paths: &paths,
        lock: None,
        release: Release::None,
        registry: Some((&cached, &url)),
    })
    .unwrap_err();
    match e {
        ResolveError::Offline(ids) => assert_eq!(ids, vec!["cable", "sfs"]),
        other => panic!("{other}"),
    }
}

#[test]
fn topological_order() {
    let mut g = BTreeMap::new();
    g.insert("bolt".to_string(), vec!["flow".to_string(), "cami".to_string()]);
    g.insert("cami".to_string(), vec![]);
    g.insert("flow".to_string(), vec!["sfs".to_string()]);
    g.insert("sfs".to_string(), vec![]);
    let order = topological(&g).unwrap();
    let pos = |x: &str| order.iter().position(|y| y == x).unwrap();
    assert!(pos("sfs") < pos("flow") && pos("flow") < pos("bolt") && pos("cami") < pos("bolt"));
}
