//! Tests of the runtime through Lua.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use nl::mlua::Value;

use crate::{Captured, Config, Output, Runtime, Tree};

struct Rt {
    rt: Runtime,
    console: Rc<RefCell<Captured>>,
    dir: tempfile::TempDir,
}

impl Rt {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, code: &str) -> Vec<Value> {
        let f = self.rt.load(code.as_bytes(), "test.lua").unwrap();
        match self.rt.call(&f, ()) {
            Ok(v) => v.into_vec(),
            Err(e) => panic!(
                "{e}\n{}\nstderr: {}",
                e.traceback(),
                self.console.borrow().err_text()
            ),
        }
    }

    fn eval<T: nl::mlua::FromLua>(&self, code: &str) -> T {
        let f = self
            .rt
            .load(format!("return {code}").as_bytes(), "test.lua")
            .unwrap();
        match self.rt.call(&f, ()) {
            Ok(v) => self
                .rt
                .lua()
                .unpack(v.into_iter().next().unwrap_or(Value::Nil))
                .unwrap(),
            Err(e) => panic!("{code}: {e}\n{}", e.traceback()),
        }
    }

    fn fails(&self, code: &str) -> String {
        let f = self.rt.load(code.as_bytes(), "test.lua").unwrap();
        self.rt.call(&f, ()).unwrap_err().message
    }

    fn out(&self) -> String {
        self.console.borrow().out_text()
    }

    fn err(&self) -> String {
        self.console.borrow().err_text()
    }
}

fn runtime() -> Rt {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gm.txt"), "\"gm\" {}").unwrap();
    let (output, console) = Output::capture();
    let tree = Tree::standalone(dir.path(), "gm");
    let rt = Runtime::new(Config {
        tree,
        output,
        color: false,
    })
    .unwrap();
    Rt { rt, console, dir }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn the_environment_of_a_server() {
    let rt = runtime();
    assert!(rt.eval::<bool>("SERVER and not CLIENT and not MENU_DLL"));
    assert!(
        rt.eval::<bool>("loadstring == nil and io == nil and os.getenv == nil and dofile == nil")
    );
    assert!(rt.eval::<bool>(
        "debug.getmetatable ~= nil and jit.arch ~= nil and string.gfind == string.gmatch"
    ));
    assert_eq!(rt.eval::<String>("engine.ActiveGamemode()"), "gm");
    assert_eq!(rt.eval::<String>("type(NULL)"), "Entity");
    assert!(!rt.eval::<bool>("IsValid(NULL)"));
    assert!(!rt.eval::<bool>("IsValid(nil)"));
    assert_eq!(rt.eval::<i64>("TYPE_ENTITY"), 9);
    assert_eq!(rt.eval::<i64>("FCVAR_ARCHIVE"), 128);
    assert_eq!(
        rt.eval::<String>("FindMetaTable('Player').MetaName"),
        "Player"
    );
    assert!(rt.eval::<bool>("FindMetaTable('Nope') == nil"));
    assert!(rt.eval::<bool>("FindMetaTable('Color') == getmetatable(Color(1, 2, 3))"));
    assert!(rt.eval::<bool>("debug.getregistry().Color == FindMetaTable('Color')"));
    assert!(rt.eval::<bool>("('abc')[2] == 'b' and ('  x '):Trim() == 'x'"));
}

#[test]
fn printing() {
    let rt = runtime();
    rt.run(
        "print('a', 1, nil, true) Msg('b', 2) MsgN('c') MsgC(Color(255, 0, 0), 'red', ' ', 3, '\\n')",
    );
    assert_eq!(rt.out(), "a\t1\tnil\ttrue\nb2c\nred 3\n");
    rt.run("ErrorNoHalt('oops', 1, '\\n')");
    assert_eq!(rt.err(), "oops1\n");
}

#[test]
fn unimplemented_api_fails_loudly() {
    let rt = runtime();
    assert!(
        rt.fails("util.TraceLine({})")
            .contains("fpkg's runtime does not implement util.TraceLine")
    );
    assert!(
        rt.fails("Vector(1, 2, 3)")
            .contains("fpkg's runtime does not implement Vector")
    );
    assert!(
        rt.fails("ents.GetAll()")
            .contains("does not implement ents.GetAll")
    );
    assert!(rt.eval::<bool>("util.NotAnApiName == nil and NotAGlobal == nil"));
    assert!(rt.eval::<bool>(
        "istable(weapons) and istable(scripted_ents) and istable(team) and istable(navmesh)"
    ));
    assert!(rt.eval::<bool>("effects.Register == nil"));
    assert!(
        rt.fails("http.Fetch('x')")
            .contains("does not implement http.Fetch")
    );
    assert_eq!(rt.eval::<i64>("#player.GetAll()"), 0);
    assert!(rt.eval::<bool>("rawget(util, 'TraceLine') == nil"));
}

#[test]
fn errors_carry_the_stack() {
    let rt = runtime();
    let f = rt
        .rt
        .load(
            b"local function inner()\n  error('boom')\nend\ninner()",
            "x.lua",
        )
        .unwrap();
    let e = rt.rt.call(&f, ()).unwrap_err();
    assert_eq!(e.message, "x.lua:2: boom");
    assert_eq!(e.stack[0].name, "error");
    assert_eq!(e.stack[1].name, "inner");
    assert_eq!(e.stack[1].line, 2);
    assert_eq!(e.stack.last().unwrap().source, "x.lua");
    assert!(
        e.traceback()
            .starts_with("  1. error - [C]:-1\n   2. inner - x.lua:2\n")
    );
}

#[test]
fn include_resolves_next_to_the_running_file_then_from_lua() {
    let rt = runtime();
    let root = rt.root().to_path_buf();
    write(
        &root,
        "lib/a.lua",
        "return include('b.lua'), include('gm/lib/c.lua')",
    );
    write(&root, "lib/b.lua", "return 'b'");
    write(&root, "lib/c.lua", "return debug.getinfo(1, 'S').source");
    write(
        &root,
        ".fpkg/garrysmod/lua/shared/d.lua",
        "return 'd', include('e.lua')",
    );
    write(&root, ".fpkg/garrysmod/lua/shared/e.lua", "return 'e'");
    let v = rt.run("return include('gm/lib/a.lua')");
    let s: Vec<String> = v.iter().map(|x| x.to_string().unwrap()).collect();
    assert_eq!(s, ["b", "@gamemodes/gm/lib/c.lua"]);
    let v = rt.run("return include('shared/d.lua')");
    assert_eq!(v.len(), 2);
    rt.run("x = include('missing.lua') y = include('gm/lib/a.txt')");
    let err = rt.err();
    assert!(err.contains("[ERROR] Couldn't include file 'missing.lua' - File not found or is empty (@test.lua (line 1))"), "{err}");
    assert!(
        err.contains("Couldn't include file 'gm/lib/a.txt' - Not a .lua file!"),
        "{err}"
    );
    write(&root, "lib/bad.lua", "error('inside')");
    write(&root, "lib/empty.lua", "");
    let v = rt.run("return include('gm/lib/bad.lua'), 'after'");
    assert!(v[0].is_nil());
    assert_eq!(v[1].to_string().unwrap(), "after");
    assert!(
        rt.err()
            .contains("[ERROR] gamemodes/gm/lib/bad.lua:1: inside")
    );
    rt.run("include('gm/lib/empty.lua')");
    assert!(
        rt.err()
            .contains("'gm/lib/empty.lua' - File not found or is empty")
    );
    assert_eq!(rt.rt.reported_errors(), 4);
}

#[test]
fn compile_and_run_strings() {
    let rt = runtime();
    assert_eq!(rt.eval::<i64>("CompileString('return 1 + 1', 'id')()"), 2);
    assert_eq!(
        rt.eval::<String>("CompileString(\"MsgN('Hi)\", 'TestCode', false)"),
        "TestCode:1: unfinished string near '<eof>'"
    );
    assert!(rt.eval::<bool>("CompileString('x x', 'id') == nil"));
    assert!(rt.err().contains("[ERROR] id:1:"));
    rt.run("RunString('ran = 5')");
    assert_eq!(rt.eval::<i64>("ran"), 5);
    assert_eq!(
        rt.eval::<String>("RunString('error(\"no\", 0)', 'id', false)"),
        "no"
    );
    assert!(rt.eval::<bool>("RunString('error(\"no\", 0)') == nil"));
}

#[test]
fn require_finds_built_in_modules() {
    let rt = runtime();
    rt.run("require('file') require('hook') require('file')");
    assert!(rt.eval::<bool>("istable(File) and isfunction(File.write)"));
    #[cfg(feature = "postgres")]
    {
        rt.run("require('pg')");
        assert!(rt.eval::<bool>("isfunction(pg.new_connection) and pg.version == '2.1.0'"));
    }
    #[cfg(not(feature = "postgres"))]
    assert!(
        rt.fails("require('pg')")
            .contains("this build of fpkg has no PostgreSQL support")
    );
    assert!(rt.fails("require('mysqloo')").contains("mysqloo"));
    assert!(rt.fails("require('nothing')").contains("Module not found!"));
    write(
        rt.root(),
        ".fpkg/garrysmod/lua/includes/modules/mine.lua",
        "mine_loaded = (mine_loaded or 0) + 1",
    );
    rt.run("require('mine') require('mine')");
    assert_eq!(rt.eval::<i64>("mine_loaded"), 1);
}

#[test]
fn sql_result_shapes() {
    let rt = runtime();
    rt.run(
        "assert(sql.Query('CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, note TEXT, data BLOB)') == nil)",
    );
    rt.run("assert(sql.Query(\"INSERT INTO t (name, score) VALUES ('a', 1.5); INSERT INTO t (name, score, note) VALUES ('b', 0.1, 'n');\") == nil)");
    let rows: String = rt.eval(
        "(function() local r = sql.Query('SELECT * FROM t ORDER BY id') local out = {} for _, row in ipairs(r) do out[#out + 1] = row.id..'|'..row.name..'|'..row.score..'|'..row.note..'|'..type(row.id) end return table.concat(out, ' ') end)()",
    );
    assert_eq!(rows, "1|a|1.5|NULL|string 2|b|0.1|n|string");
    assert!(rt.eval::<bool>("sql.Query('SELECT * FROM t WHERE id = 99') == nil"));
    assert!(rt.eval::<bool>("sql.Query('SELEC nonsense') == false"));
    assert!(
        rt.eval::<String>("sql.LastError()")
            .contains("syntax error")
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('SELECT count(*) FROM t')"),
        "2"
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryRow('SELECT name FROM t ORDER BY id', 2).name"),
        "b"
    );
    assert!(rt.eval::<bool>("sql.QueryValue('SELECT 1 WHERE 0') == nil"));
    assert!(rt.eval::<bool>("sql.TableExists('t') and not sql.TableExists('nope')"));
    rt.run("sql.Query('CREATE INDEX t_name ON t (name)')");
    assert!(rt.eval::<bool>("sql.IndexExists('t_name') and not sql.IndexExists('t')"));
    assert_eq!(rt.eval::<String>("sql.SQLStr(\"it's\")"), "'it''s'");
    assert_eq!(rt.eval::<String>("sql.SQLStr('a\\0b', true)"), "a");
    assert_eq!(rt.eval::<String>("SQLStr(5)"), "'5'");
    let typed: String = rt.eval(
        "(function() local r = sql.QueryTyped('SELECT id, name, score, note FROM t WHERE id = ?', 1) local row = r[1] return type(row.id)..' '..row.score..' '..tostring(row.note) end)()",
    );
    assert_eq!(typed, "number 1.5 nil");
    assert!(rt.eval::<bool>("#sql.QueryTyped('SELECT * FROM t WHERE id = ?', 99) == 0"));
    rt.run("assert(#sql.QueryTyped('INSERT INTO t (name, note, score) VALUES (?, ?, ?)', true, nil, 2) == 0)");
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('SELECT name FROM t WHERE id = 3')"),
        "1"
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('SELECT note FROM t WHERE id = 3')"),
        "NULL"
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryTyped('SELECT 9007199254740993 AS big')[1].big"),
        "9007199254740993"
    );
    assert!(rt.eval::<bool>("sql.QueryTyped('SELECT ?') == false"));
    assert!(rt.eval::<String>("sql.LastError()").contains("parameter"));
    assert!(
        rt.fails("sql.QueryTyped('SELECT ?', {})")
            .contains("bad argument #2 to 'QueryTyped'")
    );
    assert!(rt.eval::<bool>("sql.QueryTyped('SELECT 1 AS one; SELECT 2 AS two')[1].one == 1"));
    rt.run("sql.Begin() sql.Query(\"INSERT INTO t (name) VALUES ('c')\") sql.Commit()");
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('SELECT count(*) FROM t')"),
        "4"
    );
    assert!(rt.root().join(".fpkg/garrysmod/sv.db").is_file());
}

#[test]
fn timers_and_the_loop() {
    let rt = runtime();
    rt.run(
        "fired = {} timer.Simple(0, function() fired[#fired + 1] = 'simple' timer.Simple(0, function() fired[#fired + 1] = 'nested' end) end)",
    );
    assert_eq!(rt.eval::<i64>("#fired"), 0);
    assert!(rt.rt.pump());
    assert_eq!(rt.eval::<String>("table.concat(fired, ',')"), "simple");
    rt.rt.pump();
    assert_eq!(
        rt.eval::<String>("table.concat(fired, ',')"),
        "simple,nested"
    );
    rt.run("count = 0 timer.Create('rep', 0, 3, function() count = count + 1 end)");
    assert!(rt.eval::<bool>("timer.Exists('rep') and timer.RepsLeft('rep') == 3"));
    rt.rt.settle();
    assert_eq!(rt.eval::<i64>("count"), 3);
    assert!(!rt.eval::<bool>("timer.Exists('rep')"));
    rt.run("timer.Create('later', 100, 0, function() end) timer.Pause('later')");
    assert!(rt.eval::<bool>("timer.TimeLeft('later') < -99 and timer.IsPaused('later')"));
    assert!(rt.eval::<bool>("timer.UnPause('later') and timer.TimeLeft('later') > 99"));
    assert!(rt.eval::<bool>("timer.Adjust('later', 200) and timer.TimeLeft('later') > 199"));
    assert!(!rt.eval::<bool>("timer.Adjust('nope', 1)"));
    rt.run("timer.Remove('later')");
    assert!(rt.eval::<bool>("not timer.Exists('later') and timer.TimeLeft('later') == nil"));
    rt.run("timer.Create('bad', 0, 2, function() error('in timer') end)");
    rt.rt.settle();
    assert!(rt.err().contains("[ERROR] test.lua:1: in timer"));
    assert_eq!(rt.rt.reported_errors(), 1);
    assert!(!rt.eval::<bool>("timer.Exists('bad')"));
    rt.run("thinks = 0 hook.Add('Think', 'count', function() thinks = thinks + 1 end)");
    rt.rt.pump();
    rt.rt.pump();
    assert_eq!(rt.eval::<i64>("thinks"), 2);
    rt.run("done = false timer.Simple(0.05, function() done = true end)");
    let f: nl::mlua::Function = rt.eval("function() return done end");
    assert!(rt.rt.wait_until(&f, 5.0).unwrap());
    let never: nl::mlua::Function = rt.eval("function() return false end");
    let start = std::time::Instant::now();
    assert!(!rt.rt.wait_until(&never, 0.05).unwrap());
    assert!(start.elapsed().as_secs_f64() < 2.0);
    rt.run("timer.Create('forever', 1000, 0, function() end)");
    let start = std::time::Instant::now();
    rt.rt.settle();
    assert!(start.elapsed().as_secs_f64() < 1.0);
}

#[test]
fn posted_closures_run_at_the_next_turn() {
    let rt = runtime();
    let poster = rt.rt.poster();
    std::thread::spawn(move || {
        poster.post(|lua| lua.globals().set("posted", 7));
    })
    .join()
    .unwrap();
    assert!(rt.eval::<bool>("posted == nil"));
    rt.rt.pump();
    assert_eq!(rt.eval::<i64>("posted"), 7);
}

#[test]
fn the_file_library_reads_anywhere_and_writes_to_data() {
    let rt = runtime();
    let root = rt.root().to_path_buf();
    write(&root, "notes.txt", "in the gamemode");
    assert!(rt.eval::<bool>("file.Write('Dir/Out.TXT', 'x') == false"));
    rt.run("file.CreateDir('Dir') assert(file.Write('Dir/Out.TXT', 'one'))");
    assert_eq!(
        std::fs::read_to_string(root.join(".fpkg/garrysmod/data/dir/out.txt")).unwrap(),
        "one"
    );
    rt.run("assert(file.Append('dir/out.txt', ' two'))");
    assert_eq!(rt.eval::<String>("file.Read('dir/out.txt')"), "one two");
    assert_eq!(
        rt.eval::<String>("file.Read('dir/out.txt', 'DATA')"),
        "one two"
    );
    assert_eq!(
        rt.eval::<String>("file.Read('gamemodes/gm/notes.txt', true)"),
        "in the gamemode"
    );
    assert!(rt.eval::<bool>("file.Read('missing.txt') == nil"));
    assert!(rt.eval::<bool>(
        "file.Write('x.lua', 'no') == false and file.Write('../x.txt', 'no') == false"
    ));
    assert!(rt.eval::<bool>("file.Write('a:b.txt', 'no') == false"));
    assert!(!root.join(".fpkg/garrysmod/x.txt").exists());
    assert!(rt.eval::<bool>("file.Exists('dir', 'DATA') and file.IsDir('dir', 'DATA') and not file.IsDir('dir/out.txt', 'DATA')"));
    assert_eq!(rt.eval::<i64>("file.Size('dir/out.txt', 'DATA')"), 7);
    assert!(rt.eval::<bool>(
        "file.Time('dir/out.txt', 'DATA') > 1700000000 and file.Time('nope', 'DATA') == 0"
    ));
    let found: String = rt.eval("(function() local f, d = file.Find('*', 'DATA') return table.concat(f, ',')..'|'..table.concat(d, ',') end)()");
    assert_eq!(found, "|dir");
    assert!(rt.eval::<bool>(
        "select('#', file.Find('../*', 'DATA')) == 2 and file.Find('../*', 'DATA') == nil"
    ));
    rt.run("local f = file.Open('dir/bin.dat', 'wb', 'DATA') f:WriteLong(-2) f:WriteByte(7) f:Write('line\\nrest') f:Close()");
    let read: String = rt.eval("(function() local f = file.Open('dir/bin.dat', 'rb', 'DATA') local n, b = f:ReadLong(), f:ReadByte() local l = f:ReadLine() local r = f:Read() local eof = f:EndOfFile() f:Close() return n..' '..b..' '..l..r..' '..tostring(eof)..' '..type(f) end)()");
    assert_eq!(read, "-2 7 line\nrest true File");
    assert!(rt.eval::<bool>(
        "file.Open('x.txt', 'wb', 'GAME') == nil and file.Open('dir/bin.dat', 'wb', 'DATA') ~= nil"
    ));
    assert!(rt.eval::<bool>(
        "file.Rename('dir/out.txt', 'dir/moved.txt') and file.Exists('dir/moved.txt', 'DATA')"
    ));
    assert!(
        rt.eval::<bool>(
            "file.Delete('dir/moved.txt') and not file.Exists('dir/moved.txt', 'DATA')"
        )
    );
    rt.run("got = nil file.AsyncRead('gamemodes/gm/notes.txt', 'GAME', function(name, path, status, data) got = status..' '..data end)");
    assert!(rt.eval::<bool>("got == nil"));
    rt.rt.pump();
    assert_eq!(rt.eval::<String>("got"), "0 in the gamemode");
}

#[test]
fn the_file_module_writes_under_garrysmod_as_gmsv_file() {
    let rt = runtime();
    let root = rt.root().to_path_buf();
    rt.run("require('file')");
    assert!(rt.eval::<bool>("File.write('lua/_flux/environment.lua', 'x = 1\\0junk')"));
    assert_eq!(
        std::fs::read_to_string(root.join(".fpkg/garrysmod/lua/_flux/environment.lua")).unwrap(),
        "x = 1"
    );
    assert!(rt.eval::<bool>("File.write('gamemodes/gm/db/migrate/1_a.lua', 'm')"));
    assert_eq!(
        std::fs::read_to_string(root.join("db/migrate/1_a.lua")).unwrap(),
        "m"
    );
    assert_eq!(
        rt.eval::<String>("File.read('gamemodes/gm/db/migrate/1_a.lua')"),
        "m"
    );
    assert!(rt.eval::<bool>("File.read('nope.txt') == false"));
    assert!(rt.eval::<bool>("File.append('gamemodes/gm/db/migrate/1_a.lua', 'n') and File.read('gamemodes/gm/db/migrate/1_a.lua') == 'mn'"));
    assert!(rt.eval::<bool>("File.append('new/dir/x.txt', 'n') == false"));
    for bad in [
        "../x.txt",
        "/etc/x",
        "~x",
        "a:b",
        "a\\\\b",
        "data/$x",
        "data/%x",
        "data/../../x",
    ] {
        assert!(
            rt.eval::<bool>(&format!(
                "File.write('{bad}', 'x') == false and File.read('{bad}') == false"
            )),
            "{bad}"
        );
    }
    assert!(rt.eval::<bool>("File.mkdir('data/made') and not File.mkdir('data/made')"));
    assert!(root.join(".fpkg/garrysmod/data/made").is_dir());
    assert!(rt.eval::<bool>("File.delete('data/made') and not File.delete('data/made')"));
    let handle: String = rt.eval("(function() local h = File.open('data/h.txt', 'w+') h:write('abc') h:close() local r = File.open('data/h.txt', 'r') local s = r:read() r:close() return s..' '..tostring(File.open('data/h.txt', 'x')) end)()");
    assert_eq!(handle, "abc false");
    assert!(
        rt.fails("local h = File.open('data/h.txt', 'r') h:close() h:read()")
            .contains("open file handle expected")
    );
}

#[test]
fn util_json_keyvalues_and_hashes_through_lua() {
    let rt = runtime();
    assert_eq!(rt.eval::<String>("util.CRC('a')"), "3904355907");
    assert_eq!(
        rt.eval::<String>("util.CRC(123)"),
        rt.eval::<String>("util.CRC('123')")
    );
    assert_eq!(
        rt.eval::<String>("util.SHA256('Hello')"),
        "185f8db32271fe25f561a6fc938b2e264306ec304eda518007d1764826381969"
    );
    assert_eq!(
        rt.eval::<String>("util.MD5('abc')"),
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert_eq!(
        rt.eval::<String>("util.SHA1('abc')"),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        rt.eval::<String>("util.Base64Decode(util.Base64Encode('x\\0y'))"),
        "x\0y"
    );
    assert_eq!(
        rt.eval::<String>("util.TableToJSON({ 1, 2, { a = true } })"),
        "[1,2,{\"a\":true}]"
    );
    assert_eq!(
        rt.eval::<i64>("util.JSONToTable('{\"5\":{\"x\":3}}')[5].x"),
        3
    );
    assert!(rt.eval::<bool>("util.JSONToTable('{\"5\":1}', false, true)['5'] == 1"));
    assert!(rt.eval::<bool>("util.JSONToTable('nope') == nil"));
    let kv: String = rt.eval("(function() local t = util.KeyValuesToTable('\"hl2rp\" { \"Title\" \"HL2 RP\" \"maxplayers\" \"64\" }') return t.title..' '..(t.maxplayers + 1) end)()");
    assert_eq!(kv, "HL2 RP 65");
    assert!(rt.eval::<bool>(
        "util.KeyValuesToTable('\"r\" { \"Title\" \"x\" }', false, true).Title == 'x'"
    ));
    assert_eq!(
        rt.eval::<String>(
            "util.KeyValuesToTablePreserveOrder('\"r\" { \"b\" \"1\" \"a\" \"2\" }')[2].Key"
        ),
        "a"
    );
    assert_eq!(
        rt.eval::<String>("util.SteamIDTo64('STEAM_0:1:12345')"),
        "76561197960290419"
    );
    assert_eq!(rt.eval::<String>("util.NiceFloat(1.50)"), "1.5");
    assert_eq!(
        rt.eval::<i64>("util.AddNetworkString('Msg')"),
        rt.eval::<i64>("util.NetworkStringToID('msg')")
    );
}

#[test]
fn sorted_pairs_orders_by_key() {
    let rt = runtime();
    let order: String = rt.eval("(function() local out = {} for k, v in SortedPairs({ c = 1, a = 2, b = 3, B = 4, ['10'] = 5, ['9'] = 6 }) do out[#out + 1] = k end return table.concat(out, ' ') end)()");
    assert_eq!(order, "10 9 B a b c");
    let desc: String = rt.eval("(function() local out = {} for k in SortedPairs({ 3, 1, 2, [10] = 0 }, true) do out[#out + 1] = k end return table.concat(out, ' ') end)()");
    assert_eq!(desc, "10 3 2 1");
    let by_value: String = rt.eval("(function() local out = {} for k, v in SortedPairsByValue({ a = 3, b = 1, c = 2 }) do out[#out + 1] = k end return table.concat(out, ' ') end)()");
    assert_eq!(by_value, "b c a");
    let by_member: String = rt.eval("(function() local out = {} for k in SortedPairsByMemberValue({ a = { n = 'b' }, b = { n = 'A' }, c = { n = 'c' } }, 'n') do out[#out + 1] = k end return table.concat(out, ' ') end)()");
    assert_eq!(by_member, "b a c");
}

#[test]
fn hooks_console_commands_and_convars() {
    let rt = runtime();
    rt.run("hook.Add('Ask', 'a', function(x) if x == 1 then return 'first', 2 end end) gm = { Ask = function(self, x) return 'gamemode '..x end }");
    assert_eq!(rt.eval::<String>("hook.Call('Ask', gm, 1)"), "first");
    assert_eq!(rt.eval::<String>("hook.Call('Ask', gm, 3)"), "gamemode 3");
    assert!(rt.eval::<bool>("hook.Run('Ask', 3) == nil"));
    rt.run("local obj = { valid = true } obj.IsValid = function(self) return self.valid end hook.Add('Ask', obj, function(self, x) return 'object' end) obj_ref = obj");
    assert_eq!(rt.eval::<String>("hook.Call('Ask', nil, 5)"), "object");
    rt.run("obj_ref.valid = false hook.Call('Ask', nil, 5)");
    assert!(rt.eval::<bool>("hook.GetTable().Ask[obj_ref] == nil"));
    rt.run("hook.Remove('Ask', 'a')");
    assert!(rt.eval::<bool>("hook.GetTable().Ask.a == nil"));
    rt.run("concommand.Add('Flux', function(actor, cmd, args, str) got = { tostring(actor), cmd, table.concat(args, '|'), str } end)");
    assert!(
        rt.rt
            .run_command("flux db:migrate \"two words\" VERSION=1")
            .unwrap()
    );
    assert_eq!(
        rt.eval::<String>("table.concat(got, ';')"),
        "nil;flux;db:migrate|two words|VERSION=1;db:migrate \"two words\" VERSION=1"
    );
    rt.run("RunConsoleCommand('flux', 'a', 'b')");
    assert_eq!(rt.eval::<String>("got[4]"), "a b");
    rt.run("cv = CreateConVar('fpkg_test', '5', FCVAR_ARCHIVE, 'help', 0, 10) changed = nil cvars.AddChangeCallback('fpkg_test', function(n, old, new) changed = old..'>'..new end, 'id')");
    assert!(rt.rt.run_command("fpkg_test 20").unwrap());
    assert_eq!(rt.eval::<String>("changed"), "5>10");
    assert!(rt.eval::<bool>("GetConVar('fpkg_test'):GetInt() == 10 and GetConVarNumber('fpkg_test') == 10 and cv:GetBool()"));
    assert!(rt.eval::<bool>("CreateConVar('fpkg_test', '1') == cv and cv:GetDefault() == '5' and cv:IsFlagSet(FCVAR_ARCHIVE)"));
    rt.run("cv:Revert()");
    assert_eq!(rt.eval::<String>("cvars.String('fpkg_test')"), "5");
    assert!(!rt.rt.run_command("changelevel gm_construct").unwrap());
    assert!(rt.err().contains("'changelevel' does nothing here"));
}

#[test]
fn registered_modules_open_once() {
    let rt = runtime();
    rt.rt.register_module(
        "mine",
        Rc::new(|lua| {
            let n: i64 = lua.globals().get::<Option<i64>>("opened")?.unwrap_or(0);
            lua.globals().set("opened", n + 1)
        }),
    );
    rt.run("require('mine') require('mine')");
    assert_eq!(rt.eval::<i64>("opened"), 1);
    assert!(rt.eval::<bool>("_MODULES.mine == true"));
}

#[test]
fn syntax_checks() {
    assert!(crate::check_syntax(b"local x = 1 continue_ok = x != 2 && !false", "a.lua").is_ok());
    let e = crate::check_syntax(b"task('x', function()\n", "pkg/taskspec.lua").unwrap_err();
    assert!(e.starts_with("pkg/taskspec.lua:2:"), "{e}");
}

#[test]
fn timers_exist_during_their_last_run_and_go_when_they_fail() {
    let rt = runtime();
    rt.run("seen = {} timer.Create('rep', 0, 3, function() seen[#seen + 1] = timer.RepsLeft('rep')..':'..tostring(timer.Exists('rep')) end)");
    rt.rt.settle();
    assert_eq!(
        rt.eval::<String>("table.concat(seen, ' ')"),
        "2:true 1:true 0:true"
    );
    assert!(!rt.eval::<bool>("timer.Exists('rep')"));
    rt.run(
        "runs = 0 timer.Create('err', 0, 3, function() runs = runs + 1 error('probe error') end)",
    );
    rt.rt.settle();
    assert_eq!(rt.eval::<i64>("runs"), 1);
    assert!(!rt.eval::<bool>("timer.Exists('err')"));
    assert_eq!(rt.err().matches("probe error").count(), 1);
    rt.run("forever = 0 timer.Create('forever', 0, 0, function() forever = forever + 1 if forever == 2 then error('stop') end end)");
    rt.rt.settle();
    assert_eq!(rt.eval::<i64>("forever"), 2);
    assert!(!rt.eval::<bool>("timer.Exists('forever')"));
    rt.run("adjusted = {} timer.Create('adjust', 0, 3, function() if timer.RepsLeft('adjust') == 0 and #adjusted == 0 then timer.Adjust('adjust', 0, 2) adjusted[1] = timer.RepsLeft('adjust') else adjusted[#adjusted + 1] = timer.RepsLeft('adjust') end end)");
    rt.rt.settle();
    assert_eq!(rt.eval::<String>("table.concat(adjusted, ' ')"), "2 1 0");
    assert!(!rt.eval::<bool>("timer.Exists('adjust')"));
    rt.run("again = 0 timer.Create('again', 0, 1, function() again = again + 1 if again == 1 then timer.Create('again', 0, 1, function() again = 10 end) end end)");
    rt.rt.settle();
    assert_eq!(rt.eval::<i64>("again"), 10);
}

#[test]
fn errors_of_engine_functions_are_strings_as_in_the_game() {
    let rt = runtime();
    let probe = |code: &str| rt.eval::<String>(&format!("(function() {code} end)()"));
    assert_eq!(
        probe("local ok, err = pcall(file.Exists) return tostring(ok)..'|'..type(err)..'|'..err"),
        "false|string|bad argument #1 to '?' (string expected, got no value)"
    );
    assert_eq!(
        probe("local ok, err = pcall(timer.Simple, 'x') return err"),
        "bad argument #1 to '?' (number expected, got string)"
    );
    assert_eq!(
        probe("local ok, err = pcall(function() file.Exists() end) return err"),
        "test.lua:1: bad argument #1 to 'Exists' (string expected, got no value)"
    );
    assert_eq!(
        probe("local ok, err = pcall(function() file.Exists('x', nil) end) return err"),
        "test.lua:1: bad argument #2 to 'Exists' (string expected, got nil)"
    );
    assert_eq!(
        probe(
            "local ok, err = pcall(function() local exists = file.Exists exists(1, {}) end) return err"
        ),
        "test.lua:1: bad argument #2 to 'exists' (string expected, got table)"
    );
    assert_eq!(
        probe("return select(2, xpcall(file.Exists, function(e) return type(e)..': '..e end))"),
        "string: bad argument #1 to '?' (string expected, got no value)"
    );
    assert_eq!(
        probe("return select(2, xpcall(function(a, b) return a + b end, debug.traceback, 1, 2))"),
        "3"
    );
    assert_eq!(
        probe(
            "local co = coroutine.create(function() util.CRC() end) local ok, err = coroutine.resume(co) return type(err)..': '..err"
        ),
        "string: test.lua:1: bad argument #1 to 'CRC' (string expected, got no value)"
    );
    assert_eq!(
        probe("local ok, err = pcall(require, 'mysqloo') return type(err)..': '..err"),
        "string: the MySQL module (mysqloo) is not available in fpkg tasks yet"
    );
    assert_eq!(
        probe("local ok, err = pcall(function() sql.QueryTyped('SELECT ?', {}) end) return err"),
        "test.lua:1: bad argument #2 to 'QueryTyped' (nil, boolean, number or string expected, got table)"
    );
    assert_eq!(
        probe(
            "local f = file.Open('x.txt', 'wb', 'DATA') local ok, err = pcall(function() f:Seek('x') end) f:Close() return err"
        ),
        "test.lua:1: bad argument #1 to 'Seek' (number expected, got string)"
    );
    assert_eq!(
        probe(
            "local ok, err = pcall(function() error({ code = 7 }) end) return type(err)..' '..err.code"
        ),
        "table 7"
    );
    assert!(rt.eval::<bool>("pcall(print, 'fine') == true"));
    assert_eq!(
        probe("return select(2, pcall(pcall))"),
        "bad argument #1 to '?' (value expected)"
    );
}

#[test]
fn calls_from_rust_into_lua_nest_200_deep_at_most() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let rt = runtime();
            write(rt.root(), "self.lua", "depth = (depth or 0) + 1 return include('gm/self.lua')");
            let start = std::time::Instant::now();
            rt.run("include('gm/self.lua')");
            assert!(rt.err().contains("[ERROR] stack overflow"), "{}", rt.err());
            assert_eq!(rt.rt.reported_errors(), 1);
            assert!(rt.eval::<i64>("depth") <= 200);
            rt.run("n = 0 function G() n = n + 1 RunString('G()') end G()");
            assert!(rt.eval::<i64>("n") <= 200);
            assert!(rt.fails("local t = setmetatable({}, { __tostring = function(s) print(s) return 'x' end }) print(t)").contains("stack overflow"));
            assert!(start.elapsed().as_secs_f64() < 20.0, "{:?}", start.elapsed());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn pcall_xpcall_and_coroutines_are_the_game_s() {
    let rt = runtime();
    let probe = |code: &str| rt.eval::<String>(&format!("(function() {code} end)()"));
    assert_eq!(
        probe(
            "local co = coroutine.create(function() return pcall(coroutine.yield, 'y1') end) local ok, v = coroutine.resume(co) local ok2, inner, back = coroutine.resume(co, 'back') return tostring(ok)..' '..v..' '..tostring(inner)..' '..back"
        ),
        "true y1 true back"
    );
    assert_eq!(
        probe(
            "local co = coroutine.create(function() return xpcall(coroutine.yield, debug.traceback, 'y2') end) local ok, v = coroutine.resume(co) return tostring(ok)..' '..v"
        ),
        "true y2"
    );
    assert_eq!(
        probe(
            "local gen = coroutine.wrap(function() pcall(function() coroutine.yield('a') coroutine.yield('b') end) return 'c' end) return gen()..gen()..gen()"
        ),
        "abc"
    );
    assert_eq!(
        probe(
            "local function r(n) if n == 0 then return 'bottom' end return select(2, pcall(r, n - 1)) end return r(5000)"
        ),
        "bottom"
    );
    assert_eq!(
        probe("return select(2, pcall(xpcall, print, 1))"),
        "bad argument #2 to '?' (function expected, got number)"
    );
    assert!(rt.eval::<bool>(
        "(function() local t = {} local ok, e = pcall(error, t) return not ok and e == t end)()"
    ));
    assert!(rt.eval::<bool>("(function() local t = setmetatable({}, {}) local ok, e = xpcall(function() error(t) end, function(x) return x end) return e == t end)()"));
    assert_eq!(
        probe("local ok, err = pcall(file.Exists) return err"),
        "bad argument #1 to '?' (string expected, got no value)"
    );
    assert_eq!(
        probe(
            "local co = coroutine.create(function() file.Exists() end) local ok, err = coroutine.resume(co) return type(err)"
        ),
        "string"
    );
}

#[test]
fn tracebacks_hold_no_addresses_of_the_runtime() {
    let rt = runtime();
    write(
        rt.root(),
        "traced.lua",
        "traced = debug.traceback('in a file')",
    );
    rt.run("include('gm/traced.lua') timer.Simple(0, function() in_timer = debug.traceback('in a timer') end) ErrorNoHaltWithStack('with stack')");
    rt.rt.settle();
    for t in [
        rt.eval::<String>("traced"),
        rt.eval::<String>("in_timer"),
        rt.err(),
    ] {
        assert!(!t.contains("0x") && !t.contains("builtin#"), "{t}");
    }
    assert!(
        rt.eval::<String>("traced")
            .contains("in function 'include'")
    );
}

#[test]
fn deep_keyvalues_give_an_empty_table() {
    let rt = runtime();
    assert!(
        rt.eval::<bool>(
            "next(util.KeyValuesToTable('\"a\"'..string.rep('{\"b\"', 200000))) == nil"
        )
    );
}

#[test]
fn paths_with_a_drive_or_a_colon_leave_the_tree() {
    let rt = runtime();
    assert!(rt.eval::<bool>(
        "file.Read('C:/Windows/win.ini', 'GAME') == nil and file.Exists('C:/', 'GAME') == false and file.Find('C:/*', 'GAME') == nil"
    ));
    rt.run("include('C:/x.lua')");
    assert!(
        rt.err()
            .contains("Couldn't include file 'C:/x.lua' - File not found")
    );
}

#[test]
fn bytes_pass_through_sql_and_json_unchanged() {
    let rt = runtime();
    rt.run("sql.Query('CREATE TABLE b (a TEXT)') sql.Query(\"INSERT INTO b (a) VALUES ('\\255\\254')\")");
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('SELECT hex(a) FROM b')"),
        "FFFE"
    );
    assert!(rt.eval::<bool>("sql.QueryValue('SELECT a FROM b') == '\\255\\254'"));
    assert!(rt.eval::<bool>("sql.QueryTyped('SELECT a FROM b')[1].a == '\\255\\254'"));
    assert!(rt.eval::<bool>("sql.QueryTyped('SELECT ? AS v', '\\255x')[1].v == '\\255x'"));
    assert!(rt.eval::<bool>("sql.Query('SELECT 1 AS \"\\255\"')[1]['\\255'] == '1'"));
    assert_eq!(rt.eval::<i64>("#util.TableToJSON({ '\\255\\254' })"), 6);
    assert_eq!(
        rt.eval::<i64>("#util.JSONToTable(util.TableToJSON({ '\\255\\254' }))[1]"),
        2
    );
}

#[test]
fn sqlite_as_the_game_s() {
    let rt = runtime();
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('PRAGMA foreign_keys')"),
        "0"
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryValue('PRAGMA journal_mode')"),
        "delete"
    );
    rt.run("sql.Query('CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (p INTEGER REFERENCES p (id))')");
    assert!(rt.eval::<bool>("sql.Query('INSERT INTO c (p) VALUES (5)') == nil"));
    assert_eq!(
        rt.eval::<String>("sql.QueryTyped('select -9223372036854775808 as v')[1].v"),
        "-9223372036854775808"
    );
    assert_eq!(
        rt.eval::<String>("sql.QueryTyped('select 9007199254740993 as v')[1].v"),
        "9007199254740993"
    );
    assert_eq!(
        rt.eval::<String>("sql.Query('select 9007199254740993 as v')[1].v"),
        "9007199254740993"
    );
    assert!(rt.eval::<bool>("sql.QueryTyped('select 1.5 as v, 2 as w')[1].v == 1.5 and sql.QueryTyped('select 1.5 as v, 2 as w')[1].w == 2"));
    assert_eq!(
        rt.eval::<String>("sql.Query('select 0.1 as v, 1e300 as w, 2.0 as x')[1].v..' '..sql.Query('select 1e300 as w')[1].w..' '..sql.Query('select 2.0 as x')[1].x"),
        "0.1 1.0e+300 2.0"
    );
    assert!(rt.eval::<bool>("sql.Query('SELECT 1; ; SELECT 2 AS two; -- end') [2].two == '2'"));
    assert!(rt.eval::<bool>("sql.Query('  ') == nil and sql.Query('-- nothing') == nil"));
}

#[test]
fn files_written_to_data_are_lowercased_and_found_in_byte_order() {
    let rt = runtime();
    rt.run("file.CreateDir('fpkg_probe') for _, name in ipairs({ 'b.txt', 'A.txt', 'a2.txt', 'C.txt', '_x.txt', '10.txt', '9.txt' }) do file.Write('fpkg_probe/'..name, name) end file.CreateDir('fpkg_probe/Zdir') file.CreateDir('fpkg_probe/adir')");
    let found = |order: &str| {
        rt.eval::<String>(&format!(
            "(function() local f, d = file.Find('fpkg_probe/*', 'DATA', '{order}') return table.concat(f, ',')..' '..table.concat(d, ',') end)()"
        ))
    };
    assert_eq!(
        found("nameasc"),
        "10.txt,9.txt,_x.txt,a.txt,a2.txt,b.txt,c.txt adir,zdir"
    );
    assert_eq!(
        found("namedesc"),
        "c.txt,b.txt,a2.txt,a.txt,_x.txt,9.txt,10.txt zdir,adir"
    );
    let data = rt.root().join(".fpkg/garrysmod/data/fpkg_probe");
    assert!(data.join("a.txt").is_file() && data.join("zdir").is_dir());
    assert!(!data.join("A.txt").exists() && !data.join("Zdir").exists());
    rt.run("local f = file.Open('fpkg_probe/Opened.TXT', 'w', 'DATA') f:Write('x') f:Close() file.Append('fpkg_probe/Appended.txt', 'y')");
    assert!(data.join("opened.txt").is_file() && data.join("appended.txt").is_file());
    rt.run("require('file') File.write('data/fpkg_probe/Kept.txt', 'k')");
    assert!(data.join("Kept.txt").is_file());
}

#[test]
fn nothing_is_created_until_something_is_written() {
    let rt = runtime();
    rt.run("file.Read('x.txt') file.Exists('x', 'DATA') file.Find('*', 'DATA') file.Find('*', 'LUA') include('nope.lua') require('file') File.read('data/x.txt') File.open('data/y.txt', 'r') File.open('data/y.txt', 'x') file.Open('z.txt', 'rb', 'DATA')");
    assert!(!rt.root().join(".fpkg").exists());
    rt.run("file.Write('x.txt', 'x')");
    let gm = rt.root().join(".fpkg/garrysmod");
    assert_eq!(
        std::fs::read_to_string(gm.join(".gitignore")).unwrap(),
        "*\n"
    );
    assert!(gm.join("data/x.txt").is_file() && gm.join("lua").is_dir());

    let rt = runtime();
    rt.run("sql.Query('SELECT 1')");
    assert!(rt.root().join(".fpkg/garrysmod/sv.db").is_file());
    assert!(rt.root().join(".fpkg/garrysmod/.gitignore").is_file());
}

#[test]
fn the_stand_alone_folder_is_not_part_of_the_game_s_tree() {
    let rt = runtime();
    write(rt.root(), "lib/a.lua", "return 'a'");
    rt.run("file.Write('x.txt', 'x')");
    write(rt.root(), ".fpkg/garrysmod/lua/b.lua", "return 'b'");
    let dirs: String = rt.eval(
        "(function() local f, d = file.Find('gm/*', 'LUA') return table.concat(d, ',') end)()",
    );
    assert_eq!(dirs, "lib");
    let dirs: String = rt.eval("(function() local f, d = file.Find('gamemodes/gm/*', 'GAME') return table.concat(d, ',') end)()");
    assert_eq!(dirs, "lib");
    assert!(rt.eval::<bool>(
        "not file.Exists('gamemodes/gm/.fpkg', 'GAME') and not file.IsDir('gm/.fpkg', 'LUA') and file.Read('gamemodes/gm/.fpkg/garrysmod/data/x.txt', 'GAME') == nil"
    ));
    rt.run("include('gm/.fpkg/garrysmod/lua/b.lua')");
    assert!(
        rt.err()
            .contains("Couldn't include file 'gm/.fpkg/garrysmod/lua/b.lua'")
    );
    assert_eq!(rt.eval::<String>("include('b.lua')"), "b");
}

#[test]
fn escape_sequences_are_stripped_from_what_is_not_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let (output, console) = Output::capture();
    let rt = Runtime::new(Config {
        tree: Tree::standalone(dir.path(), "gm"),
        output,
        color: true,
    })
    .unwrap();
    let f = rt
        .load(
            b"ErrorNoHalt('\\27[31m[Flux]\\27[0m oops\\n') MsgC(Color(255, 0, 0), 'red', '\\n') print('\\27[1mbold\\27[0m') error('\\27[33myellow')",
            "x.lua",
        )
        .unwrap();
    let e = rt.call(&f, ()).unwrap_err();
    rt.report(&e);
    let c = console.borrow();
    assert_eq!(c.out_text(), "red\nbold\n");
    assert!(
        c.err_text()
            .starts_with("[Flux] oops\n[ERROR] x.lua:1: yellow\n"),
        "{}",
        c.err_text()
    );
    assert_eq!(rt.error_messages(), 1);
    assert_eq!(rt.reported_errors(), 1);
}

#[test]
fn messages_are_not_reported_errors() {
    let rt = runtime();
    rt.run("ErrorNoHalt('a\\n') ErrorNoHaltWithStack('b') RunConsoleCommand('changelevel', 'gm_construct')");
    assert_eq!(rt.rt.reported_errors(), 0);
    assert_eq!(rt.rt.error_messages(), 3);
    rt.run("include('missing.lua') timer.Simple(0, function() error('x') end)");
    rt.rt.settle();
    assert_eq!(rt.rt.reported_errors(), 2);
}

#[test]
fn stand_ins_that_would_mislead_fail_loudly() {
    let rt = runtime();
    assert!(
        rt.fails("game.GetMap()")
            .contains("fpkg's runtime has no map: game.GetMap is not available in tasks")
    );
    assert!(
        rt.fails("util.IsValidModel('models/x.mdl')")
            .contains("fpkg's runtime does not implement util.IsValidModel")
    );
    assert!(
        rt.eval::<bool>("game.IsDedicated() and not game.SinglePlayer() and #player.GetAll() == 0")
    );
}

#[test]
fn addon_gamemodes_are_in_lua() {
    let dir = tempfile::tempdir().unwrap();
    let gm = dir.path().join("garrysmod/gamemodes/gm");
    write(&gm, "gm.txt", "\"gm\" {}");
    write(
        dir.path(),
        "garrysmod/addons/extra/gamemodes/other/init.lua",
        "return 'addon gamemode'",
    );
    let (output, _console) = Output::capture();
    let rt = Runtime::new(Config {
        tree: Tree::server(dir.path(), "gm", &gm),
        output,
        color: false,
    })
    .unwrap();
    let f = rt
        .load(b"return include('other/init.lua')", "t.lua")
        .unwrap();
    let v = rt.call(&f, ()).unwrap();
    assert_eq!(v[0].to_string().unwrap(), "addon gamemode");
}

#[test]
fn names_are_found_whatever_their_case() {
    let rt = runtime();
    let root = rt.root().to_path_buf();
    rt.run("file.CreateDir('Fpkg_Upper') file.Write('Fpkg_Upper/Mixed.TXT', 'hello')");
    let data = root.join(".fpkg/garrysmod/data");
    assert_eq!(
        std::fs::read_to_string(data.join("fpkg_upper/mixed.txt")).unwrap(),
        "hello"
    );
    assert!(rt.eval::<bool>(
        "file.Read('Fpkg_Upper/Mixed.TXT', 'DATA') == 'hello' and file.Read('fpkg_upper/mixed.txt', 'DATA') == 'hello'"
    ));
    assert!(rt.eval::<bool>(
        "file.Exists('Fpkg_Upper/Mixed.TXT', 'DATA') and file.Exists('FPKG_UPPER/MIXED.TXT', 'DATA') and file.Size('FPKG_UPPER/MIXED.TXT', 'DATA') == 5 and file.IsDir('FPKG_upper', 'DATA')"
    ));
    assert_eq!(
        rt.eval::<String>("table.concat((file.Find('Fpkg_Upper/*', 'DATA')), ',')"),
        "mixed.txt"
    );
    write(&root, "README.md", "readme");
    write(&root, "Lib/Shared.lua", "return 'shared'");
    assert!(rt.eval::<bool>(
        "file.Exists('GameModes/GM/gm.txt', 'GAME') and file.Read('gamemodes/gm/readme.md', 'GAME') == 'readme' and file.Read('gamemodes/gm/README.md', 'GAME') == 'readme'"
    ));
    assert!(rt.eval::<bool>(
        "file.Exists('GM/lib/shared.lua', 'LUA') and file.Time('gm/LIB', 'LUA') > 0"
    ));
    assert_eq!(rt.eval::<String>("include('gm/lib/shared.lua')"), "shared");
    assert_eq!(
        rt.eval::<String>("CompileFile('GM/LIB/SHARED.LUA')()"),
        "shared"
    );
    assert_eq!(
        rt.eval::<String>("table.concat((file.Find('gamemodes/gm/*.md', 'GAME')), ',')"),
        "README.md"
    );
    rt.run("require('file')");
    assert!(rt.eval::<bool>(
        "File.read('gamemodes/gm/README.md') == 'readme' and File.read('gamemodes/gm/readme.md') == false"
    ));
}

#[test]
fn of_two_names_that_differ_by_case_the_exact_then_the_lower_case_one() {
    let rt = runtime();
    rt.run("file.Write('x.txt', '')");
    let data = rt.root().join(".fpkg/garrysmod/data");
    if std::fs::write(data.join("Same.txt"), "upper").is_err()
        || std::fs::write(data.join("same.txt"), "lower").is_err()
        || std::fs::read_to_string(data.join("Same.txt")).unwrap() != "upper"
    {
        return;
    }
    assert_eq!(rt.eval::<String>("file.Read('Same.txt')"), "upper");
    assert_eq!(rt.eval::<String>("file.Read('SAME.TXT')"), "lower");
    assert_eq!(rt.eval::<String>("file.Read('same.txt')"), "lower");
}

#[test]
fn writes_are_seen_by_lookups_that_missed_before() {
    let rt = runtime();
    rt.run("file.Write('x.txt', '')");
    assert!(!rt.eval::<bool>("file.Exists('LATER.TXT', 'DATA')"));
    rt.run("file.Write('later.txt', 'l')");
    assert!(rt.eval::<bool>("file.Exists('LATER.TXT', 'DATA')"));
    rt.run("file.CreateDir('Made')");
    assert!(rt.eval::<bool>("file.IsDir('MADE', 'DATA')"));
    rt.run("file.Delete('LATER.TXT')");
    assert!(!rt.eval::<bool>("file.Exists('later.txt', 'DATA')"));
}

#[test]
fn names_that_are_not_ascii_are_read_but_never_written() {
    let rt = runtime();
    rt.run("file.CreateDir('fpkg_upper') file.Write('fpkg_upper/ÄBC.txt', 'x') file.Write('fpkg_upper/\\255z.txt', 'x')");
    let dir = rt.root().join(".fpkg/garrysmod/data/fpkg_upper");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(rt.eval::<i64>("#(file.Find('fpkg_upper/*', 'DATA'))"), 0);
    std::fs::write(dir.join("ÄBC.txt"), "by hand").unwrap();
    assert_eq!(
        rt.eval::<String>("file.Read('fpkg_upper/ÄBC.txt')"),
        "by hand"
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(dir.join(std::ffi::OsStr::from_bytes(b"\xffz.txt")), "raw").unwrap();
        assert_eq!(
            rt.eval::<String>("file.Read('fpkg_upper/\\255Z.TXT')"),
            "raw"
        );
        assert!(rt.eval::<bool>(
            "(function() for _, n in ipairs((file.Find('fpkg_upper/*', 'DATA'))) do if n == '\\255z.txt' then return true end end end)()"
        ));
    }
}
