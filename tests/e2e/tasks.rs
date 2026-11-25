//! Tasks: the taskspec.lua files of the `tasks` fixture listed and run, stand-alone and inside
//! a server tree.

use std::path::{Path, PathBuf};

use super::{Env, code, copy_dir, json, ok, read, text};

fn fixture(at: &Path) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tasks");
    copy_dir(&from, at);
    at.to_path_buf()
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn tasks_are_listed_and_run() {
    let env = Env::new();
    let p = fixture(&env.root.join("taskgm"));

    let o = ok(env.fpkg(&p, &["tasks"]));
    let listed = stdout(&o);
    let names: Vec<&str> = listed
        .lines()
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "args",
            "db:count",
            "db:setup",
            "environment",
            "exit",
            "fail",
            "files",
            "helpers:greet",
            "host",
            "wait"
        ]
    );
    assert!(
        listed.contains("db:count       Add a visit and count them\n"),
        "{listed}"
    );
    let all = stdout(&ok(env.fpkg(&p, &["tasks", "--all"])));
    assert!(all.lines().any(|l| l == "hidden"), "{all}");
    let filtered = stdout(&ok(env.fpkg(&p, &["tasks", "db:"])));
    assert_eq!(filtered.lines().count(), 2, "{filtered}");
    let j = json(&ok(env.fpkg(&p, &["--json", "tasks", "--all"])));
    let count = j["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "db:count")
        .unwrap();
    assert_eq!(
        count["prerequisites"],
        serde_json::json!(["environment", "db:setup"])
    );
    assert_eq!(count["package"], "taskgm");
    let greet = j["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "helpers:greet")
        .unwrap();
    assert_eq!(greet["package"], "helpers");
    assert_eq!(greet["prerequisites"], serde_json::json!(["environment"]));

    let o = ok(env.fpkg(&p, &["run", "db:count", "note=first"]));
    assert_eq!(stdout(&o), "environment 1\nvisits 1\n");
    let o = ok(env.fpkg(&p, &["db:count"]));
    assert_eq!(stdout(&o), "environment 1\nvisits 2\n");
    assert!(p.join(".fpkg/garrysmod/sv.db").is_file());

    let o = ok(env.fpkg(
        &p,
        &[
            "run",
            "args",
            "step=2",
            "--flag",
            "Migration",
            "VERSION=7",
            "-q",
        ],
    ));
    assert_eq!(
        stdout(&o),
        "name args\noptions STEP=2 VERSION=7\nwords --flag Migration -q\n"
    );
    let o = ok(env.fpkg(&p, &["args", "x", "Key=v"]));
    assert_eq!(stdout(&o), "name args\noptions KEY=v\nwords x\n");
    for (form, prefix) in [(&["run", "args"][..], "run"), (&["args"][..], "shorthand")] {
        let mut a = form.to_vec();
        a.extend(["-q", "--env", "test", "--help", "-v", "VERSION=1"]);
        let o = ok(env.fpkg(&p, &a));
        assert_eq!(
            stdout(&o),
            "name args\noptions VERSION=1\nwords -q --env test --help -v\n",
            "{prefix}: {}",
            text(&o)
        );
    }
    let o = ok(env.fpkg(&p, &["--env", "test", "run", "host"]));
    assert!(stdout(&o).starts_with("env test\n"), "{}", text(&o));

    let o = ok(env.fpkg(&p, &["helpers:greet"]));
    assert_eq!(stdout(&o), "environment 1\nhello from helpers\n");

    let o = ok(env.fpkg(&p, &["files"]));
    assert_eq!(stdout(&o), "read data file, return 1\nfound out.lua\n");
    assert_eq!(
        read(&p.join(".fpkg/garrysmod/data/notes/visit.txt")),
        "data file"
    );
    assert_eq!(read(&p.join("generated/out.lua")), "return 1");

    let o = ok(env.fpkg(&p, &["wait"]));
    assert_eq!(stdout(&o), "waited true\ndelivered after the task\n");

    let o = ok(env.fpkg(&p, &["host"]));
    assert_eq!(
        stdout(&o),
        "env nil\nserver false\nproject taskgm taskgm/\npackages helpers taskgm\ngamemode taskgm\n"
    );
    let o = ok(env.fpkg(&p, &["--env", "test", "host"]));
    assert!(stdout(&o).starts_with("env test\n"), "{}", text(&o));

    let o = code(env.fpkg(&p, &["run", "fail"]), 1);
    assert_eq!(stderr(&o), "error: it failed on purpose\n");
    let o = code(env.fpkg(&p, &["run", "install"]), 1);
    assert_eq!(
        stderr(&o),
        "error: gamemodes/taskgm/taskspec.lua:97: a task named like a command failed\nrun fpkg -v run install for the Lua stack\n"
    );
    let o = code(env.fpkg(&p, &["run", "exit", "x"]), 3);
    assert!(stderr(&o).is_empty(), "{}", text(&o));
    let o = code(env.fpkg(&p, &["-v", "fail"]), 1);
    assert!(
        stderr(&o).contains("deeper - gamemodes/taskgm/taskspec.lua:"),
        "{}",
        text(&o)
    );
    assert!(!stderr(&o).contains("for the Lua stack"), "{}", text(&o));
    code(env.fpkg(&p, &["exit"]), 3);
    let o = code(env.fpkg(&p, &["late"]), 1);
    let e = stderr(&o);
    assert!(e.contains("[ERROR] gamemodes/taskgm/taskspec.lua:"), "{e}");
    assert!(e.contains("late failure"), "{e}");
    assert!(
        e.ends_with("error: the task finished, but 1 Lua error was reported\n"),
        "{e}"
    );

    let o = code(env.fpkg(&p, &["run", "nope"]), 2);
    assert!(stderr(&o).contains("unknown task 'nope'"), "{}", text(&o));
    let o = code(env.fpkg(&p, &["run", "db:cont"]), 2);
    assert!(
        stderr(&o).contains("did you mean 'db:count'?"),
        "{}",
        text(&o)
    );
    let o = code(env.fpkg(&p, &["db:cont"]), 2);
    let e = stderr(&o);
    assert!(e.contains("unrecognized subcommand 'db:cont'"), "{e}");
    assert!(e.contains("a similar subcommand exists: 'db:count'"), "{e}");
    let o = code(env.fpkg(&p, &["instal"]), 2);
    let e = stderr(&o);
    assert!(e.contains("unrecognized subcommand 'instal'"), "{e}");
    assert!(
        e.contains("similar subcommand") && e.contains("'install'"),
        "{e}"
    );
    let o = code(env.fpkg(&p, &["ls"]), 2);
    assert!(
        stderr(&o).contains("a similar subcommand exists: 'list'"),
        "{}",
        text(&o)
    );
    let o = code(env.fpkg_in(&env.root, &["db:count"]), 2);
    let e = stderr(&o);
    assert!(e.contains("unrecognized subcommand 'db:count'"), "{e}");
    assert!(
        e.ends_with(
            "note: tasks need a project, and there is none here (fpkg --project <dir> names one)\n"
        ),
        "{e}"
    );
    let o = code(env.fpkg_in(&env.root, &["run", "db:count"]), 2);
    assert!(
        stderr(&o).contains("not inside an fpkg project"),
        "{}",
        text(&o)
    );

    let list = stdout(&ok(env.fpkg(&p, &["pack", "helpers", "--list"])));
    assert!(list.contains("taskspec.lua"), "{list}");
    ok(env.fpkg(&p, &["check"]));
    std::fs::write(
        p.join("packages/helpers/taskspec.lua"),
        "task('x', function()\n",
    )
    .unwrap();
    let o = env.fpkg(&p, &["check"]);
    assert!(!o.status.success());
    assert!(
        text(&o).contains("taskspec.lua does not compile: packages/helpers/taskspec.lua:2:"),
        "{}",
        text(&o)
    );
    let o = code(env.fpkg(&p, &["tasks"]), 1);
    assert!(
        stderr(&o).contains("gamemodes/taskgm/packages/helpers/taskspec.lua:2:"),
        "{}",
        text(&o)
    );
}

#[test]
fn a_mistyped_command_costs_nothing_visible() {
    let env = Env::new();
    let p = fixture(&env.root.join("taskgm"));
    std::fs::write(
        p.join("packages/helpers/taskspec.lua"),
        "print('loading helpers')\nerror('load-time boom')\n",
    )
    .unwrap();
    let spec = p.join("packagespec.lua");
    let text_of_spec = read(&spec).replace(
        "s.depends 'helpers'",
        "s.depends 'helpers'\n  s.depends 'ghost'",
    );
    std::fs::write(&spec, text_of_spec).unwrap();
    let o = code(env.fpkg(&p, &["instal"]), 2);
    let e = stderr(&o);
    assert!(stdout(&o).is_empty(), "{}", text(&o));
    assert!(
        e.starts_with("error: unrecognized subcommand 'instal'"),
        "{e}"
    );
    assert!(e.contains("'install'"), "{e}");
    assert!(
        !e.contains("warning") && !e.contains("loading helpers"),
        "{e}"
    );
    assert_eq!(
        e.lines().last().unwrap(),
        "note: the project's task files could not be loaded: gamemodes/taskgm/packages/helpers/taskspec.lua:2: load-time boom; see fpkg tasks"
    );
    assert!(!p.join(".fpkg").exists());
    std::fs::write(
        p.join("packages/helpers/taskspec.lua"),
        "print('loading helpers')\n",
    )
    .unwrap();
    let o = code(env.fpkg(&p, &["db:cont"]), 2);
    let e = stderr(&o);
    assert!(e.contains("a similar subcommand exists: 'db:count'"), "{e}");
    assert!(
        !e.contains("warning") && !e.contains("loading helpers") && !e.contains("note:"),
        "{e}"
    );
    assert!(stdout(&o).is_empty(), "{}", text(&o));
    let o = ok(env.fpkg(&p, &["args", "w"]));
    assert_eq!(
        stdout(&o),
        "loading helpers\nname args\noptions \nwords w\n"
    );
    assert!(
        stderr(&o).contains("warning: ghost, a dependency of taskgm"),
        "{}",
        text(&o)
    );
    ok(env.fpkg(&p, &["tasks"]));
    ok(env.fpkg(&p, &["host"]));
    assert!(!p.join(".fpkg").exists());
    ok(env.fpkg(&p, &["files"]));
    assert_eq!(read(&p.join(".fpkg/garrysmod/.gitignore")), "*\n");
}

#[test]
fn check_loads_task_files_on_a_large_stack() {
    let env = Env::new();
    let p = fixture(&env.root.join("taskgm"));
    std::fs::write(
        p.join("taskspec.lua"),
        "local function deeper(s) return (s:gsub('.', deeper)) end\ndeeper('x')\n",
    )
    .unwrap();
    let o = code(env.fpkg(&p, &["check"]), 1);
    assert!(
        stdout(&o).contains("its task files do not load") && stdout(&o).contains("stack overflow"),
        "{}",
        text(&o)
    );
    let o = code(env.fpkg(&p, &["tasks"]), 1);
    assert!(stderr(&o).contains("stack overflow"), "{}", text(&o));
}

#[test]
fn the_listing_is_alone_on_stdout() {
    let env = Env::new();
    let p = fixture(&env.root.join("taskgm"));
    let helpers = p.join("packages/helpers/taskspec.lua");
    let with_print = format!("print('loading tasks')\n{}", read(&helpers));
    std::fs::write(&helpers, with_print).unwrap();
    let o = ok(env.fpkg(&p, &["--json", "tasks", "--all"]));
    let j = json(&o);
    let late = j["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "late")
        .unwrap();
    assert_eq!(late["prerequisites"], serde_json::json!([]));
    assert!(stderr(&o).contains("loading tasks"), "{}", text(&o));
    let o = ok(env.fpkg(&p, &["tasks"]));
    assert!(!stdout(&o).contains("loading tasks"), "{}", text(&o));
    assert!(stdout(&o).starts_with("args "), "{}", text(&o));
    let o = ok(env.fpkg(&p, &["tasks", "db:count"]));
    assert_eq!(
        stdout(&o),
        "db:count  Add a visit and count them\n          needs: environment, db:setup\n"
    );
    let o = ok(env.fpkg(&p, &["tasks", "late"]));
    assert_eq!(stdout(&o), "late\n      needs no other task\n");
    let o = ok(env.fpkg(&p, &["tasks", "hid"]));
    assert!(stdout(&o).is_empty());
    assert!(
        stderr(&o).contains(
            "no tasks with a description match; 1 without one is listed by fpkg tasks --all"
        ),
        "{}",
        text(&o)
    );
    let o = ok(env.fpkg(&p, &["tasks", "nothing-like-this"]));
    assert!(stderr(&o).contains("no tasks match"), "{}", text(&o));
    std::fs::write(p.join("taskspec.lua"), "desc 'x'\ntask('x', { 'ghost' })\n").unwrap();
    let o = ok(env.fpkg(&p, &["tasks"]));
    assert!(
        stderr(&o).contains("warning: task 'x' needs 'ghost', which no task file defines"),
        "{}",
        text(&o)
    );
    let o = ok(env.fpkg(&p, &["check"]));
    assert!(
        stdout(&o).contains("warning: task 'x' needs 'ghost', which no task file defines"),
        "{}",
        text(&o)
    );
    std::fs::write(p.join("taskspec.lua"), "error('broken at load', 0)\n").unwrap();
    let o = env.fpkg(&p, &["check"]);
    assert!(!o.status.success());
    assert!(
        stdout(&o)
            .contains("its task files do not load: gamemodes/taskgm/taskspec.lua: broken at load"),
        "{}",
        text(&o)
    );
}

#[test]
fn inside_a_server_tree_the_server_s_folders_are_used() {
    let env = Env::new();
    let server = env.root.join("srv");
    let p = fixture(&server.join("garrysmod/gamemodes/taskgm"));
    std::fs::create_dir_all(server.join("garrysmod/data")).unwrap();
    let o = ok(env.fpkg_in(&p, &["db:count"]));
    assert_eq!(stdout(&o), "environment 1\nvisits 1\n");
    assert!(server.join("garrysmod/sv.db").is_file());
    assert!(!p.join(".fpkg").exists());
    ok(env.fpkg_in(&p, &["files"]));
    assert_eq!(
        read(&server.join("garrysmod/data/notes/visit.txt")),
        "data file"
    );
    assert_eq!(read(&p.join("generated/out.lua")), "return 1");
    let o = ok(env.fpkg_in(&p, &["host"]));
    assert!(stdout(&o).contains("server true\n"), "{}", text(&o));
}

#[test]
fn help_names_the_task_commands() {
    let env = Env::new();
    let o = ok(env.fpkg_in(&env.root, &["--help"]));
    let help = stdout(&o);
    assert!(help.lines().any(|l| l.starts_with("  tasks ")), "{help}");
    assert!(help.lines().any(|l| l.starts_with("  run ")), "{help}");
    let o = ok(env.fpkg_in(&env.root, &["run", "--help"]));
    let run = stdout(&o);
    assert!(run.contains("Usage: fpkg run [OPTIONS] <TASK>..."), "{run}");
    assert!(run.contains("`fpkg <task>`"), "{run}");
    assert!(
        run.contains("fpkg's own flags go before the task's name"),
        "{run}"
    );
    let o = ok(env.fpkg_in(&env.root, &["tasks", "--help"]));
    assert!(stdout(&o).contains("--all"), "{}", text(&o));
}

/// A PostgreSQL server that lets anyone in and answers every query with no rows, enough for the
/// pg module to deliver the "success" of a query.
fn quiet_postgres() -> u16 {
    use std::io::{Read, Write};

    fn put(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn serve(mut s: std::net::TcpStream) -> Option<()> {
        let mut len = [0u8; 4];
        s.read_exact(&mut len).ok()?;
        let mut startup = vec![0u8; (u32::from_be_bytes(len) as usize).checked_sub(4)?];
        s.read_exact(&mut startup).ok()?;
        let mut out = put(b'R', &0u32.to_be_bytes());
        out.extend(put(b'Z', b"I"));
        s.write_all(&out).ok()?;
        loop {
            let mut head = [0u8; 5];
            s.read_exact(&mut head).ok()?;
            let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
            let mut body = vec![0u8; len.checked_sub(4)?];
            s.read_exact(&mut body).ok()?;
            if head[0] == b'Q' {
                let mut out = put(b'C', b"SELECT 0\0");
                out.extend(put(b'Z', b"I"));
                s.write_all(&out).ok()?;
            }
        }
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || serve(s));
        }
    });
    port
}

#[test]
fn an_error_in_a_listener_of_the_pg_module_fails_the_task() {
    let env = Env::new();
    let p = env.root.join("pgtask");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("pgtask.txt"),
        "\"pgtask\"\n{\n\t\"base\"\t\t\"base\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        p.join("packagespec.lua"),
        "Package:describe(function(s)\n  s.name = 'PG task'\n  s.version = '0.1.0'\n  s.date = '2026-11-25'\n  s.summary = 'A listener that fails.'\n  s.license = 'MIT'\nend)\n",
    )
    .unwrap();
    std::fs::write(
        p.join("taskspec.lua"),
        format!(
            "task('listener', function()\n  require('pg')\n  local db = pg.new_connection()\n  assert(db:connect('127.0.0.1', 'u', 'p', 'd', {}, 'sslmode=disable'))\n  local q = db:query('select 1')\n  q:on('success', function() error('listener failure') end)\n  q:on('success', function() print('second listener') end)\n  q:run()\nend)\n",
            quiet_postgres()
        ),
    )
    .unwrap();
    let o = code(env.fpkg(&p, &["listener"]), 1);
    assert_eq!(stdout(&o), "second listener\n", "{}", text(&o));
    let e = stderr(&o);
    assert!(
        e.starts_with("[ERROR] gamemodes/pgtask/taskspec.lua:6: listener failure\n"),
        "{e}"
    );
    assert!(
        e.ends_with("error: the task finished, but 1 Lua error was reported\n"),
        "{e}"
    );
}
