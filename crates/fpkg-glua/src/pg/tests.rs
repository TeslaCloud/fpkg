//! Tests of the pg module. Without a server: the text of parameters and of result fields,
//! escaping and quoting, libpq's message layout (the expected texts are what psql prints for
//! the same errors), connection strings, the listeners, and against scripted fake servers the
//! `sslmode` retries, certificate checks, a dropped TLS connection and answers that make no
//! sense. With the server that `FPKG_TEST_POSTGRES=host:port:user:password:db` names
//! (`cargo test -- --ignored`): connecting, queries, results, errors, prepared statements,
//! synchronous and asynchronous runs, `disconnect`, TLS.

use std::cell::RefCell;
use std::rc::Rc;

use bytes::{BufMut, BytesMut};
use nl::mlua::{AnyUserData, MultiValue, Value};
use postgres_protocol::message::backend::Message;

use super::text::{self, Encoding, Field, Fields};
use super::wire::{Column, Done, Set};
use super::{Converted, conninfo, module, params, queue};
use crate::{Captured, Config, Output, Runtime, Tree};

struct Rt {
    rt: Runtime,
    console: Rc<RefCell<Captured>>,
    _dir: tempfile::TempDir,
}

impl Rt {
    fn new() -> Rt {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gm.txt"), "\"gm\" {}").unwrap();
        let (output, console) = Output::capture();
        let rt = Runtime::new(Config {
            tree: Tree::standalone(dir.path(), "gm"),
            output,
            color: false,
        })
        .unwrap();
        let wait = rt.wait_until_function().unwrap();
        rt.lua().globals().set("wait_until", wait).unwrap();
        let t = Rt {
            rt,
            console,
            _dir: dir,
        };
        t.run(HELPERS);
        t
    }

    fn run(&self, code: &str) {
        let f = self.rt.load(code.as_bytes(), "pg_test.lua").unwrap();
        if let Err(e) = self.rt.call(&f, ()) {
            panic!(
                "{e}\n{}\nconsole: {}{}",
                e.traceback(),
                self.console.borrow().out_text(),
                self.console.borrow().err_text()
            );
        }
    }

    fn err(&self) -> String {
        self.console.borrow().err_text()
    }
}

const HELPERS: &str = r#"
require('pg')
function eq(name, got, want)
  if got ~= want then
    error(name .. ': got ' .. tostring(got) .. ' (' .. type(got) .. '), want ' .. tostring(want), 2)
  end
end
function sync(conn, sql, ...)
  local q = conn:query(sql)
  q:set_sync(true)
  return q:run(...)
end
"#;

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn numbers_go_out_as_whole_numbers_or_in_their_shortest_form() {
    let cases: &[(f64, &str)] = &[
        (100000.0, "100000"),
        (0.0, "0"),
        (-0.0, "0"),
        (-5.0, "-5"),
        (1e15, "1000000000000000"),
        (9007199254740992.0, "9007199254740992"),
        (4611686018427387904.0, "4611686018427387904"),
        (-9223372036854775808.0, "-9223372036854775808"),
        (9223372036854775808.0, "9223372036854775808"),
        (18446744073709551616.0, "18446744073709551616"),
        (1e20, "1e+20"),
        (1e300, "1e+300"),
        (1.0 / 3.0, "0.3333333333333333"),
        (0.1, "0.1"),
        (-1.5, "-1.5"),
        (5432.5, "5432.5"),
        (4294972728.0, "4294972728"),
        (5e-324, "5e-324"),
        (1e-5, "1e-05"),
        (2e-4, "2e-04"),
        (1.5e-10, "1.5e-10"),
        (1e15 + 0.5, "1000000000000000.5"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ];
    for (value, want) in cases {
        assert_eq!(text::number_param(*value), *want, "{value:e}");
    }
}

#[test]
fn parameters_of_every_lua_type() {
    let rt = Rt::new();
    let lua = rt.rt.lua();
    let args: MultiValue = lua
        .load("return 'self', nil, true, false, 100000, 2^53, 0.5, [[it's]], '', nil")
        .eval()
        .unwrap();
    let p = params(lua, &args).unwrap();
    let text: Vec<Option<String>> = p.iter().map(|v| v.as_deref().map(s)).collect();
    assert_eq!(
        text,
        vec![
            None,
            Some("true".into()),
            Some("false".into()),
            Some("100000".into()),
            Some("9007199254740992".into()),
            Some("0.5".into()),
            Some("it's".into()),
            Some(String::new()),
            None,
        ]
    );
    for (code, want) in [
        (
            "return 'self', 'fine', {}",
            "pg - bad parameter #2 (string, number, boolean or nil expected)",
        ),
        (
            "return 'self', print",
            "pg - bad parameter #1 (string, number, boolean or nil expected)",
        ),
        (
            "return 'self', 'a', 'b\\0c'",
            "pg - parameter #2 contains a zero byte, binary data has to go through escape_bytea",
        ),
    ] {
        let args: MultiValue = lua.load(code).eval().unwrap();
        let e = params(lua, &args).unwrap_err().to_string();
        assert!(e.contains(want), "{e}");
    }
}

#[test]
fn fields_become_lua_values_by_the_category_of_their_type() {
    let n = |t: &'static [u8]| text::convert(b'N', t);
    assert_eq!(text::convert(b'B', b"t"), Field::Bool(true));
    assert_eq!(text::convert(b'B', b"f"), Field::Bool(false));
    assert_eq!(n(b"1"), Field::Number(1.0));
    assert_eq!(n(b"-1.5"), Field::Number(-1.5));
    assert_eq!(n(b"1e+30"), Field::Number(1e30));
    assert_eq!(n(b"5e-324"), Field::Number(5e-324));
    assert_eq!(n(b"9007199254740992"), Field::Number(9007199254740992.0));
    assert_eq!(n(b"-9007199254740992"), Field::Number(-9007199254740992.0));
    assert_eq!(n(b"Infinity"), Field::Number(f64::INFINITY));
    assert_eq!(n(b"-Infinity"), Field::Number(f64::NEG_INFINITY));
    assert!(matches!(n(b"NaN"), Field::Number(v) if v.is_nan()));
    for kept in [
        &b"9007199254740993"[..],
        b"-9007199254740993",
        b"76561198012345678",
        b"123456789012345678901234567890",
        b"$1.00",
        b"pg_class",
        b"1e400",
        b"1e-400",
        b"+1",
        b"",
        b"1e",
    ] {
        assert_eq!(n(kept), Field::Text(kept), "{}", s(kept));
    }
    assert_eq!(text::convert(b'S', b"1"), Field::Text(b"1"));
    assert_eq!(text::convert(0, b"t"), Field::Text(b"t"));
}

/// A `DataRow` message as the server sends it.
fn data_row(fields: &[Option<&str>]) -> postgres_protocol::message::backend::DataRowBody {
    let mut buf = BytesMut::new();
    buf.put_u8(b'D');
    let at = buf.len();
    buf.put_i32(0);
    buf.put_i16(fields.len() as i16);
    for f in fields {
        match f {
            Some(v) => {
                buf.put_i32(v.len() as i32);
                buf.put_slice(v.as_bytes());
            }
            None => buf.put_i32(-1),
        }
    }
    let len = (buf.len() - at) as i32;
    buf[at..at + 4].copy_from_slice(&len.to_be_bytes());
    match Message::parse(&mut buf).unwrap() {
        Some(Message::DataRow(row)) => row,
        _ => unreachable!(),
    }
}

#[test]
fn rows_are_tables_by_column_name_without_null_and_with_the_first_of_a_name() {
    let rt = Rt::new();
    let lua = rt.rt.lua();
    let column = |name: &str, type_oid: u32| Column {
        name: name.as_bytes().to_vec(),
        type_oid,
    };
    let set = Set {
        columns: vec![
            column("x", 23),
            column("x", 25),
            column("b", 16),
            column("big", 20),
            column("t", 25),
        ],
        rows: vec![
            data_row(&[
                Some("1"),
                Some("2"),
                Some("t"),
                Some("9007199254740993"),
                None,
            ]),
            data_row(&[None, Some("4"), Some("f"), Some("5"), Some("five")]),
        ],
        tag: b"SELECT 2".to_vec(),
    };
    let types = [(23u32, b'N'), (25, b'S'), (16, b'B'), (20, b'N')]
        .into_iter()
        .collect();
    let done = Done {
        success: true,
        set: Some(set),
        ..Done::default()
    };
    let converted = Converted::of(lua, done, &types).unwrap();
    lua.globals()
        .set(
            "returned",
            lua.create_sequence_from(converted.returned).unwrap(),
        )
        .unwrap();
    rt.run(
        r#"
        local ok, rows, size, affected = unpack(returned)
        eq('ok', ok, true)
        eq('size', size, 2)
        eq('affected', affected, 2)
        eq('#rows', #rows, 2)
        eq('first of a name', rows[1].x, 1)
        eq('first of a name where it is NULL', rows[2].x, nil)
        eq('boolean', rows[1].b, true)
        eq('false', rows[2].b, false)
        eq('beyond 2^53', rows[1].big, '9007199254740993')
        eq('whole number', rows[2].big, 5)
        eq('NULL is absent', rows[1].t, nil)
        eq('text', rows[2].t, 'five')
        local n = 0 for _ in pairs(rows[1]) do n = n + 1 end
        eq('fields of the first row', n, 3)
        "#,
    );

    let failed = Done {
        error: b"ERROR:  boom".to_vec(),
        details: Some(Fields(vec![
            (b'S', b"ERROR".to_vec()),
            (b'V', b"ERROR".to_vec()),
            (b'C', b"22012".to_vec()),
            (b'M', b"boom".to_vec()),
            (b'P', b"15".to_vec()),
            (b'n', b"c".to_vec()),
        ])),
        lost: true,
        ..Done::default()
    };
    let converted = Converted::of(lua, failed, &types).unwrap();
    lua.globals()
        .set(
            "returned",
            lua.create_sequence_from(converted.returned).unwrap(),
        )
        .unwrap();
    rt.run(
        r#"
        local ok, err, d = unpack(returned)
        eq('ok', ok, false)
        eq('message', err, 'ERROR:  boom')
        eq('sqlstate', d.sqlstate, '22012')
        eq('severity', d.severity, 'ERROR')
        eq('message field', d.message, 'boom')
        eq('position is a number', d.position, 15)
        eq('constraint', d.constraint, 'c')
        eq('connection_lost', d.connection_lost, true)
        eq('no hint', d.hint, nil)
        "#,
    );
}

#[test]
fn escaping_and_quoting() {
    let esc = |t: &[u8]| text::escape_string(t, Encoding::Utf8, true);
    assert_eq!(esc(b"O'Reilly").unwrap(), b"O''Reilly");
    assert_eq!(esc(b"a\\b").unwrap(), b"a\\b");
    assert_eq!(
        text::escape_string(b"a\\b'", Encoding::Utf8, false).unwrap(),
        b"a\\\\b''"
    );
    assert_eq!(
        esc("привет 'мир'".as_bytes()).unwrap(),
        "привет ''мир''".as_bytes()
    );
    assert_eq!(esc("naïve 😀".as_bytes()).unwrap(), "naïve 😀".as_bytes());
    for (broken, why) in [
        (&b"\x80"[..], text::INVALID),
        (b"abc\xe2\x82", text::INCOMPLETE),
        (b"\xc0\xaf", text::INVALID),
        (b"\xed\xa0\x80", text::INVALID),
        (b"\xf4\x90\x80\x80", text::INVALID),
        (b"x\xffy", text::INVALID),
    ] {
        assert_eq!(esc(broken).unwrap_err(), why, "{broken:?}");
        assert!(!text::well_formed(broken, Encoding::Utf8));
        assert_eq!(
            text::escape_identifier(broken, Encoding::Utf8).unwrap_err(),
            why
        );
    }
    assert_eq!(
        text::escape_string(b"\xe9'", Encoding::Single, true).unwrap(),
        b"\xe9''"
    );
    assert_eq!(
        text::escape_identifier(b"\xe9\"", Encoding::Single).unwrap(),
        b"\"\xe9\"\"\""
    );
    assert!(!text::well_formed(b"\xe9", Encoding::Single));
    assert!(text::escape_string(b"\xa4\xa2", Encoding::Other, true).is_err());
    assert_eq!(text::quote_plain_name(b"we\"ird"), b"\"we\"\"ird\"");
    assert_eq!(text::quote_plain_name(b""), b"\"\"");
    assert_eq!(text::escape_bytea(b"\0\xffA"), b"\\x00ff41");
    assert_eq!(text::escape_bytea(b""), b"\\x");
    assert_eq!(text::unescape_bytea(b"\\x4a4B").unwrap(), b"JK");
    assert_eq!(text::unescape_bytea(b"\\x").unwrap(), b"");
    assert_eq!(
        text::unescape_bytea(b"4142").unwrap_err(),
        "pg - binary data has to start with \\x"
    );
    assert_eq!(
        text::unescape_bytea(b"\\x414").unwrap_err(),
        "pg - binary data is cut off"
    );
    assert_eq!(
        text::unescape_bytea(b"\\x41zz").unwrap_err(),
        "pg - binary data has something other than hex digits in it"
    );
    assert_eq!(text::affected(b"INSERT 0 5"), 5.0);
    assert_eq!(text::affected(b"UPDATE 3"), 3.0);
    assert_eq!(text::affected(b"SELECT 7"), 7.0);
    assert_eq!(text::affected(b"COPY 5000"), 5000.0);
    assert_eq!(text::affected(b"MERGE 2"), 2.0);
    assert_eq!(text::affected(b"CREATE INDEX"), 0.0);
    assert_eq!(text::affected(b""), 0.0);
}

fn error_fields(message: &str, position: Option<&str>) -> Fields {
    let mut f = vec![
        (b'S', b"ERROR".to_vec()),
        (b'V', b"ERROR".to_vec()),
        (b'C', b"42703".to_vec()),
        (b'M', message.as_bytes().to_vec()),
    ];
    if let Some(p) = position {
        f.push((b'P', p.as_bytes().to_vec()));
    }
    Fields(f)
}

#[test]
fn messages_in_the_layout_of_libpq() {
    let m = |f: &Fields, q: Option<&str>| {
        s(text::trimmed(&text::message(
            f,
            q.map(str::as_bytes),
            Encoding::Utf8,
            true,
        )))
    };
    let missing = error_fields("relation \"nowhere\" does not exist", Some("18"));
    assert_eq!(
        m(&missing, Some("select nope from nowhere")),
        "ERROR:  relation \"nowhere\" does not exist\nLINE 1: select nope from nowhere\n                         ^"
    );
    assert_eq!(
        m(&missing, None),
        "ERROR:  relation \"nowhere\" does not exist at character 18"
    );
    let nope = error_fields("column \"nope\" does not exist", Some("17"));
    assert_eq!(
        m(&nope, Some("select 1,\n  2,\n\tnope")),
        "ERROR:  column \"nope\" does not exist\nLINE 3:  nope\n         ^"
    );
    let long = "select 1 from generate_series(1,2) g where g.aaaaaaaaaaaaaaaaaaaa = 1 and bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb = 2 and cccccc = 3 and dddddddd = 4";
    assert_eq!(
        m(
            &error_fields("column g.aaaaaaaaaaaaaaaaaaaa does not exist", Some("44")),
            Some(long)
        ),
        "ERROR:  column g.aaaaaaaaaaaaaaaaaaaa does not exist\nLINE 1: select 1 from generate_series(1,2) g where g.aaaaaaaaaaaaaaa...\n                                                   ^"
    );
    let left = format!("select {}, nope, {} from x", "a".repeat(89), "b".repeat(74));
    assert_eq!(
        m(
            &error_fields("relation \"x\" does not exist", Some("185")),
            Some(&left)
        ),
        "ERROR:  relation \"x\" does not exist\nLINE 1: ...bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb from x\n                                                                      ^"
    );
    let both = format!(
        "select 1 from (select 1 as x) s where {}nope = 1{}",
        "x = 1 and ".repeat(12),
        " and x = 1".repeat(12)
    );
    assert_eq!(
        m(
            &error_fields("column \"nope\" does not exist", Some("159")),
            Some(&both)
        ),
        "ERROR:  column \"nope\" does not exist\nLINE 1: ...x = 1 and x = 1 and x = 1 and x = 1 and x = 1 and nope = 1 a...\n                                                             ^"
    );
    assert_eq!(
        m(
            &error_fields("column \"nope\" does not exist", Some("18")),
            Some("select '漢字漢字漢字', nope")
        ),
        "ERROR:  column \"nope\" does not exist\nLINE 1: select '漢字漢字漢字', nope\n                               ^"
    );
    let mut internal = error_fields("column \"nope\" does not exist", None);
    internal.0.extend([
        (b'p', b"8".to_vec()),
        (b'q', b"SELECT nope from pg_class".to_vec()),
        (
            b'W',
            b"PL/pgSQL function inline_code_block line 1 at PERFORM".to_vec(),
        ),
    ]);
    assert_eq!(
        m(
            &internal,
            Some("do $$ begin perform nope from pg_class; end $$")
        ),
        "ERROR:  column \"nope\" does not exist\nLINE 1: SELECT nope from pg_class\n               ^\nQUERY:  SELECT nope from pg_class\nCONTEXT:  PL/pgSQL function inline_code_block line 1 at PERFORM"
    );
    let raised = Fields(vec![
        (b'S', b"ERROR".to_vec()),
        (b'M', b"boom".to_vec()),
        (b'D', b"the detail".to_vec()),
        (b'H', b"the hint".to_vec()),
        (
            b'W',
            b"PL/pgSQL function inline_code_block line 1 at RAISE".to_vec(),
        ),
    ]);
    assert_eq!(
        m(&raised, None),
        "ERROR:  boom\nDETAIL:  the detail\nHINT:  the hint\nCONTEXT:  PL/pgSQL function inline_code_block line 1 at RAISE"
    );
    let notice = s(text::trimmed(&text::message(
        &raised,
        None,
        Encoding::Utf8,
        false,
    )));
    assert_eq!(notice, "ERROR:  boom\nDETAIL:  the detail\nHINT:  the hint");
    assert!(text::message(&missing, None, Encoding::Utf8, true).ends_with(b"\n"));
}

#[test]
fn connection_strings_as_libpq_reads_them() {
    let o = conninfo::parse(
        b"connect_timeout=5 host='127.0.0.1' user='pg test user' password='it\\'s a \"pass\\\\word\" = x' dbname = 'pg test' port='5432' application_name=a\\ b options='-c x=1' host=other",
    )
    .unwrap();
    assert_eq!(o.host.as_deref(), Some(&b"other"[..]));
    assert_eq!(o.user.as_deref(), Some(&b"pg test user"[..]));
    assert_eq!(
        o.password.as_deref(),
        Some(&b"it's a \"pass\\word\" = x"[..])
    );
    assert_eq!(o.dbname.as_deref(), Some(&b"pg test"[..]));
    assert_eq!(o.application_name.as_deref(), Some(&b"a b"[..]));
    assert_eq!(o.options.as_deref(), Some(&b"-c x=1"[..]));
    assert_eq!(o.connect_timeout.as_deref(), Some(&b"5"[..]));
    assert_eq!(
        conninfo::quoted(b"it's a \"pass\\word\""),
        b"'it\\'s a \"pass\\\\word\"'"
    );
    assert_eq!(
        conninfo::parse(b"port='5432' sslmode=disable bogus=1").unwrap_err(),
        "invalid connection option \"bogus\""
    );
    assert_eq!(
        conninfo::parse(b"sslcert=/x").unwrap_err(),
        "fpkg's pg module does not implement the connection option \"sslcert\""
    );
    assert_eq!(
        conninfo::parse(b"host").unwrap_err(),
        "missing \"=\" after \"host\" in connection info string"
    );
    assert_eq!(
        conninfo::parse(b"host='x").unwrap_err(),
        "unterminated quoted string in connection info string"
    );
    assert_eq!(conninfo::int_param(b" 5432 ", "port"), Ok(5432));
    assert_eq!(conninfo::int_param(b"-1", "port"), Ok(-1));
    for bad in [
        "5432.5",
        "4294972728",
        "1e+300",
        "Infinity",
        "NaN",
        "",
        "5432 sslmode=disable",
    ] {
        assert_eq!(
            conninfo::int_param(bad.as_bytes(), "port").unwrap_err(),
            format!("invalid integer value \"{bad}\" for connection option \"port\"")
        );
    }
}

#[test]
fn objects_before_a_connection() {
    let rt = Rt::new();
    rt.run(
        r#"
        eq('version', pg.version, '2.1.0')
        eq('parts', pg.version_major .. '.' .. pg.version_minor .. '.' .. pg.version_patch, pg.version)
        eq('suffix', pg.version_suffix, '')
        local db = pg.new_connection()
        eq('tostring', tostring(db), '#<DatabaseConnection>')
        eq('type', type(db), 'UserData')
        eq('unknown field', db.nope, nil)
        eq('host before connect', db.host, '')
        db.host = 'x'
        eq('fields are not set from Lua', db.host, '')
        for _, name in ipairs({ 'is_open', 'server_version', 'escape', 'query', 'query_prepared', 'disconnect',
            'activate', 'cancel', 'listen', 'set_encoding', 'prepare', 'escape_bytea' }) do
          local ok, err = pcall(db[name], db, 'x')
          eq(name .. ' needs a connection', ok, false)
          eq(name .. ' says so', err, 'pg - no connection, connect to a database first.')
        end
        local ok, err = pcall(db.query, 5, 'select 1')
        eq('bad self', err, 'bad argument #1 (pg.DatabaseConnection expected)')
        local ok, err = pcall(db.on, db, 5, print)
        eq('event name', err, 'bad argument #2 (string expected)')
        local ok, err = pcall(db.on, db, 'x', 'not a function')
        eq('listener', err, 'bad argument #3 (function expected)')
        local ok, err = db:connect('127.0.0.1', 'postgres', 'x', 'postgres', 5432, 'application_name=a\0 b')
        eq('zero byte in extra', err, 'pg - connection parameters contain a zero byte')
        eq('host getter after it', db.host, '127.0.0.1')
        eq('port getter after it', db.port, '5432')
        eq('a failed connect is no connection', pcall(db.is_open, db), false)
        local ok, err = db:connect({}, true, 5, 'db', 2^32 + 5432)
        eq("a bad port is libpq's to turn down", err, 'invalid integer value "4294972728" for connection option "port"')
        eq('user getter after a boolean', db.user, 'postgres')
        eq('password getter after a number', db.password, '')
        local ok, err = db:connect('127.0.0.1', 'postgres', nil, nil, 1, 'sslmode=sometimes')
        eq('sslmode', err, 'invalid sslmode value: "sometimes"')
        local ok, err = db:connect('127.0.0.1', 'postgres', nil, nil, 1, 'connect_timeout=soon')
        eq('connect_timeout', err, 'invalid integer value "soon" for connection option "connect_timeout"')
        local ok, err = db:connect('a,b', 'postgres', nil, nil, 1)
        eq('several hosts', err, "fpkg's pg module does not implement several hosts in one connection string")
        local ok, err = db:connect('pgtest-no-such-host.invalid', 'postgres', nil, nil, 1)
        eq('unknown host', string.find(err, 'could not translate host name "pgtest-no-such-host.invalid" to address: ', 1, true), 1)
        "#,
    );
}

#[test]
fn listeners_in_their_order_with_errors_kept_to_themselves() {
    let rt = Rt::new();
    rt.run(
        r#"
        calls = {}
        target = pg.new_connection()
        target:on('success', function(x) calls[#calls + 1] = 'first ' .. x error('part of the test') end)
        target:on('success', function(x) calls[#calls + 1] = 'second ' .. x end)
        target:once('success', function(x) calls[#calls + 1] = 'once ' .. x end)
        target:add_listener('success', function(x) calls[#calls + 1] = 'added ' .. x end)
        target:add_listener('success', function(x) calls[#calls + 1] = 'added once ' .. x end, true)
        target:add_listener('success', function(x) calls[#calls + 1] = 'added for good ' .. x end, false)
        target:on('error', function() calls[#calls + 1] = 'error' end)
        "#,
    );
    let lua = rt.rt.lua();
    let target: AnyUserData = lua.globals().get("target").unwrap();
    let m = module(lua).unwrap();
    super::hook_think(lua, &m).unwrap();
    let event = |x: &str| {
        queue(
            lua,
            &m,
            &target,
            "success",
            vec![Value::String(lua.create_string(x).unwrap())],
        )
    };
    event("1");
    rt.run("eq('nothing before Think', #calls, 0)");
    rt.rt.pump();
    rt.run(
        r#"
        eq('order', table.concat(calls, ','), 'first 1,second 1,added 1,added for good 1,once 1,added once 1')
        calls = {}
        "#,
    );
    assert!(rt.err().contains("part of the test"), "{}", rt.err());
    assert!(
        rt.err().contains("[ERROR] pg_test.lua:"),
        "reported as an error in a timer is: {}",
        rt.err()
    );
    assert_eq!(rt.rt.reported_errors(), 1);
    event("2");
    rt.rt.settle();
    rt.run(
        r#"
        eq('once ones are gone', table.concat(calls, ','), 'first 2,second 2,added 2,added for good 2')
        calls = {}
        target:remove_listeners()
        local deliver = hook.GetTable().Think.LuaEventEmitterManager_fpkg
        eq('the hook', type(deliver), 'function')
        "#,
    );
    event("3");
    rt.run(
        r#"
        hook.GetTable().Think.LuaEventEmitterManager_fpkg()
        eq('remove_listeners', #calls, 0)
        "#,
    );
}

/// The server of `FPKG_TEST_POSTGRES`, as the globals `PG_HOST`, `PG_PORT`, `PG_USER`,
/// `PG_PASSWORD` and `PG_DB`, and `connect()` to it. The tests that use it are ignored; run
/// without the variable, they fail.
fn server() -> Rt {
    let v = std::env::var("FPKG_TEST_POSTGRES")
        .expect("the server tests need FPKG_TEST_POSTGRES=host:port:user:password:db");
    let parts: Vec<&str> = v.splitn(5, ':').collect();
    assert_eq!(
        parts.len(),
        5,
        "FPKG_TEST_POSTGRES=host:port:user:password:db"
    );
    let rt = Rt::new();
    let g = rt.rt.lua().globals();
    for (name, value) in ["PG_HOST", "PG_PORT", "PG_USER", "PG_PASSWORD", "PG_DB"]
        .into_iter()
        .zip(parts)
    {
        g.set(name, value).unwrap();
    }
    rt.run(
        r#"
        function connect(extra)
          local c = pg.new_connection()
          local ok, err = c:connect(PG_HOST, PG_USER, PG_PASSWORD, PG_DB, tonumber(PG_PORT), extra)
          if not ok then error('connect: ' .. tostring(err), 2) end
          return c
        end
        "#,
    );
    rt
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_connect_and_its_failures() {
    let rt = server();
    let refused = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    rt.rt.lua().globals().set("REFUSED", refused).unwrap();
    rt.rt.lua().globals().set("SILENT", silent_port).unwrap();
    rt.run(
        r#"
        local db = connect()
        eq('is_open', db:is_open(), true)
        eq('protocol_version', db:protocol_version(), 3)
        assert(db:server_version() >= 150000, db:server_version())
        eq('host', db.host, PG_HOST)
        eq('port', db.port, PG_PORT)
        eq('database', db.database, PG_DB)

        local c = pg.new_connection()
        local ok, err = c:connect(PG_HOST, PG_USER, 'wrong ' .. PG_PASSWORD, PG_DB, tonumber(PG_PORT))
        eq('wrong password', ok, false)
        local wrong = 'connection to server at "' .. PG_HOST .. '", port ' .. PG_PORT
          .. ' failed: FATAL:  password authentication failed for user "' .. PG_USER .. '"'
        local tls = select(2, sync(db, 'show ssl'))[1].ssl == 'on'
        eq('wrong password says so, for each try of prefer', err, tls and wrong .. '\n' .. wrong or wrong)
        local ok, err = c:connect(PG_HOST, PG_USER, PG_PASSWORD, 'pgtest no such database', tonumber(PG_PORT))
        eq('unknown database', err, 'connection to server at "' .. PG_HOST .. '", port ' .. PG_PORT
          .. ' failed: FATAL:  database "pgtest no such database" does not exist')
        local ok, err = c:connect('127.0.0.1', PG_USER, PG_PASSWORD, PG_DB, REFUSED)
        eq('refused', err, 'connection to server at "127.0.0.1", port ' .. REFUSED .. ' failed: Connection refused\n'
          .. '\tIs the server running on that host and accepting TCP/IP connections?')
        local ok, err = c:connect(PG_HOST, PG_USER, '', PG_DB, tonumber(PG_PORT))
        eq('empty password', err, 'connection to server at "' .. PG_HOST .. '", port ' .. PG_PORT
          .. ' failed: fe_sendauth: no password supplied')
        local started = SysTime()
        local ok, err = c:connect('127.0.0.1', PG_USER, PG_PASSWORD, PG_DB, SILENT, 'connect_timeout=2 sslmode=disable')
        local took = SysTime() - started
        eq('timeout', err, 'connection to server at "127.0.0.1", port ' .. SILENT .. ' failed: timeout expired')
        assert(took > 1.9 and took < 3, took)
        eq('a failed connect leaves the connection alone', pcall(c.is_open, c), false)

        local ok, rows = sync(db, "select current_setting('application_name') as app")
        eq('no application_name', rows[1].app, '')
        local named = connect("application_name='pg test app'")
        local ok, rows = sync(named, "select current_setting('application_name') as app")
        eq('application_name in extra', rows[1].app, 'pg test app')
        local ok, err = db:connect(PG_HOST, PG_USER, 'wrong', PG_DB, tonumber(PG_PORT))
        eq('connect again fails', ok, false)
        eq('the connection before stays', db:is_open(), true)
        eq('and works', select(2, sync(db, 'select 1 as x'))[1].x, 1)
        "#,
    );
    drop(silent);
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_queries_and_the_shapes_of_their_results() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local ok, rows, size, affected = sync(db, [[select 1 as one, 'x' as s, true as b, false as nb, null as n,
          1.5::numeric as f, 9007199254740993::bigint as big, 'pg_class'::regclass as r, '{1,2}'::int[] as arr,
          '\x00ff'::bytea as bin, 'NaN'::float8 as nan, '1.50'::money as m, 1 as dup, 2 as dup]])
        eq('ok', ok, true)
        eq('size', size, 1)
        eq('affected of a select', affected, 1)
        local r = rows[1]
        eq('int', r.one, 1) eq('text', r.s, 'x') eq('true', r.b, true) eq('false', r.nb, false)
        eq('null', r.n, nil) eq('numeric', r.f, 1.5) eq('bigint beyond 2^53', r.big, '9007199254740993')
        eq('regclass', r.r, 'pg_class') eq('array', r.arr, '{1,2}') eq('bytea', r.bin, '\\x00ff')
        assert(r.nan ~= r.nan, 'NaN') eq('money stays text', type(r.m), 'string') eq('first of a name', r.dup, 1)

        local ok, rows, size, affected = sync(db, 'select 1 where false')
        eq('empty result', ok and size == 0 and next(rows) == nil and affected == 0, true)
        local ok, rows, size, affected = sync(db, '')
        eq('empty query', ok and size == 0 and next(rows) == nil and affected == 0, true)
        local ok, rows = sync(db, 'select 1 as a; select 2 as b')
        eq('several statements: the last', rows[1].b, 2)
        local ok, err = sync(db, 'select $1::int as a; select 2 as b', 1)
        eq('several statements with parameters fail', ok, false)

        local ok, rows = sync(db, 'select $1::int as i, $2::text as s, $3::bool as b, $4::float8 as f, $5::text as n, $6::bigint::text as big',
          42, "it's; drop table x; --", true, 1.5, nil, 2^62)
        eq('int parameter', rows[1].i, 42) eq('string parameter', rows[1].s, "it's; drop table x; --")
        eq('bool parameter', rows[1].b, true) eq('float parameter', rows[1].f, 1.5) eq('nil parameter', rows[1].n, nil)
        eq('whole number parameter', rows[1].big, '4611686018427387904')
        local raw = "\0\1\255'\\ binary \0"
        local ok, rows = sync(db, 'select $1::bytea as bin, length($1::bytea) as len', db:escape_bytea(raw))
        eq('bytea round trip', db:unescape(rows[1].bin), raw) eq('bytea length', rows[1].len, #raw)

        sync(db, 'drop table if exists pgtest_shapes')
        local ok, _, _, affected = sync(db, 'create table pgtest_shapes (n int)')
        eq('affected of create', affected, 0)
        local ok, _, size, affected = sync(db, 'insert into pgtest_shapes select generate_series(1, 10)')
        eq('affected of insert', affected, 10) eq('no rows of insert', size, 0)
        local ok, _, _, affected = sync(db, 'update pgtest_shapes set n = n + 100 where n > $1', 7)
        eq('affected of update', affected, 3)
        local ok, rows, size, affected = sync(db, 'delete from pgtest_shapes where n > 100 returning n')
        eq('affected of delete returning', affected, 3) eq('rows of delete returning', size, 3)
        local ok, err, d = sync(db, 'copy pgtest_shapes to stdout')
        eq('copy to stdout', err, 'pg - COPY TO STDOUT is not supported') eq('no details', next(d), nil)
        local ok, err, d = sync(db, 'copy pgtest_shapes from stdin')
        eq('copy from stdin', err, 'pg - COPY FROM STDIN is not supported')
        eq('after the copies', select(4, sync(db, 'select * from pgtest_shapes')), 7)
        sync(db, 'drop table pgtest_shapes')

        local ok, err = db:set_encoding('LATIN1')
        eq('set_encoding', ok, true)
        eq('client_encoding', select(2, sync(db, 'show client_encoding'))[1].client_encoding, 'LATIN1')
        eq('escape in LATIN1', db:escape('\233\''), "\233''")
        eq('text in LATIN1', select(2, sync(db, 'select $1::text = chr(233) as same', '\233'))[1].same, true)
        local ok, err = db:set_encoding('NOT_AN_ENCODING')
        eq('bad encoding', ok, false)
        assert(string.find(err, 'NOT_AN_ENCODING', 1, true), err)
        eq('no encoding', select(2, db:set_encoding()), 'invalid encoding')
        sync(db, "set client_encoding to 'UTF8'")
        eq('escape follows the encoding of a query', pcall(db.escape, db, '\233'), false)
        eq('escape UTF-8', db:escape("é'"), "é''")
        local ok, err = pcall(db.escape, db, 'broken \226\130')
        eq('broken text', err, 'incomplete multibyte character')
        eq('quote_name', db:quote_name('we"ird'), '"we""ird"')
        "#,
    );
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_errors_and_their_details() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local ok, err, d = sync(db, 'select nope from nowhere')
        eq('message', err, 'ERROR:  relation "nowhere" does not exist\nLINE 1: select nope from nowhere\n                         ^')
        eq('sqlstate', d.sqlstate, '42P01') eq('severity', d.severity, 'ERROR') eq('position', d.position, 18)
        eq('message field', d.message, 'relation "nowhere" does not exist') eq('not lost', d.connection_lost, nil)

        sync(db, 'drop table if exists pgtest_strict')
        sync(db, 'create table pgtest_strict (id int primary key, name text not null)')
        sync(db, "insert into pgtest_strict values (1, 'one')")
        local ok, err, d = sync(db, "insert into pgtest_strict values (1, 'again')")
        eq('unique', d.sqlstate, '23505') eq('detail', d.detail, 'Key (id)=(1) already exists.')
        eq('schema', d.schema, 'public') eq('table', d.table, 'pgtest_strict') eq('constraint', d.constraint, 'pgtest_strict_pkey')
        eq('whole message', err, 'ERROR:  duplicate key value violates unique constraint "pgtest_strict_pkey"\nDETAIL:  Key (id)=(1) already exists.')
        local ok, err, d = sync(db, 'insert into pgtest_strict values (2, null)')
        eq('not null column', d.column, 'name')
        sync(db, 'drop table pgtest_strict')

        local ok, err, d = sync(db, [[do $$ begin raise exception 'pgtest boom' using detail = 'the detail', hint = 'the hint', errcode = 'PGT01'; end $$]])
        eq('raised', err, 'ERROR:  pgtest boom\nDETAIL:  the detail\nHINT:  the hint\nCONTEXT:  PL/pgSQL function inline_code_block line 1 at RAISE')
        eq('raised sqlstate', d.sqlstate, 'PGT01') eq('raised hint', d.hint, 'the hint')
        local ok, err = sync(db, 'do $$ begin perform nope from pg_class; end $$')
        eq('internal query', err, 'ERROR:  column "nope" does not exist\nLINE 1: SELECT nope from pg_class\n               ^\n'
          .. 'QUERY:  SELECT nope from pg_class\nCONTEXT:  PL/pgSQL function inline_code_block line 1 at PERFORM')
        local ok, err, d = sync(db, 'select $1::int', 'abc')
        eq('parameter of the wrong type', d.sqlstate, '22P02')

        local seen = {}
        local q = db:query([[do $$ begin raise notice 'pgtest one' using detail = 'the detail'; raise warning 'pgtest two'; end $$; select 1 as x]])
        q:set_sync(true)
        q:on('notice', function(message, details) seen[#seen + 1] = message .. '|' .. details.severity .. '|' .. details.sqlstate end)
        local ok, rows = q:run()
        eq('notices before run returns', table.concat(seen, ','), 'NOTICE:  pgtest one\nDETAIL:  the detail|NOTICE|00000,WARNING:  pgtest two|WARNING|01000')
        eq('and the rows', rows[1].x, 1)
        "#,
    );
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_prepared_statements() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        eq('prepare', db:prepare('pgtest_add', 'select $1::int + $2::int as n'), true)
        local ok, err = db:prepare('pgtest_add', 'select 1')
        eq('twice', ok, false) eq('twice says why', err, 'ERROR:  prepared statement "pgtest_add" already exists')
        eq('empty name', select(2, db:prepare('', 'select 1')), 'pg - prepared query name is empty')
        eq('no definition', select(2, db:prepare('pgtest_x')), 'pg - prepared query definition is invalid')
        eq('zero byte', select(2, db:prepare('a\0b', 'select 1')), 'pg - prepared query name contains a zero byte')
        local ok, err = db:prepare('pgtest_bad', 'select from where')
        eq('bad definition', err, 'ERROR:  syntax error at or near "where"\nLINE 1: select from where\n                    ^')
        local pq = db:query_prepared('pgtest_add')
        eq('tostring', tostring(pq), '#<PreparedQuery>')
        pq:set_sync(true)
        local ok, rows, size, affected = pq:run(40, 2)
        eq('run', rows[1].n, 42) eq('size', size, 1)
        local ok, err, d = pq:run(1)
        eq('too few parameters', err, 'ERROR:  bind message supplies 1 parameters, but prepared statement "pgtest_add" requires 2')
        local ok, err, d = pq:run('x', 1)
        eq('parameter of the wrong type', err, 'ERROR:  invalid input syntax for type integer: "x"\n'
          .. "CONTEXT:  unnamed portal parameter $1 = '...'")
        eq('unprepare', db:unprepare('pgtest_add'), true)
        local ok, err, d = pq:run(1, 2)
        eq('gone', d.sqlstate, '26000')
        local ok, err = db:unprepare('pgtest_add')
        eq('unprepare of nothing', err, 'ERROR:  prepared statement "pgtest_add" does not exist')
        local ok, err = pcall(db.query_prepared, db, '')
        eq('query_prepared without a name', string.find(err, 'pg - prepared query name is empty', 1, true) ~= nil, true)
        local ok, err = pcall(pq.run, db)
        eq('run on a connection', string.find(err, 'pg.PreparedQuery expected', 1, true) ~= nil, true)
        "#,
    );
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_synchronous_and_asynchronous_runs() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        calls = {}
        local q = db:query([[do $$ begin raise notice 'first'; end $$; select 7 as n]])
        q:on('notice', function(m, d) calls[#calls + 1] = 'notice ' .. d.message end)
        q:on('success', function(rows, size, affected, ...)
          calls[#calls + 1] = 'success ' .. rows[1].n .. ' ' .. size .. ' ' .. affected .. ' ' .. select('#', ...)
        end)
        q:on('error', function(e) calls[#calls + 1] = 'error ' .. e end)
        eq('run returns nothing', select('#', q:run()), 0)
        eq('nothing before Think', #calls, 0)
        "#,
    );
    assert!(rt.rt.pump());
    rt.run(
        r#"
        eq('Think delivers', table.concat(calls, ','), 'notice first,success 7 1 1 0')
        local db = connect()
        local bad = db:query('select 1 / $1::int as x')
        local got
        bad:on('error', function(e, d, ...) got = d.sqlstate .. ' ' .. select('#', ...) end)
        bad:run(0)
        eq('wait_until delivers', wait_until(function() return got ~= nil end, 5), true)
        eq('error event', got, '22012 0')
        local direct
        local q = db:query('select 5 as n')
        q:on('success', function(rows) direct = rows[1].n end)
        q:run()
        for name, fn in pairs(hook.GetTable().Think) do
          if string.find(name, '^LuaEventEmitterManager_') then fn() end
        end
        eq('the hook called by hand delivers, as Flux calls it', direct, 5)

        local sq = db:query('select 6 as n')
        local listened = false
        sq:on('success', function() listened = true end)
        sq:set_sync(true)
        local ok, rows = sq:run()
        eq('sync returns', rows[1].n, 6)
        wait_until(function() return false end, 0.05)
        eq('no success listener for a sync query', listened, false)

        local weak = setmetatable({}, { __mode = 'k' })
        results = 0
        for i = 1, 50 do
          local q = db:query('select ' .. i .. ' as n')
          weak[q] = true
          q:on('success', function(rows) if q and rows[1].n == i then results = results + 1 end end)
          q:run()
        end
        collectgarbage() collectgarbage()
        wait_until(function() return results == 50 end, 5)
        eq('unreferenced queries deliver', results, 50)
        collectgarbage() collectgarbage()
        eq('and are collected once done', next(weak), nil)

        local co
        coroutine.wrap(function()
          local ok, rows = sync(db, 'select 42 as answer')
          co = rows[1].answer
        end)()
        eq('sync query in a coroutine', co, 42)
        "#,
    );
    assert_eq!(rt.rt.reported_errors(), 0, "{}", rt.err());
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_disconnect_and_a_lost_connection() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local q = db:query('select 1 as back')
        q:set_sync(true)
        eq('disconnect', db:disconnect(), true)
        eq('is_open', db:is_open(), false)
        eq('server_version', db:server_version(), 0)
        eq('protocol_version', db:protocol_version(), 0)
        local ok, err, d = q:run()
        eq('query on a closed connection', err, 'pg - connection is closed, connect to the database again.')
        eq('details are empty', next(d), nil)
        local ok, err = pcall(db.escape, db, 'x')
        eq('escape on a closed connection', err, 'pg - connection is closed, connect to the database again.')
        eq('quote_name needs none', db:quote_name('x'), '"x"')
        eq('escape_bytea needs none', db:escape_bytea('A'), '\\x41')
        eq('prepare on a closed connection', select(2, db:prepare('p', 'select 1')), 'pg - connection is closed, connect to the database again.')
        local async_error
        q:set_sync(false)
        q:on('error', function(e, d) async_error = e end)
        q:run()
        wait_until(function() return async_error end, 2)
        eq('async query on a closed connection', async_error, 'pg - connection is closed, connect to the database again.')

        local lost = connect()
        local ok, err, d = sync(lost, 'select pg_terminate_backend(pg_backend_pid())')
        eq('killed', ok, false) eq('lost', d.connection_lost, true)
        eq('sqlstate of the kill', d.sqlstate, '57P01') eq('severity of the kill', d.severity, 'FATAL')
        eq('message', err, 'FATAL:  terminating connection due to administrator command\n'
          .. 'server closed the connection unexpectedly\n\tThis probably means the server terminated abnormally\n'
          .. '\tbefore or while processing the request.')
        eq('is_open after', lost:is_open(), false)
        local ok, err, d = sync(lost, 'select 1')
        eq('no reconnect', err, "fpkg's pg module does not implement reconnecting a lost connection")
        eq('as lost', d.connection_lost, true)
        eq('escape on a lost connection', lost:escape("it's"), "it''s")
        local ok, err = pcall(lost.activate, lost)
        eq('activate', string.find(err, "fpkg's pg module does not implement connection:activate", 1, true) ~= nil, true)
        "#,
    );
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_tls_as_sslmode_asks() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local ssl = select(2, sync(db, 'show ssl'))[1].ssl
        if ssl == 'on' then
          for _, mode in ipairs({ 'prefer', 'require', 'allow' }) do
            local c = connect('sslmode=' .. mode)
            local ok, rows = sync(c, 'select ssl from pg_stat_ssl where pid = pg_backend_pid()')
            eq('encrypted with ' .. mode, rows[1].ssl, mode ~= 'allow')
          end
          local c = pg.new_connection()
          local ok, err = c:connect(PG_HOST, PG_USER, PG_PASSWORD, PG_DB, tonumber(PG_PORT), 'sslmode=verify-full')
          eq('a certificate of no known authority is not verified', ok, false)
          assert(string.find(err, 'failed: SSL error: certificate verify failed', 1, true), err)
          branch = 'the server has ssl on: prefer, require and allow encrypt as they should, verify-full turns its certificate down'
        else
          local c = pg.new_connection()
          local ok, err = c:connect(PG_HOST, PG_USER, PG_PASSWORD, PG_DB, tonumber(PG_PORT), 'sslmode=require')
          eq('required', err, 'connection to server at "' .. PG_HOST .. '", port ' .. PG_PORT
            .. ' failed: server does not support SSL, but SSL was required')
          local c = connect('sslmode=prefer')
          eq('prefer goes without', select(2, sync(c, 'select 1 as x'))[1].x, 1)
          branch = 'the server has ssl off: require is refused, prefer goes without'
        end
        local c = connect('sslmode=disable')
        local ok, rows = sync(c, 'select ssl from pg_stat_ssl where pid = pg_backend_pid()')
        eq('disable', rows[1].ssl, false)
        "#,
    );
    let branch: String = rt.rt.lua().globals().get("branch").unwrap();
    println!("server_tls_as_sslmode_asks: {branch}");
}

/// Fake servers that speak just enough of the protocol to script what a test needs, plain or
/// with TLS on certificates that each test makes for itself with rcgen.
mod fake {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose,
        IsCa, KeyPair, KeyUsagePurpose,
    };
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    /// A certificate that a test made: its PEM, for a root file, and the TLS configuration of a
    /// fake server that presents it.
    pub(super) struct Made {
        pub(super) pem: String,
        pub(super) config: Arc<rustls::ServerConfig>,
    }

    fn made(cert: &rcgen::Certificate, key: &KeyPair) -> Made {
        let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(rustls::ALL_VERSIONS)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], der)
            .unwrap();
        Made {
            pem: cert.pem(),
            config: Arc::new(config),
        }
    }

    /// Parameters for `pgtest.example`, with the names of its subjectAltName.
    fn params(names: &[&str]) -> CertificateParams {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let mut p = CertificateParams::new(names).unwrap();
        p.distinguished_name
            .push(DnType::CommonName, "pgtest.example");
        p
    }

    /// A throwaway certificate authority.
    pub(super) fn authority() -> CertifiedIssuer<'static, KeyPair> {
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.distinguished_name
            .push(DnType::CommonName, "fpkg test authority");
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        CertifiedIssuer::self_signed(p, KeyPair::generate().unwrap()).unwrap()
    }

    /// A server certificate that `by` signed, for the names (none: a common name alone).
    pub(super) fn issued(by: &CertifiedIssuer<'static, KeyPair>, names: &[&str]) -> Made {
        let mut p = params(names);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().unwrap();
        made(&p.signed_by(&key, by).unwrap(), &key)
    }

    /// A server certificate that is its own authority, as `openssl req -x509` makes one, valid
    /// until the end of `until` (a year) when that is given.
    pub(super) fn self_signed(names: &[&str], until: Option<i32>) -> Made {
        let mut p = params(names);
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        if let Some(year) = until {
            p.not_before = rcgen::date_time_ymd(year - 1, 1, 1);
            p.not_after = rcgen::date_time_ymd(year, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        made(&p.self_signed(&key).unwrap(), &key)
    }

    pub(super) enum Io {
        Plain(TcpStream),
        Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
    }

    impl Read for Io {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self {
                Io::Plain(s) => s.read(buf),
                Io::Tls(s) => s.read(buf),
            }
        }
    }

    impl Write for Io {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self {
                Io::Plain(s) => s.write(buf),
                Io::Tls(s) => s.write(buf),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self {
                Io::Plain(s) => s.flush(),
                Io::Tls(s) => s.flush(),
            }
        }
    }

    fn read_n(io: &mut Io, n: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0; n];
        io.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// The body of a start-up packet or of an SSLRequest.
    fn startup(io: &mut Io) -> Option<Vec<u8>> {
        let len = u32::from_be_bytes(read_n(io, 4)?.try_into().ok()?) as usize;
        read_n(io, len.checked_sub(4)?)
    }

    /// The next message of the client: its tag and body.
    pub(super) fn message(io: &mut Io) -> Option<(u8, Vec<u8>)> {
        let tag = read_n(io, 1)?[0];
        let len = u32::from_be_bytes(read_n(io, 4)?.try_into().ok()?) as usize;
        Some((tag, read_n(io, len.checked_sub(4)?)?))
    }

    pub(super) fn put(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    pub(super) fn error(fields: &[(u8, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (code, value) in fields {
            body.push(*code);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);
        put(b'E', &body)
    }

    pub(super) fn fatal(sqlstate: &str, message: &str) -> Vec<u8> {
        error(&[
            (b'S', "FATAL"),
            (b'V', "FATAL"),
            (b'C', sqlstate),
            (b'M', message),
        ])
    }

    pub(super) fn ready() -> Vec<u8> {
        put(b'Z', b"I")
    }

    pub(super) fn row_description(columns: &[(&str, u32)]) -> Vec<u8> {
        let mut body = (columns.len() as u16).to_be_bytes().to_vec();
        for (name, oid) in columns {
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
            body.extend_from_slice(&oid.to_be_bytes());
            body.extend_from_slice(&(-1i16).to_be_bytes());
            body.extend_from_slice(&(-1i32).to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        put(b'T', &body)
    }

    pub(super) fn send(io: &mut Io, bytes: &[u8]) {
        let _ = io.write_all(bytes).and_then(|_| io.flush());
    }

    /// Lets the client in and answers the query `connect` makes for the types, with none.
    pub(super) fn welcome(io: &mut Io) {
        let mut out = put(b'R', &0u32.to_be_bytes());
        for (k, v) in [
            ("server_version", "18.7"),
            ("client_encoding", "UTF8"),
            ("standard_conforming_strings", "on"),
        ] {
            out.extend(put(
                b'S',
                &[k.as_bytes(), b"\0", v.as_bytes(), b"\0"].concat(),
            ));
        }
        out.extend(put(b'K', &[0, 0, 0, 1, 0, 0, 0, 2]));
        out.extend(ready());
        send(io, &out);
        if let Some((b'Q', _)) = message(io) {
            let mut out = row_description(&[("oid", 26), ("typcategory", 18)]);
            out.extend(put(b'C', b"SELECT 0\0"));
            out.extend(ready());
            send(io, &out);
        }
    }

    /// Takes a connection: an SSLRequest is answered with TLS when there is a `tls`
    /// configuration, else with `N`. `None` when the client gives up on the way.
    fn accept(s: TcpStream, tls: &Option<Arc<rustls::ServerConfig>>) -> Option<Io> {
        s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        let mut io = Io::Plain(s);
        let body = startup(&mut io)?;
        if body.get(..4) == Some(&80_877_103u32.to_be_bytes()[..]) {
            if let Some(config) = tls {
                io.write_all(b"S").ok()?;
                let Io::Plain(s) = io else { return None };
                let conn = rustls::ServerConnection::new(config.clone()).ok()?;
                let mut t = rustls::StreamOwned::new(conn, s);
                while t.conn.is_handshaking() {
                    t.conn.complete_io(&mut t.sock).ok()?;
                }
                io = Io::Tls(Box::new(t));
            } else {
                io.write_all(b"N").ok()?;
            }
            startup(&mut io)?;
        }
        Some(io)
    }

    /// A fake server on a port of its own: each connection, the `n`th from 0, goes to
    /// `handler`, one after the other. Whether each was encrypted comes through the channel.
    pub(super) fn serve(
        tls: Option<Arc<rustls::ServerConfig>>,
        handler: impl Fn(usize, &mut Io) + Send + 'static,
    ) -> (u16, mpsc::Receiver<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut n = 0;
            for s in listener.incoming() {
                let Ok(s) = s else { return };
                if let Some(mut io) = accept(s, &tls) {
                    let _ = tx.send(matches!(io, Io::Tls(_)));
                    handler(n, &mut io);
                    n += 1;
                }
            }
        });
        (port, rx)
    }
}

fn received(rx: &std::sync::mpsc::Receiver<bool>) -> Vec<bool> {
    let mut out = Vec::new();
    while let Ok(encrypted) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
        out.push(encrypted);
    }
    out
}

#[test]
fn prefer_falls_back_to_plaintext_when_the_server_turns_the_encrypted_session_down() {
    let tls = fake::issued(&fake::authority(), &["pgtest.example"]).config;
    let (port, rx) = fake::serve(Some(tls.clone()), |_, io| {
        if matches!(io, fake::Io::Tls(_)) {
            let reject = fake::fatal(
                "28000",
                "pg_hba.conf rejects connection for host \"127.0.0.1\", user \"u\", database \"d\", SSL encryption",
            );
            fake::send(io, &reject);
        } else {
            fake::welcome(io);
        }
    });
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local c = pg.new_connection()
        local ok, err = c:connect('127.0.0.1', 'u', 'p', 'd', PORT)
        eq('prefer connects in the clear', ok, true)
        eq('and is open', c:is_open(), true)
        "#,
    );
    assert_eq!(received(&rx), [true, false]);

    let (port, rx) = fake::serve(Some(tls.clone()), |_, io| {
        let wrong = fake::fatal("28P01", "password authentication failed for user \"u\"");
        fake::send(io, &wrong);
    });
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local ok, err = pg.new_connection():connect('127.0.0.1', 'u', 'wrong', 'd', PORT)
        local failed = 'connection to server at "127.0.0.1", port ' .. PORT
          .. ' failed: FATAL:  password authentication failed for user "u"'
        eq('both attempts are reported, each with the server', err, failed .. '\n' .. failed)
        "#,
    );
    assert_eq!(received(&rx), [true, false]);

    let (port, rx) = fake::serve(Some(tls.clone()), |_, io| {
        if matches!(io, fake::Io::Tls(_)) {
            fake::welcome(io);
        } else {
            let reject = fake::fatal("28000", "no pg_hba.conf entry, no encryption");
            fake::send(io, &reject);
        }
    });
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local ok, err = pg.new_connection():connect('127.0.0.1', 'u', 'p', 'd', PORT, 'sslmode=allow')
        eq('allow tries TLS after the clear', ok, true)
        "#,
    );
    assert_eq!(received(&rx), [false, true]);

    let (port, rx) = fake::serve(Some(tls.clone()), |_, io| {
        let mut out = fake::put(b'R', &0u32.to_be_bytes());
        out.extend(fake::fatal("3D000", "database \"d\" does not exist"));
        fake::send(io, &out);
    });
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local ok, err = pg.new_connection():connect('127.0.0.1', 'u', 'p', 'd', PORT)
        eq('no other try after authentication', err, 'connection to server at "127.0.0.1", port ' .. PORT
          .. ' failed: FATAL:  database "d" does not exist')
        "#,
    );
    assert_eq!(received(&rx), [true]);
}

#[test]
fn a_tls_connection_that_ends_without_close_notify_is_lost_as_libpq_says() {
    let tls = fake::issued(&fake::authority(), &["pgtest.example"]).config;
    let (port, _rx) = fake::serve(Some(tls.clone()), |_, io| {
        fake::welcome(io);
        let _ = fake::message(io);
        if let fake::Io::Tls(t) = io {
            let _ = t.sock.shutdown(std::net::Shutdown::Both);
        }
    });
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local c = pg.new_connection()
        local ok, err = c:connect('127.0.0.1', 'u', 'p', 'd', PORT, 'sslmode=require')
        eq('connect', ok, true)
        local ok, err, d = sync(c, 'select 1')
        eq('lost', err, 'server closed the connection unexpectedly\n\tThis probably means the server terminated abnormally\n'
          .. '\tbefore or while processing the request.')
        eq('as lost', d.connection_lost, true)
        eq('is_open', c:is_open(), false)
        "#,
    );
}

#[test]
fn answers_that_make_no_sense() {
    let (port, _rx) = fake::serve(None, |n, io| {
        if n == 2 {
            fake::send(io, &fake::put(b'R', &99u32.to_be_bytes()));
            return;
        }
        fake::welcome(io);
        let _ = fake::message(io);
        let mut out = Vec::new();
        if n == 0 {
            out.extend(fake::row_description(&[("n", 23)]));
            out.extend(fake::put(b'D', &[0, 1, 0, 0, 0, 100, b'1']));
            out.extend(fake::put(b'C', b"SELECT 1\0"));
        } else {
            out.extend(fake::error(&[]));
        }
        out.extend(fake::ready());
        fake::send(io, &out);
    });
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local c = pg.new_connection()
        eq('connect', c:connect('127.0.0.1', 'u', 'p', 'd', PORT, 'sslmode=disable'), true)
        local ok, err, d = sync(c, 'select 1 as n')
        eq('a field beyond its row', err, 'lost synchronization with server: got message type "D", length 11')
        eq('loses the connection', d.connection_lost, true)
        eq('is_open', c:is_open(), false)

        local c = pg.new_connection()
        eq('connect', c:connect('127.0.0.1', 'u', 'p', 'd', PORT, 'sslmode=disable'), true)
        local ok, err, d = sync(c, 'select 1')
        eq('an error without fields', err, 'no error message available')
        eq('and no details', next(d), nil)

        local ok, err = pg.new_connection():connect('127.0.0.1', 'u', 'p', 'd', PORT, 'sslmode=disable')
        eq('an authentication of no known kind', err, 'connection to server at "127.0.0.1", port ' .. PORT
          .. ' failed: authentication method 99 not supported')
        "#,
    );
}

/// A tempdir with root files: `CA` and `OTHER` (authorities), `NOT_PEM`, `BAD_CERT` (a
/// CERTIFICATE block that holds no certificate), `DIR` (a folder), `MISSING`, and those given.
fn root_files(rt: &Rt, more: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let g = rt.rt.lua().globals();
    let mut files = vec![
        ("NOT_PEM", "subjectAltName=DNS:pgtest.example\n".to_string()),
        (
            "BAD_CERT",
            "-----BEGIN CERTIFICATE-----\nAAAAAAAA\n-----END CERTIFICATE-----\n".to_string(),
        ),
    ];
    files.extend(more.iter().map(|(k, v)| (*k, v.to_string())));
    for (name, text) in files {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        g.set(name, path.display().to_string()).unwrap();
    }
    std::fs::create_dir(dir.path().join("dir")).unwrap();
    g.set("DIR", dir.path().join("dir").display().to_string())
        .unwrap();
    g.set("MISSING", dir.path().join("none.crt").display().to_string())
        .unwrap();
    rt.run(
        r#"
        function try(host, extra)
          local ok, err = pg.new_connection():connect(host, 'u', 'p', 'd', PORT, 'hostaddr=127.0.0.1 ' .. extra)
          return ok or err
        end
        at = 'connection to server at "127.0.0.1", port ' .. PORT .. ' failed: '
        "#,
    );
    dir
}

#[test]
fn certificates_checked_against_a_root_certificate_file() {
    let authority = fake::authority();
    let other = fake::authority();
    let tls = fake::issued(&authority, &["pgtest.example"]).config;
    let (port, rx) = fake::serve(Some(tls), |_, io| fake::welcome(io));
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    let _files = root_files(&rt, &[("CA", &authority.pem()), ("OTHER", &other.pem())]);
    rt.run(
        r#"
        eq('verify-full, the name of the certificate', try('pgtest.example', 'sslmode=verify-full sslrootcert=' .. CA), true)
        eq('verify-full, another name', try('wrong.example', 'sslmode=verify-full sslrootcert=' .. CA),
          at .. 'server certificate for "pgtest.example" does not match host name "wrong.example"')
        eq('verify-ca, another name', try('wrong.example', 'sslmode=verify-ca sslrootcert=' .. CA), true)
        eq('verify-ca, another authority', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. OTHER),
          at .. 'SSL error: certificate verify failed')
        eq('verify-full, the built-in roots', try('pgtest.example', 'sslmode=verify-full'),
          at .. 'SSL error: certificate verify failed')
        eq('require checks the chain with a file', try('pgtest.example', 'sslmode=require sslrootcert=' .. OTHER),
          at .. 'SSL error: certificate verify failed')
        eq('require without the file it names', try('pgtest.example', 'sslmode=require sslrootcert=' .. MISSING), true)
        eq('verify-ca without the file it names', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. MISSING),
          at .. 'root certificate file "' .. MISSING .. '" does not exist\n'
          .. 'Either provide the file, use the system\'s trusted roots with sslrootcert=system, '
          .. 'or change sslmode to disable server certificate verification.')
        eq('a file that is no PEM', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. NOT_PEM),
          at .. 'could not read root certificate file "' .. NOT_PEM .. '": no certificate or crl found')
        eq('a block that holds no certificate', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. BAD_CERT),
          at .. 'could not read root certificate file "' .. BAD_CERT .. '": a certificate in it is not one that can be read')
        eq('a folder', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. DIR),
          at .. 'could not read root certificate file "' .. DIR .. '": no certificate or crl found')
        eq('system with verify-ca', try('pgtest.example', 'sslmode=verify-ca sslrootcert=system'),
          'weak sslmode "verify-ca" may not be used with sslrootcert=system (use "verify-full")')
        eq('system makes verify-full the default', try('pgtest.example', 'sslrootcert=system'),
          at .. 'SSL error: certificate verify failed')
        eq('prefer goes without when the chain fails', try('pgtest.example', 'sslrootcert=' .. OTHER), true)
        "#,
    );
    assert_eq!(
        received(&rx),
        [true, true, true, false],
        "the sessions that got through the handshake: verify-full and verify-ca with their names, require without its file, prefer in the clear"
    );
}

#[test]
fn a_certificate_that_is_its_own_root() {
    let own = fake::self_signed(&["pgtest.example"], None);
    let (port, rx) = fake::serve(Some(own.config.clone()), |_, io| fake::welcome(io));
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    let _files = root_files(&rt, &[("OWN", &own.pem)]);
    rt.run(
        r#"
        for _, mode in ipairs({ 'verify-ca', 'verify-full', 'require', 'prefer' }) do
          eq(mode .. ' with the certificate as the root', try('pgtest.example', 'sslmode=' .. mode .. ' sslrootcert=' .. OWN), true)
        end
        eq('verify-full, another name', try('wrong.example', 'sslmode=verify-full sslrootcert=' .. OWN),
          at .. 'server certificate for "pgtest.example" does not match host name "wrong.example"')
        "#,
    );
    assert_eq!(received(&rx), [true, true, true, true], "all four over TLS");

    let old = fake::self_signed(&["pgtest.example"], Some(2021));
    let (port, _rx) = fake::serve(Some(old.config.clone()), |_, io| fake::welcome(io));
    rt.rt.lua().globals().set("PORT", port).unwrap();
    let _old_files = root_files(&rt, &[("OLD", &old.pem)]);
    rt.run(
        r#"
        eq('an expired one', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. OLD),
          at .. 'SSL error: certificate verify failed')
        "#,
    );

    let authority = fake::authority();
    let common = fake::issued(&authority, &[]);
    let (port, _rx) = fake::serve(Some(common.config.clone()), |_, io| fake::welcome(io));
    rt.rt.lua().globals().set("PORT", port).unwrap();
    let _common_files = root_files(&rt, &[("CA", &authority.pem())]);
    rt.run(
        r#"
        eq('a common name alone, verify-full', try('pgtest.example', 'sslmode=verify-full sslrootcert=' .. CA),
          at .. 'the server certificate has no subjectAltName for a host name, and fpkg\'s pg module does not read '
          .. 'its common name: reissue it with a subjectAltName for "pgtest.example", or use sslmode=verify-ca')
        eq('a common name alone, verify-ca', try('pgtest.example', 'sslmode=verify-ca sslrootcert=' .. CA), true)
        "#,
    );
}

#[test]
fn allow_requires_tls_the_second_time() {
    let (port, rx) = fake::serve(None, |_, io| {
        let wrong = fake::fatal("28P01", "password authentication failed for user \"u\"");
        fake::send(io, &wrong);
    });
    let rt = Rt::new();
    rt.rt.lua().globals().set("PORT", port).unwrap();
    rt.run(
        r#"
        local ok, err = pg.new_connection():connect('127.0.0.1', 'u', 'wrong', 'd', PORT, 'sslmode=allow')
        local at = 'connection to server at "127.0.0.1", port ' .. PORT .. ' failed: '
        eq('allow', err, at .. 'FATAL:  password authentication failed for user "u"\n'
          .. at .. 'server does not support SSL, but SSL was required')
        "#,
    );
    assert_eq!(received(&rx), [false], "one session, in the clear");
}

#[test]
fn names_of_a_certificate_in_libpq_s_message() {
    use super::tls::name_mismatch;
    assert_eq!(
        name_mismatch(&["DnsName(\"a.example\")".into()], "b"),
        "server certificate for \"a.example\" does not match host name \"b\""
    );
    assert_eq!(
        name_mismatch(
            &[
                "DnsName(\"a.example\")".into(),
                "IpAddress(10.0.0.1)".into(),
                "DirectoryName".into()
            ],
            "b"
        ),
        "server certificate for \"a.example\" (and 1 other name) does not match host name \"b\""
    );
    assert_eq!(
        name_mismatch(&[], "b"),
        "the server certificate has no subjectAltName for a host name, and fpkg's pg module does not read its common name: reissue it with a subjectAltName for \"b\", or use sslmode=verify-ca"
    );
}

#[test]
fn requiressl_and_sslrootcert_in_a_connection_string() {
    let o = conninfo::parse(b"requiressl=1 sslrootcert=/x/root.crt").unwrap();
    assert_eq!(o.sslmode.as_deref(), Some(&b"require"[..]));
    assert_eq!(o.sslrootcert.as_deref(), Some(&b"/x/root.crt"[..]));
    let o = conninfo::parse(b"sslmode=require requiressl=0").unwrap();
    assert_eq!(o.sslmode.as_deref(), Some(&b"prefer"[..]));
    let o = conninfo::parse(b"requiressl=1 sslmode=disable").unwrap();
    assert_eq!(o.sslmode.as_deref(), Some(&b"disable"[..]));
}

#[test]
#[ignore = "needs FPKG_TEST_POSTGRES"]
fn server_copy_in_a_prepared_statement_and_a_statement_timeout() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        sync(db, 'drop table if exists pgtest_pcopy')
        sync(db, 'create table pgtest_pcopy (n int)')
        sync(db, 'insert into pgtest_pcopy select generate_series(1, 5)')
        eq('prepare a copy out', db:prepare('pgtest_copy_out', 'copy pgtest_pcopy to stdout'), true)
        eq('prepare a copy in', db:prepare('pgtest_copy_in', 'copy pgtest_pcopy from stdin'), true)
        for name, want in pairs({ pgtest_copy_out = 'pg - COPY TO STDOUT is not supported',
            pgtest_copy_in = 'pg - COPY FROM STDIN is not supported' }) do
          local q = db:query_prepared(name)
          q:set_sync(true)
          local ok, err, d = q:run()
          eq(name, err, want)
          eq(name .. ' has no details', next(d), nil)
          eq('the connection after ' .. name, select(4, sync(db, 'select * from pgtest_pcopy')), 5)
          eq(name .. ' unprepared', db:unprepare(name), true)
        end
        sync(db, 'drop table pgtest_pcopy')

        sync(db, "set statement_timeout = '200ms'")
        local started = SysTime()
        local ok, err, d = sync(db, 'select pg_sleep(3)')
        eq('timed out', err, 'ERROR:  canceling statement due to statement timeout')
        eq('sqlstate', d.sqlstate, '57014')
        eq('not lost', d.connection_lost, nil)
        assert(SysTime() - started < 2, SysTime() - started)
        local ok, err, d = sync(db, 'select pg_sleep($1::float8)', 3)
        eq('with parameters too', d.sqlstate, '57014')
        sync(db, 'reset statement_timeout')
        eq('the connection after the timeouts', select(2, sync(db, 'select 1 as x'))[1].x, 1)
        "#,
    );
}
