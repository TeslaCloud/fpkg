//! Tests of task files.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use super::*;
use crate::install;
use crate::project::Project;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn spec(id: &str, deps: &[&str]) -> String {
    let deps: String = deps
        .iter()
        .map(|d| format!("  s.depends '{d}'\n"))
        .collect();
    format!(
        "Package:describe(function(s)\n  s.name = '{id}'\n  s.id = '{id}'\n  s.version = '1.2.3'\n{deps}end)\n"
    )
}

/// A gamemode `gm` that depends on `base` and `extra`; `base` depends on `extra` and the
/// missing `ghost`.
fn project() -> tempfile::TempDir {
    crate::spec::test_in_process();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "gm.txt", "\"gm\" { \"title\" \"GM\" }");
    write(root, "packagespec.lua", &spec("gm", &["base", "extra"]));
    write(
        root,
        "packages/base/packagespec.lua",
        &spec("base", &["extra", "ghost"]),
    );
    write(root, "packages/extra/packagespec.lua", &spec("extra", &[]));
    write(
        root,
        "packages/unused/packagespec.lua",
        &spec("unused", &[]),
    );
    write(
        root,
        "packages/unused/taskspec.lua",
        "task('unused', function() end)",
    );
    dir
}

struct Loaded {
    set: TaskSet,
    console: Rc<RefCell<fpkg_glua::Captured>>,
}

fn load(root: &Path) -> Result<Loaded, TaskError> {
    let project = Project::discover(root).unwrap();
    let loaded = install::load_used(&project, Env::Production, None).unwrap();
    let order = install::load_order(&loaded, Env::Production).unwrap();
    let (output, console) = Output::capture();
    let set = TaskSet::load(Context {
        project: &project,
        packages: &order.packages,
        env: Some(Env::Development),
        tree: tree_for(&project, &order.packages),
        output,
        color: false,
    })?;
    Ok(Loaded { set, console })
}

impl Loaded {
    fn eval<T: nl::mlua::FromLua>(&self, code: &str) -> T {
        self.set
            .runtime()
            .lua()
            .load(format!("return {code}"))
            .eval()
            .unwrap()
    }

    fn out(&self) -> String {
        self.console.borrow().out_text()
    }
}

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|s| s.to_string()).collect()
}

fn order_of(root: &Path) -> Result<install::LoadOrder, install::InstallError> {
    let project = Project::discover(root).unwrap();
    let loaded = install::load_used(&project, Env::Production, None).unwrap();
    install::load_order(&loaded, Env::Production)
}

fn ids(order: &install::LoadOrder) -> Vec<&str> {
    order.packages.iter().map(|p| p.id.as_str()).collect()
}

#[test]
fn packages_load_in_flux_order() {
    let dir = project();
    let order = order_of(dir.path()).unwrap();
    assert_eq!(ids(&order), ["extra", "base", "gm"]);
    assert_eq!(order.missing, [("ghost".to_string(), "base".to_string())]);
    let paths: Vec<&str> = order.packages.iter().map(|p| p.lua_path.as_str()).collect();
    assert_eq!(paths, ["gm/packages/extra/", "gm/packages/base/", "gm/"]);
    write(
        dir.path(),
        "packagespec.lua",
        &spec("gm", &["zeta", "base", "alpha"]),
    );
    write(
        dir.path(),
        "packages/zeta/packagespec.lua",
        &spec("zeta", &["alpha"]),
    );
    write(
        dir.path(),
        "packages/alpha/packagespec.lua",
        &spec("alpha", &[]),
    );
    let order = order_of(dir.path()).unwrap();
    assert_eq!(ids(&order), ["alpha", "zeta", "extra", "base", "gm"]);
}

#[test]
fn a_package_cycle_is_the_error_install_gives() {
    let dir = project();
    write(dir.path(), "packagespec.lua", &spec("gm", &["conf"]));
    write(
        dir.path(),
        "packages/conf/packagespec.lua",
        &spec("conf", &["reg"]),
    );
    write(
        dir.path(),
        "packages/reg/packagespec.lua",
        &spec("reg", &["conf"]),
    );
    let e = order_of(dir.path()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "dependency cycle: conf -> reg -> conf; Flux's Package:include would recurse forever"
    );
    write(
        dir.path(),
        "packages/reg/packagespec.lua",
        &spec("reg", &["reg"]),
    );
    assert!(
        order_of(dir.path())
            .unwrap_err()
            .to_string()
            .contains("reg -> reg")
    );
}

#[test]
fn the_dependencies_of_a_clientside_package_are_not_followed() {
    let dir = project();
    write(dir.path(), "packagespec.lua", &spec("gm", &["ui"]));
    write(
        dir.path(),
        "packages/ui/packagespec.lua",
        "Package:describe(function(s)\n  s.name = 'ui'\n  s.id = 'ui'\n  s.version = '1.0.0'\n  s.clientside = true\n  s.depends 'extra'\nend)\n",
    );
    let order = order_of(dir.path()).unwrap();
    assert_eq!(ids(&order), ["ui", "gm"]);
    assert!(order.missing.is_empty());
}

#[test]
fn a_nested_package_is_found_once_its_outer_package_is_reached() {
    let dir = project();
    write(
        dir.path(),
        "packages/outer/packagespec.lua",
        &spec("outer", &["inner"]),
    );
    write(
        dir.path(),
        "packages/outer/packages/inner/packagespec.lua",
        &spec("inner", &[]),
    );
    write(dir.path(), "packagespec.lua", &spec("gm", &["outer"]));
    let order = order_of(dir.path()).unwrap();
    assert_eq!(ids(&order), ["inner", "outer", "gm"]);
    assert_eq!(
        order.packages[0].lua_path,
        "gm/packages/outer/packages/inner/"
    );
    write(
        dir.path(),
        "packagespec.lua",
        &spec("gm", &["inner", "outer"]),
    );
    let order = order_of(dir.path()).unwrap();
    assert_eq!(
        order.too_early,
        [("inner".to_string(), "gm".to_string(), "outer".to_string())]
    );
    assert_eq!(ids(&order), ["inner", "outer", "gm"]);
}

/// Marks `packages/<id>` as installed from a registry.
fn mark_installed(root: &Path, id: &str) {
    write(
        root,
        &format!("packages/{id}/.fpkg.toml"),
        &format!(
            "id = '{id}'\nversion = '1.2.3'\nregistry = 'file:///r/'\nsha256 = '00'\ntree_sha256 = '00'\ninstalled_at = '2026-11-25T00:00:00Z'\n"
        ),
    );
}

#[test]
fn a_broken_installed_spec_matters_only_when_it_is_used() {
    let dir = project();
    write(
        dir.path(),
        "packages/broken/packagespec.lua",
        "error('broken spec')",
    );
    mark_installed(dir.path(), "broken");
    mark_installed(dir.path(), "extra");
    let order = order_of(dir.path()).unwrap();
    assert_eq!(ids(&order), ["extra", "base", "gm"]);
    write(
        dir.path(),
        "packagespec.lua",
        &spec("gm", &["base", "broken"]),
    );
    assert!(
        order_of(dir.path())
            .unwrap_err()
            .to_string()
            .contains("broken spec")
    );
}

#[test]
fn linked_nested_installed_and_path_packages_have_their_tasks() {
    let dir = project();
    let root = dir.path();
    let checkout = tempfile::tempdir().unwrap();
    write(
        checkout.path(),
        "packagespec.lua",
        &spec("linked", &["deep"]),
    );
    write(
        checkout.path(),
        "packages/deep/packagespec.lua",
        &spec("deep", &[]),
    );
    write(
        checkout.path(),
        "packages/deep/taskspec.lua",
        "local spec = TASKSPEC task('deep', function() print('deep', spec.path, include(spec.path..'x.lua')) end)",
    );
    write(checkout.path(), "packages/deep/x.lua", "return 'from deep'");
    #[cfg(unix)]
    std::os::unix::fs::symlink(checkout.path(), root.join("packages/linked")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(checkout.path(), root.join("packages/linked")).unwrap();
    write(
        root,
        "packages/extra/taskspec.lua",
        "local spec = TASKSPEC task('installed', function() print('installed', spec.path, include(spec.path..'x.lua')) end)",
    );
    write(root, "packages/extra/x.lua", "return 'from installed'");
    mark_installed(root, "extra");
    let shared = tempfile::tempdir().unwrap();
    let shared_pkg = shared.path().join("shared");
    write(&shared_pkg, "packagespec.lua", &spec("shared", &[]));
    write(
        &shared_pkg,
        "taskspec.lua",
        "local spec = TASKSPEC task('path', function() print('path', include(spec.path..'x.lua')) end)",
    );
    write(&shared_pkg, "x.lua", "return 'from the path package'");
    let rel = pathdiff(&shared_pkg, root);
    write(
        root,
        "packagespec.lua",
        &format!(
            "Package:describe(function(s)\n  s.name = 'gm'\n  s.id = 'gm'\n  s.version = '1.2.3'\n  s.depends 'base'\n  s.depends 'linked'\n  s.depends '{rel}'\nend)\n"
        ),
    );
    let l = load(root).unwrap();
    for t in ["deep", "installed", "path"] {
        l.set.run(t, &[]).unwrap();
    }
    assert_eq!(
        l.out(),
        "deep\tgm/packages/linked/packages/deep/\tfrom deep\ninstalled\tgm/packages/extra/\tfrom installed\npath\tfrom the path package\n"
    );
    assert!(!root.join(".fpkg").exists());
}

/// `to` relative to `from`, with forward slashes (both are temporary directories).
fn pathdiff(to: &Path, from: &Path) -> String {
    let from = from.canonicalize().unwrap();
    let to = to.canonicalize().unwrap();
    let common = from
        .components()
        .zip(to.components())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from.components().count() - common;
    let mut parts: Vec<String> = vec!["..".into(); ups];
    parts.extend(
        to.components()
            .skip(common)
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

#[test]
fn loading_and_reading_create_no_folders() {
    let dir = project();
    let l = load(dir.path()).unwrap();
    assert!(l.set.tasks().is_empty());
    assert!(!dir.path().join(".fpkg").exists());
    write(
        dir.path(),
        "taskspec.lua",
        "local here = file.Exists('x.txt', 'DATA') task('read', function() file.Read('x.txt') file.Find('*', 'DATA') end) task('write', function() file.Write('x.txt', 'x') end)",
    );
    let l = load(dir.path()).unwrap();
    l.set.run("read", &[]).unwrap();
    assert!(!dir.path().join(".fpkg").exists());
    l.set.run("write", &[]).unwrap();
    assert!(dir.path().join(".fpkg/garrysmod/data/x.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".fpkg/garrysmod/.gitignore")).unwrap(),
        "*\n"
    );
}

#[test]
fn namespaces_descriptions_and_files_in_order() {
    let dir = project();
    write(
        dir.path(),
        "packages/extra/taskspec.lua",
        "namespace('db', function()\n  desc 'Migrate'\n  task('migrate', { 'environment' }, function() end)\n  namespace('migrate', function()\n    task('status', { ':migrate' })\n  end)\nend)\ndesc 'Boot'\ntask('environment', function() end)\n",
    );
    write(
        dir.path(),
        "taskspec.lua",
        "task('db:migrate', { 'gm:prepare' }, function() end)\ntask('gm:prepare')\n",
    );
    let l = load(dir.path()).unwrap();
    let names: Vec<&str> = l.set.tasks().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "db:migrate",
            "db:migrate:status",
            "environment",
            "gm:prepare"
        ]
    );
    let migrate = l.set.get("db:migrate").unwrap();
    assert_eq!(migrate.description.as_deref(), Some("Migrate"));
    assert_eq!(migrate.prerequisites, ["environment", "gm:prepare"]);
    assert_eq!(migrate.package, "extra");
    assert_eq!(migrate.actions, 2);
    let status = l.set.get("db:migrate:status").unwrap();
    assert_eq!(status.prerequisites, ["db:migrate:migrate"]);
    assert_eq!(status.description, None);
    assert!(l.set.get("unused").is_none());
}

#[test]
fn prerequisites_run_first_and_every_task_once() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "log = {}\nlocal function step(name) return function(args) log[#log + 1] = name..'('..args.name..')' end end\ntask('a', step('a'))\ntask('b', { 'a' }, step('b'))\ntask('c', { 'a', 'b' }, step('c'))\ntask('d', { 'c', 'b' }, step('d1'))\ntask('d', step('d2'))\n",
    );
    let l = load(dir.path()).unwrap();
    l.set.run("d", &[]).unwrap();
    assert_eq!(
        l.eval::<String>("table.concat(log, ' ')"),
        "a(a) b(b) c(c) d1(d) d2(d)"
    );
    l.set.run("c", &[]).unwrap();
    assert_eq!(l.eval::<i64>("#log"), 5);
}

#[test]
fn cycles_are_named() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('a', { 'b' }, function() end)\ntask('b', { 'c' })\ntask('c', { 'a' })\ntask('d', { 'missing' })\n",
    );
    let l = load(dir.path()).unwrap();
    let e = l.set.run("a", &[]).unwrap_err();
    assert_eq!(e.to_string(), "a cycle of prerequisites: a -> b -> c -> a");
    assert!(matches!(e, TaskError::Failed { .. }));
    let e = l.set.run("d", &[]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "unknown task 'missing', a prerequisite of 'd'"
    );
    assert!(matches!(l.set.run("nope", &[]), Err(TaskError::Unknown(n)) if n == "nope"));
}

#[test]
fn arguments_options_and_words() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('pre', function(args) pre = args.options.STEP end)\ntask('t', { 'pre' }, function(args) got = args end)\n",
    );
    let l = load(dir.path()).unwrap();
    l.set
        .run(
            "t",
            &words(&[
                "step=2",
                "migration",
                "Name",
                "col:string",
                "VERSION=",
                "=x",
                "a-b=c",
            ]),
        )
        .unwrap();
    assert_eq!(l.eval::<String>("pre"), "2");
    assert_eq!(l.eval::<String>("got.options.STEP"), "2");
    assert_eq!(l.eval::<String>("got.options.VERSION"), "");
    assert_eq!(
        l.eval::<String>("table.concat(got.words, ' ')"),
        "migration Name col:string =x a-b=c"
    );
    assert_eq!(l.eval::<String>("got.name"), "t");
    let (options, rest) = parse_words(&words(&["Key_1=v=w", "plain"]));
    assert_eq!(options.get("KEY_1").map(String::as_str), Some("v=w"));
    assert_eq!(rest, ["plain"]);
}

#[test]
fn tasks_invoke_exists_taskspec_and_fpkg() {
    let dir = project();
    write(
        dir.path(),
        "packages/base/lib/helper.lua",
        "return 'helped'",
    );
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "local spec = TASKSPEC\nseen_id, seen_path = spec.id, spec.path\ntask('base:help', function() helped = include(spec.path..'lib/helper.lua') end)\n",
    );
    write(
        dir.path(),
        "taskspec.lua",
        "task('outer', function(args) inner_exists = Tasks.exists('base:help') missing = Tasks.exists('nope') Tasks.invoke('base:help') Tasks.invoke('base:help', { options = { X = '1' } }) end)\n",
    );
    let l = load(dir.path()).unwrap();
    assert_eq!(
        l.eval::<String>("seen_id..' '..seen_path"),
        "base gm/packages/base/"
    );
    assert!(l.eval::<bool>("TASKSPEC == nil"));
    l.set.run("outer", &[]).unwrap();
    assert!(l.eval::<bool>("inner_exists and not missing"));
    assert_eq!(l.eval::<String>("helped"), "helped");
    assert_eq!(l.eval::<String>("fpkg.version"), crate::FPKG_VERSION);
    assert_eq!(
        l.eval::<String>("fpkg.project.id..' '..fpkg.project.path"),
        "gm gm/"
    );
    assert!(l.eval::<bool>("fpkg.project.gamemode == true and fpkg.server == false"));
    assert_eq!(l.eval::<String>("fpkg.env"), "development");
    assert_eq!(
        l.eval::<String>(
            "fpkg.packages[1].id..' '..fpkg.packages[2].path..' '..fpkg.packages[3].version"
        ),
        "extra gm/packages/base/ 1.2.3"
    );
    assert!(l.eval::<bool>("isfunction(fpkg.wait_until) and isfunction(fpkg.exit)"));
}

#[test]
fn wait_until_turns_the_loop() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('wait', function()\n  local done = false\n  timer.Simple(0.02, function() done = true end)\n  ok = fpkg.wait_until(function() return done end, 5)\n  late = fpkg.wait_until(function() return false end, 0.02)\n  print('waited', ok, late)\nend)\ntask('later', function() timer.Simple(0, function() print('delivered') end) end)\n",
    );
    let l = load(dir.path()).unwrap();
    l.set.run("wait", &[]).unwrap();
    assert!(l.eval::<bool>("ok == true and late == false"));
    l.set.run("later", &[]).unwrap();
    assert_eq!(l.out(), "waited\ttrue\tfalse\ndelivered\n");
}

#[test]
fn a_failure_caught_by_pcall_leaves_no_false_cycle() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('boom', function() error('boom!') end)\ntask('twice', function() first = select(2, pcall(Tasks.invoke, 'boom')) second = select(2, pcall(Tasks.invoke, 'boom')) end)\ntask('self', function() Tasks.invoke('self') end)\npcall(namespace, 'n', function() error('in namespace') end)\ntask('after')\n",
    );
    let l = load(dir.path()).unwrap();
    l.set.run("twice", &[]).unwrap();
    assert_eq!(
        l.eval::<String>("first..' | '..second"),
        "gamemodes/gm/taskspec.lua:1: boom! | gamemodes/gm/taskspec.lua:1: boom!"
    );
    assert_eq!(
        l.set.run("self", &[]).unwrap_err().to_string(),
        "'self' is still running, and invoking it again makes a cycle: self -> self"
    );
    assert!(l.set.get("after").is_some() && l.set.get("n:after").is_none());
}

#[test]
fn tasks_invoked_from_coroutines() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('coself', function() coroutine.wrap(function() Tasks.invoke('coself') end)() end)\n\
         namespace('ns', function()\n  task('x', function() x_runs = (x_runs or 0) + 1 end)\n  task('co', function() coroutine.wrap(function() Tasks.invoke(':x') rel = Tasks.exists(':x') end)() end)\nend)\n\
         task('paused', function() paused_runs = (paused_runs or 0) + 1 if coroutine.running() then coroutine.yield() end end)\n\
         task('abandon', function() local co = coroutine.create(function() Tasks.invoke('paused') end) coroutine.resume(co) Tasks.invoke('paused') end)\n\
         task('fine', function() local co = coroutine.create(function() Tasks.invoke('ns:x') end) assert(coroutine.resume(co)) end)\n",
    );
    let l = load(dir.path()).unwrap();
    assert!(l.set.run("coself", &[]).unwrap_err().to_string().ends_with(
        "'coself' is still running, and invoking it again makes a cycle: coself -> coself"
    ));
    l.set.run("ns:co", &[]).unwrap();
    assert!(l.eval::<bool>("x_runs == 1 and rel"));
    l.set.run("abandon", &[]).unwrap();
    assert_eq!(l.eval::<i64>("paused_runs"), 2);
    l.set.run("fine", &[]).unwrap();
}

#[test]
fn names_descriptions_and_relative_invocations() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "desc 'kept' desc(nil) task('plain')\nnamespace('db', function()\n  task('load_config', function() loaded = (loaded or 0) + 1 end)\n  task('migrate', function() Tasks.invoke(':load_config') exists = Tasks.exists(':load_config') end)\nend)\ntask('top', function() top_exists = Tasks.exists(':plain') end)\n",
    );
    let l = load(dir.path()).unwrap();
    assert_eq!(l.set.get("plain").unwrap().description, None);
    l.set.run("db:migrate", &[]).unwrap();
    l.set.run("top", &[]).unwrap();
    assert!(l.eval::<bool>("loaded == 1 and exists and top_exists"));
    for bad in [
        "task('--help')",
        "task('-v')",
        "task('K=V')",
        "namespace('-n', function() end)",
        "task('x', { 'a=b' })",
        "desc({})",
    ] {
        write(dir.path(), "taskspec.lua", bad);
        let e = load(dir.path()).err().unwrap().to_string();
        assert!(e.starts_with("gamemodes/gm/taskspec.lua:1:"), "{bad}: {e}");
    }
}

#[test]
fn exit_takes_codes_from_0_to_255() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('t', function(args) local code = args.words[1] fpkg.exit(code == 'true' and true or tonumber(code) or code) end)",
    );
    let l = load(dir.path()).unwrap();
    for code in ["256", "-1", "1.5", "2147483648", "true", "abc"] {
        assert_eq!(
            l.set.run("t", &words(&[code])).unwrap_err().to_string(),
            "fpkg.exit: code must be 0-255",
            "{code}"
        );
    }
}

#[test]
fn reported_errors_fail_the_task_and_messages_do_not() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('timer', function() timer.Simple(0, function() error('late') end) end)\ntask('include', function() include('missing.lua') end)\ntask('message', function() ErrorNoHalt('just a message\\n') end)\ntask('caught', function() pcall(error, 'x') end)\n",
    );
    let l = load(dir.path()).unwrap();
    let e = l.set.run("timer", &[]).unwrap_err();
    assert!(matches!(e, TaskError::Reported(1)), "{e}");
    assert_eq!(
        e.to_string(),
        "the task finished, but 1 Lua error was reported"
    );
    assert!(matches!(
        l.set.run("include", &[]),
        Err(TaskError::Reported(1))
    ));
    l.set.run("message", &[]).unwrap();
    l.set.run("caught", &[]).unwrap();
    assert_eq!(
        TaskError::Reported(2).to_string(),
        "the task finished, but 2 Lua errors were reported"
    );
}

#[test]
fn wait_until_raises_again_what_the_predicate_raised() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('t', function() local ok, err = pcall(fpkg.wait_until, function() error({ code = 1 }) end, 1) got = type(err)..' '..tostring(err.code) local ok2, err2 = pcall(fpkg.wait_until, 'x') bad = err2 end)",
    );
    let l = load(dir.path()).unwrap();
    l.set.run("t", &[]).unwrap();
    assert_eq!(l.eval::<String>("got"), "table 1");
    assert_eq!(
        l.eval::<String>("bad"),
        "bad argument #1 to 'wait_until' (function expected, got string)"
    );
}

#[test]
fn prerequisites_that_name_no_task() {
    let dir = project();
    write(
        dir.path(),
        "taskspec.lua",
        "task('a', { 'b', 'ghost' })\ntask('b')\n",
    );
    let l = load(dir.path()).unwrap();
    let missing: Vec<(String, String)> = l
        .set
        .missing_prerequisites()
        .into_iter()
        .map(|(t, p)| (t.name.clone(), p.to_string()))
        .collect();
    assert_eq!(missing, [("a".to_string(), "ghost".to_string())]);
}

#[test]
fn errors_of_files_and_tasks() {
    let dir = project();
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "task('x', function()\n",
    );
    let e = load(dir.path()).err().unwrap();
    assert!(matches!(e, TaskError::Load { .. }));
    assert!(
        e.to_string()
            .starts_with("gamemodes/gm/packages/base/taskspec.lua:2:"),
        "{e}"
    );
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "error('broken file', 0)\n",
    );
    let e = load(dir.path()).err().unwrap();
    assert_eq!(
        e.to_string(),
        "gamemodes/gm/packages/base/taskspec.lua: broken file"
    );
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "task('bad name')\n",
    );
    let e = load(dir.path()).err().unwrap();
    assert!(
        e.to_string()
            .contains("a task must be a name without blanks"),
        "{e}"
    );
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "local function deep() error('it broke') end\ntask('fail', function() deep() end)\n",
    );
    let l = load(dir.path()).unwrap();
    let e = l.set.run("fail", &[]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "gamemodes/gm/packages/base/taskspec.lua:1: it broke"
    );
    assert!(
        e.traceback()
            .contains("deep - gamemodes/gm/packages/base/taskspec.lua:1"),
        "{}",
        e.traceback()
    );
    let base = dir.path().join("packages/base");
    assert!(check_task_file(&base, "packages/base/taskspec.lua").is_none());
    write(
        dir.path(),
        "packages/base/taskspec.lua",
        "task('x', function()\n",
    );
    let msg = check_task_file(&base, "packages/base/taskspec.lua").unwrap();
    assert!(
        msg.starts_with("taskspec.lua does not compile: packages/base/taskspec.lua:2:"),
        "{msg}"
    );
}
