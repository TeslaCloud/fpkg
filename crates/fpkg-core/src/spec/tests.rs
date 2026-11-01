use std::path::{Path, PathBuf};

use super::*;
use crate::project::Project;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/flux-ce").canonicalize().unwrap()
}

fn eval_src(src: &str) -> Result<Evaluation, SpecError> {
    evaluate_here(&EvalInput {
        source: src.to_string(),
        chunk_name: "packages/t/packagespec.lua".into(),
        project_root: fixture(),
        package_path: "flux/packages/t/".into(),
        project_id: "flux".into(),
        env: Env::Production,
    })
}

#[test]
fn evaluates_every_flux_ce_spec() {
    test_in_process();
    let project = Project::new(fixture());
    let root = load(&project.root_location(), Env::Production).unwrap();
    assert_eq!(root.name, "Flux");
    assert_eq!(root.version.to_string(), "0.9.0");
    assert_eq!(root.authors.len(), 5);
    assert_eq!(root.globals, vec!["Flux"]);
    let ids: Vec<_> = root.dependencies.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids.len(), 16);
    assert_eq!(ids[5], "lib/flux.lua");
    assert_eq!(root.dependencies[5].kind, DepKind::File);
    assert!(check(&root, &project.root_location()).iter().all(|d| d.severity != Severity::Error));

    let pkgs = project.packages().unwrap();
    assert_eq!(pkgs.len(), 16);
    for p in &pkgs {
        let loc = project.package_location(&p.id);
        let m = load(&loc, Env::Production).unwrap_or_else(|e| panic!("{}: {e}", p.id));
        assert_eq!(m.id, p.id);
        assert!(!m.name.is_empty(), "{}", p.id);
        let errors: Vec<_> = check(&m, &loc).into_iter().filter(|d| d.severity == Severity::Error).collect();
        assert!(errors.is_empty(), "{}: {errors:?}", p.id);
        match p.id.as_str() {
            "fontawesome" => {
                assert_eq!(
                    m.resources,
                    vec![
                        "resource/fonts/fa-brands-400.ttf",
                        "resource/fonts/fa-regular-400.ttf",
                        "resource/fonts/fa-solid-900.ttf"
                    ]
                );
                assert_eq!(m.cl_files, vec!["lib/cl_fontawesome.lua", "lib/cl_icons.lua"]);
                assert_eq!(m.license.as_deref(), Some("file"));
            }
            "sfs" => {
                assert_eq!(m.client_env, vec![("SFS_PATH".to_string(), "flux/packages/sfs/".to_string())]);
                assert_eq!(m.name, "Srlion's Fast Serializer");
                assert_eq!(m.authors, vec!["Srlion"]);
                assert_eq!(m.files, vec!["lib/install.lua"]);
            }
            "utf8" => {
                assert_eq!(m.client_env, vec![("UTF8_PATH".to_string(), "flux/packages/utf8/".to_string())]);
                assert_eq!(m.sv_files, vec!["lib/utf8.lua"]);
                assert_eq!(m.cl_files, vec!["lib/utf8.min.lua"]);
            }
            "cami" => {
                assert_eq!(m.version.to_string(), "20211019.0.0");
                assert_eq!(m.version_components, 1);
            }
            "bolt" => {
                let deps: Vec<_> = m.dependencies.iter().map(|d| d.id.as_str()).collect();
                assert_eq!(deps, vec!["flow", "active_record", "active_ui", "cami"]);
            }
            "cable" => assert_eq!(m.dependencies[0].id, "sfs"),
            "active_record" => assert!(!m.reload),
            "llm" | "yaml" => assert!(m.serverside),
            _ => assert!(m.reload),
        }
    }
}

#[test]
fn plural_names_map_like_flux() {
    let ev = eval_src(
        "Package:describe(function(s)\n  s.name = 'T'\n  s.version = '1.0.0'\n  s.authors = { 'a' }\n  s.dependencies = 1\n  s.globals = 'G'\n  s.address = 'x'\n  s.cl_files = { 'a.lua' }\nend)",
    )
    .unwrap();
    let keys: Vec<_> = ev.fields.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["addres", "author", "cl_file", "dependency", "global", "name", "version"]);
}

#[test]
fn depends_forms() {
    let ev = eval_src(
        "Package:describe(function(s)\n  s.version = '1.0.0'\n  s.depends 'a'\n  s.depends('b', '~> 7.0')\n  s:depends('c')\n  s:depends('d', '>= 1.0')\n  PACKAGE:depends('e')\n  s.depends 'lib/x.lua'\nend)",
    )
    .unwrap();
    let got: Vec<_> = ev.depends.iter().map(|(n, c)| (n.as_str(), c.as_deref())).collect();
    assert_eq!(
        got,
        vec![
            ("a", None),
            ("b", Some("~> 7.0")),
            ("c", None),
            ("d", Some(">= 1.0")),
            ("e", None),
            ("lib/x.lua", None)
        ]
    );
}

#[test]
fn glua_syntax_and_flags() {
    let ev = eval_src(
        "// a GLua comment\n/* and a block one */\nPackage:describe(function(s)\n  s.version = '1.0.0'\n  for i = 1, 3 do if i == 2 then continue end end\n  if IS_PRODUCTION && !IS_DEVELOPMENT && SERVER && !CLIENT then s.name = engine.ActiveGamemode() end\n  s.reload = tobool('0')\nend)",
    )
    .unwrap();
    assert_eq!(ev.fields.get("name"), Some(&LuaData::String("flux".into())));
    assert_eq!(ev.fields.get("reload"), Some(&LuaData::Bool(false)));
}

#[test]
fn unknown_global_names_global_and_line() {
    let e = eval_src("Package:describe(function(s)\n  s.version = '1.0.0'\n  s.name = Foo.bar\nend)").unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("unknown global 'Foo'"), "{msg}");
    assert!(msg.contains(":3:"), "{msg}");
}

#[test]
fn sandbox_has_no_escape_hatches() {
    for name in ["io", "os", "debug", "ffi", "jit", "loadfile", "dofile", "collectgarbage"] {
        let e = eval_src(&format!("local x = {name}")).unwrap_err();
        assert!(e.to_string().contains(&format!("unknown global '{name}'")), "{name}: {e}");
    }
    let e = eval_src("require('x')").unwrap_err();
    assert!(e.to_string().contains("require is not available"), "{e}");
    let ok = |src: &str| {
        let r = eval_src(&format!("{src}\nPackage:describe(function(s) s.version='1.0.0' end)"));
        assert!(r.is_ok(), "{src}: {r:?}");
    };
    ok("assert(string.dump == nil)");
    // Strings index the sandbox's string copy, so there is no dump there either.
    ok("assert((\"\").dump == nil and ('x').upper('a') == 'A')");
    // load takes source text only, and does not bring removed names back.
    ok("assert(load('\\27LJ\\2') == nil)");
    ok("assert(load('return 1 + 1')() == 2)");
    ok("load('return 1')(); assert(rawget(_G, 'package') == nil and rawget(_G, 'event') == nil)");
    for name in ["package", "load_nl", "event", "keyed", "raise", "add_path"] {
        let e = eval_src(&format!("local x = {name}")).unwrap_err();
        assert!(e.to_string().contains(&format!("unknown global '{name}'")), "{name}: {e}");
    }
    let e = eval_src("local s = string.rep('x', 2^29)").unwrap_err();
    assert!(e.to_string().contains("larger than 16 MiB"), "{e}");
}

#[test]
fn sandbox_names_are_allow_listed() {
    let names = sandbox_names().unwrap();
    let allowed = [
        "AddCSLuaFile", "CLIENT", "IS_DEVELOPMENT", "IS_PRODUCTION", "IS_TEST", "PACKAGE", "Package", "SERVER",
        "String", "_G", "_VERSION", "add_client_env", "assert", "bit", "coroutine", "engine", "error", "file",
        "getmetatable", "include", "ipairs", "isbool", "isfunction", "isnumber", "isstring", "istable", "load",
        "loadstring", "math", "next", "pairs", "pcall", "print", "rawequal", "rawget", "rawset", "require",
        "require_relative", "resource", "select", "setmetatable", "string", "table", "tobool", "tonumber",
        "tostring", "type", "typeof", "unpack", "xpcall",
    ];
    let unexpected: Vec<_> = names.iter().filter(|n| !allowed.contains(&n.as_str())).collect();
    assert!(unexpected.is_empty(), "names not on the allow-list: {unexpected:?}");
}

#[test]
fn shared_and_cyclic_tables_are_refused() {
    let e = eval_src("local t = {}\nt[1] = t\nPackage:describe(function(s) s.version='1.0.0'; s.files = t end)").unwrap_err();
    assert!(e.to_string().contains("appears twice"), "{e}");
    let e = eval_src("local t = {'a'}\nfor i = 1, 30 do t = {t, t} end\nPackage:describe(function(s) s.version='1.0.0'; s.files = t end)").unwrap_err();
    assert!(e.to_string().contains("appears twice") || e.to_string().contains("deep"), "{e}");
    let e = eval_src("local t = {}\nfor i = 1, 200000 do t[i] = i end\nPackage:describe(function(s) s.version='1.0.0'; s.files = t end)").unwrap_err();
    assert!(e.to_string().contains("100000"), "{e}");
}

#[test]
fn file_find_stays_inside_the_project() {
    let ev = eval_src(
        "Package:describe(function(s)\n  s.version = '1.0.0'\n  local a = file.Find('../*', 'GAME')\n  local b = file.Find('/etc/*', 'GAME')\n  local c, d = file.Find('packages/*', 'LUA')\n  s.globals = { tostring(#a), tostring(#b), tostring(#c), tostring(#d) }\n  local e = file.Find('flux/packages/sfs/packagespec.lua', 'LUA')\n  s.file = e\nend)",
    )
    .unwrap();
    assert_eq!(
        ev.fields.get("global"),
        Some(&LuaData::List(vec![
            LuaData::String("0".into()),
            LuaData::String("0".into()),
            LuaData::String("0".into()),
            LuaData::String("16".into())
        ]))
    );
    assert_eq!(ev.fields.get("file"), Some(&LuaData::List(vec![LuaData::String("packagespec.lua".into())])));
}

#[test]
fn instruction_budget_stops_loops() {
    let e = eval_src("while true do end").unwrap_err();
    assert!(e.to_string().contains("instruction budget"), "{e}");
    // Coroutines count, and a spent budget cannot be caught and outrun.
    let e = eval_src("coroutine.wrap(function() while true do end end)()").unwrap_err();
    assert!(e.to_string().contains("instruction budget"), "{e}");
    let e = eval_src("while true do pcall(function() while true do end end) end").unwrap_err();
    assert!(e.to_string().contains("instruction budget"), "{e}");
}

#[test]
fn print_is_recorded() {
    let ev = eval_src("print('hi', 1)\nPackage:describe(function(s) s.version='1.0.0' end)").unwrap();
    assert_eq!(ev.output, vec!["hi\t1"]);
}

#[test]
fn env_flags() {
    let src = "Package:describe(function(s)\n s.version='1.0.0'\n if IS_DEVELOPMENT then s.depends 'dev_only' end\nend)";
    let mut input = EvalInput {
        source: src.into(),
        chunk_name: "t".into(),
        project_root: fixture(),
        package_path: "flux/".into(),
        project_id: "flux".into(),
        env: Env::Development,
    };
    assert_eq!(evaluate_here(&input).unwrap().depends.len(), 1);
    input.env = Env::Production;
    assert_eq!(evaluate_here(&input).unwrap().depends.len(), 0);
}

#[test]
fn manifest_errors() {
    let m = |src: &str| Manifest::from_evaluation("t", "t", eval_src(src).unwrap());
    assert!(matches!(m("local x = 1"), Err(SpecError::NotDescribed { .. })));
    assert!(m("Package:describe(function(s) s.name = 'x' end)").is_err());
    assert!(m("Package:describe(function(s) s.version = 'abc' end)").is_err());
    assert!(m("Package:describe(function(s) s.version = '1.0.0'; s.depends('a', 'bogus') end)").is_err());
    let ok = m("Package:describe(function(s) s.version = '1.0.0'; s.serverside = 'yes'; s.date = 'June' end)").unwrap();
    assert_eq!(ok.problems.len(), 2);
}

#[test]
fn licenses() {
    assert!(is_known_license("MIT"));
    assert!(is_known_license("MIT OR Apache-2.0"));
    assert!(is_known_license("(MIT AND BSD-3-Clause)"));
    assert!(is_known_license("GPL-2.0-only WITH Classpath-exception-2.0"));
    assert!(!is_known_license("Proprietary"));
    assert!(!is_known_license(""));
}
