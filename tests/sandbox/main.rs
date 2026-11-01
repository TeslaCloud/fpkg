//! Hostile specs against the process evaluator: each must fail quickly, without
//! crashing or hanging fpkg.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fpkg_core::project::Project;
use fpkg_core::spec::{self, Env, Evaluator, SpecError, SpecLocation};

fn evaluator() -> Evaluator {
    Evaluator::Process { program: PathBuf::from(env!("CARGO_BIN_EXE_fpkg-eval-spec")), args: Vec::new() }
}

fn setup() {
    spec::set_evaluator(evaluator());
}

/// A one-package project whose spec is `src`.
fn project_with(src: &str) -> (tempfile::TempDir, Project) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    std::fs::write(root.join("packagespec.lua"), "Package:describe(function(s) s.version = '1.0.0' end)").unwrap();
    std::fs::create_dir_all(root.join("packages/evil")).unwrap();
    std::fs::write(root.join("packages/evil/packagespec.lua"), src).unwrap();
    let p = Project::new(root);
    (t, p)
}

#[track_caller]
fn refused(src: &str, want: &[&str]) -> Duration {
    setup();
    let (_t, p) = project_with(src);
    let start = Instant::now();
    let e = spec::load(&p.package_location("evil"), Env::Production).expect_err("a hostile spec must fail");
    let took = start.elapsed();
    let msg = e.to_string();
    assert!(want.iter().any(|w| msg.contains(w)), "unexpected error for {src:?}: {msg}");
    assert!(took < Duration::from_secs(6), "{src:?} took {took:?}");
    println!("{took:>10.2?}  {msg}");
    took
}

#[test]
fn c_recursion_is_an_error_not_a_crash() {
    refused("local function f(s) return (s:gsub('.', f)) end f('ab')", &["stack overflow", "C stack", "evaluator"]);
}

#[test]
fn coroutines_do_not_escape_the_budget() {
    refused("coroutine.wrap(function() while true do end end)()", &["instruction budget"]);
}

#[test]
fn pcall_cannot_outrun_the_budget() {
    refused("while true do pcall(function() while true do end end) end", &["instruction budget"]);
    refused("while true do pcall(pcall, function() while true do end end) end", &["instruction budget"]);
}

#[test]
fn shared_tables_do_not_explode() {
    refused(
        "local t = {'x'}\nfor i = 1, 30 do t = {t, t} end\nPackage:describe(function(s) s.version = '1.0.0'; s.files = t end)",
        &["appears twice", "nested more than"],
    );
}

#[test]
fn memory_is_bounded() {
    refused("for i = 1, 6 do _G['x' .. i] = string.rep('x', 2^29) end", &["larger than 16 MiB"]);
    refused("local s = 'x'\nfor i = 1, 40 do s = s .. s end", &["memory", "not enough"]);
    refused("local t = {}\nfor i = 1, 1e9 do t[i] = ('y'):rep(1024 * 1024) .. i end", &["memory", "not enough", "instruction budget"]);
}

#[test]
fn a_crashing_or_spinning_evaluator_is_reported() {
    let (_t, p) = project_with("Package:describe(function(s) s.version = '1.0.0' end)");
    let loc = p.package_location("evil");
    let file = loc.spec_path();
    let sh = |script: &str| Evaluator::Process { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()] };
    let e = spec::load_with(&sh("echo partial >&2; kill -ABRT $$"), &loc, Env::Production, &file).unwrap_err();
    assert!(matches!(&e, SpecError::Crashed { status, stderr, .. } if status.contains("SIGABRT") && stderr.contains("partial")), "{e}");
    let start = Instant::now();
    let e = spec::load_with(&sh("while :; do :; done"), &loc, Env::Production, &file).unwrap_err();
    assert!(matches!(&e, SpecError::Crashed { .. }), "{e}");
    assert!(start.elapsed() < Duration::from_secs(6), "{:?}", start.elapsed());
    let e = spec::load_with(&sh("exec sleep 30"), &loc, Env::Production, &file).unwrap_err();
    assert!(e.to_string().contains("5 s of wall clock"), "{e}");
    let e = spec::load_with(&sh("yes | head -c 3000000"), &loc, Env::Production, &file).unwrap_err();
    assert!(e.to_string().contains("1 MiB of output") || e.to_string().contains("unreadable reply"), "{e}");
}

#[test]
fn flux_ce_in_child_processes() {
    setup();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/flux-ce").canonicalize().unwrap();
    let p = Project::new(root);
    let mut locs: Vec<SpecLocation> = vec![p.root_location()];
    locs.extend(p.packages().unwrap().iter().map(|x| p.package_location(&x.id)));
    let start = Instant::now();
    let results = spec::load_many(&locs, Env::Production);
    let took = start.elapsed();
    assert_eq!(results.len(), 17);
    for (loc, r) in locs.iter().zip(&results) {
        let m = r.as_ref().unwrap_or_else(|e| panic!("{}: {e}", loc.id));
        assert_eq!(m.id, loc.id);
    }
    let sfs = results.iter().flatten().find(|m| m.id == "sfs").unwrap();
    assert_eq!(sfs.client_env, vec![("SFS_PATH".to_string(), "flux/packages/sfs/".to_string())]);
    let fa = results.iter().flatten().find(|m| m.id == "fontawesome").unwrap();
    assert_eq!(fa.resources.len(), 3);
    println!("17 specs in child processes: {took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
}
