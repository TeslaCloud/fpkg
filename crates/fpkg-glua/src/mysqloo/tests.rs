//! Tests of the mysqloo module. Without a server: the text of the values of the binary protocol,
//! the Lua values of fields, escaping, the rows of both protocols, the scrambles and the
//! encryption of a password, and, with results made up in place of a server's, the rows, the
//! callbacks and when they run, parameters, transactions and the errors of the methods. With
//! the server that `FPKG_TEST_MYSQL=host:port:user:password:db` names, the server tests, which
//! are ignored otherwise (`cargo test -p fpkg-glua mysqloo -- --ignored`): connecting and its
//! failures, queries and prepared queries with every parameter setter and column type,
//! several results, errors, waiting, ping, status, disconnecting, character sets, losing the
//! connection, and TLS when `FPKG_TEST_MYSQL_CA` names the certificate of the server's CA.

use std::cell::RefCell;
use std::rc::Rc;

use nl::mlua::{AnyUserData, Lua, Table};

use super::value::{self, Binary, Charset, Field, Meta, kind};
use super::wire::{self, Answer, Column, Set};
use super::{Arg, Database, Input, Native, Outcome, Queued, auth, server_version};
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
        let t = Rt {
            rt,
            console,
            _dir: dir,
        };
        t.run(HELPERS);
        t
    }

    fn lua(&self) -> &Lua {
        self.rt.lua()
    }

    fn run(&self, code: &str) {
        let f = self.rt.load(code.as_bytes(), "mysqloo_test.lua").unwrap();
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

    /// The database of the object in a global.
    fn db(&self, global: &str) -> Rc<Database> {
        let t: Table = self.lua().globals().get(global).unwrap();
        let ud: AnyUserData = t.raw_get("__CppObject").unwrap();
        let native = ud.borrow::<Native>().unwrap();
        match &*native {
            Native::Database(db) => db.clone(),
            Native::Job(_) => panic!("{global} is not a database"),
        }
    }

    /// The next run in the queue of a database.
    fn next_run(&self, global: &str) -> Rc<super::Run> {
        let entry = self.db(global).queue.borrow_mut().pop_front();
        let Some(Queued::Run(run)) = entry else {
            panic!("nothing queued to finish");
        };
        run
    }

    /// Finishes the queued runs of a database as a server would have, in their order, with
    /// these results or errors, and has their callbacks wait for `Think`.
    fn finish(&self, global: &str, outcomes: Vec<Result<Vec<Answer>, &str>>) {
        for outcome in outcomes {
            let run = self.next_run(global);
            *run.outcome.borrow_mut() = Some(match outcome {
                Ok(r) => Outcome::Rows(r),
                Err(e) => Outcome::Failed(e.as_bytes().to_vec(), Vec::new()),
            });
            self.db(global).done.borrow_mut().push_back(run);
        }
    }
}

const HELPERS: &str = r#"
require('mysqloo')
function eq(name, got, want)
  if got ~= want then
    error(name .. ': got ' .. tostring(got) .. ' (' .. type(got) .. '), want ' .. tostring(want), 2)
  end
end
function count(t) local n = 0 for _ in pairs(t) do n = n + 1 end return n end
function message(f, ...)
  local ok, err = pcall(f, ...)
  if ok then error('no error', 2) end
  return (tostring(err):gsub('^mysqloo_test%.lua:%d+: ', ''))
end
"#;

fn meta(kind: u8) -> Meta {
    Meta {
        kind,
        ..Meta::default()
    }
}

fn text(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

/// A result set of text fields.
fn set(columns: &[(&str, u8)], rows: &[&[Option<&str>]]) -> Set {
    Set {
        columns: columns
            .iter()
            .map(|(name, k)| Column {
                name: name.as_bytes().to_vec(),
                meta: meta(*k),
            })
            .collect(),
        rows: rows
            .iter()
            .map(|r| r.iter().map(|f| f.map(|s| s.as_bytes().to_vec())).collect())
            .collect(),
    }
}

fn results(sets: Vec<(Set, u64, u64)>) -> Vec<Answer> {
    sets.into_iter()
        .map(|(set, insert_id, affected_rows)| Answer {
            set,
            insert_id,
            affected_rows,
        })
        .collect()
}

#[test]
fn values_of_the_binary_protocol_as_libmariadb_writes_them() {
    let b = |m: Meta, v: Binary| text(&value::binary_text(&m, v));
    let float = |decimals| Meta {
        kind: kind::FLOAT,
        decimals,
        ..Meta::default()
    };
    let double = |decimals| Meta {
        kind: kind::DOUBLE,
        decimals,
        ..Meta::default()
    };
    assert_eq!(
        b(meta(kind::LONGLONG), Binary::Signed(i64::MIN)),
        "-9223372036854775808"
    );
    assert_eq!(
        b(meta(kind::LONGLONG), Binary::Unsigned(u64::MAX)),
        "18446744073709551615"
    );
    let zerofill = Meta {
        kind: kind::LONG,
        flags: value::ZEROFILL | value::UNSIGNED,
        length: 5,
        decimals: 0,
    };
    assert_eq!(b(zerofill, Binary::Unsigned(42)), "00042");
    let year = Meta {
        kind: kind::YEAR,
        flags: value::ZEROFILL | value::UNSIGNED,
        length: 4,
        decimals: 0,
    };
    assert_eq!(b(year, Binary::Unsigned(0)), "0000");
    assert_eq!(b(float(31), Binary::Float(0.1)), "0.1");
    assert_eq!(b(float(31), Binary::Float(16777217.0)), "16777200.0");
    assert_eq!(b(float(31), Binary::Float(1.0e-7)), "1e-7");
    assert_eq!(b(float(4), Binary::Float(1.2345)), "1.2345");
    assert_eq!(
        b(double(31), Binary::Double(1.0 / 3.0)),
        "0.3333333333333333"
    );
    assert_eq!(b(double(31), Binary::Double(5.0)), "5.0");
    assert_eq!(b(double(3), Binary::Double(1.23456)), "1.235");
    assert_eq!(b(double(31), Binary::Double(f64::INFINITY)), "0");
    assert_eq!(b(double(31), Binary::Double(f64::NAN)), "0");
    assert_eq!(b(float(31), Binary::Float(f32::NEG_INFINITY)), "0");
    let dt = |kind, decimals| Meta {
        kind,
        decimals,
        ..Meta::default()
    };
    assert_eq!(
        b(dt(kind::DATE, 0), Binary::DateTime(2024, 1, 31, 0, 0, 0, 0)),
        "2024-01-31"
    );
    assert_eq!(
        b(
            dt(kind::DATETIME, 0),
            Binary::DateTime(2024, 1, 31, 12, 34, 56, 0)
        ),
        "2024-01-31 12:34:56"
    );
    assert_eq!(
        b(
            dt(kind::DATETIME, 3),
            Binary::DateTime(2024, 1, 31, 12, 34, 56, 789000)
        ),
        "2024-01-31 12:34:56.789"
    );
    assert_eq!(
        b(
            dt(kind::TIMESTAMP, 6),
            Binary::DateTime(0, 0, 0, 0, 0, 0, 0)
        ),
        "0000-00-00 00:00:00.000000"
    );
    assert_eq!(
        b(dt(kind::TIME, 0), Binary::Time(true, 34, 22, 59, 59, 0)),
        "-838:59:59"
    );
    assert_eq!(
        b(dt(kind::TIME, 2), Binary::Time(false, 0, 1, 2, 3, 450000)),
        "01:02:03.45"
    );
    assert_eq!(
        b(meta(kind::STRING), Binary::Bytes(b"a\0\xff".to_vec())),
        "a\0\u{fffd}"
    );
}

#[test]
fn fields_become_lua_values_by_the_type_of_their_column() {
    for k in [
        kind::TINY,
        kind::SHORT,
        kind::INT24,
        kind::LONG,
        kind::LONGLONG,
        kind::FLOAT,
        kind::DOUBLE,
    ] {
        assert_eq!(value::field(k, b"42"), Field::Number(42.0), "{k}");
    }
    assert_eq!(
        value::field(kind::LONGLONG, b"9007199254740993"),
        Field::Number(9007199254740992.0)
    );
    assert_eq!(
        value::field(kind::LONGLONG, b"18446744073709551615"),
        Field::Number(18446744073709551615.0)
    );
    assert_eq!(value::field(kind::BIT, b"\x01"), Field::Number(1.0));
    assert_eq!(value::field(kind::BIT, b"\xff\x01"), Field::Number(-1.0));
    assert_eq!(value::field(kind::BIT, b""), Field::Number(0.0));
    assert_eq!(value::field(kind::NULL, b"x"), Field::Nil);
    for k in [
        0,
        246,
        kind::YEAR,
        kind::DATE,
        kind::DATETIME,
        kind::TIMESTAMP,
        kind::TIME,
        245,
        247,
        248,
        252,
        253,
        kind::STRING,
        255,
    ] {
        assert_eq!(value::field(k, b"1.50"), Field::Text(b"1.50"), "{k}");
    }
    let cases: &[(&[u8], f64)] = &[
        (b"1.5", 1.5),
        (b"  -2e3x", -2000.0),
        (b"+.5", 0.5),
        (b"5.", 5.0),
        (b"1e", 1.0),
        (b"1e+", 1.0),
        (b"x", 0.0),
        (b"", 0.0),
        (b".", 0.0),
        (b"-", 0.0),
        (b"1e400", f64::INFINITY),
        (b"-inf", f64::NEG_INFINITY),
        (b"0.1", 0.1),
        (b"00042", 42.0),
    ];
    for (t, want) in cases {
        assert_eq!(value::atof(t), *want, "{}", text(t));
    }
    assert!(value::atof(b"nan").is_nan());
}

#[test]
fn escaping_as_mysql_real_escape_string() {
    let e = |t: &[u8], cs, nb| value::escape(t, cs, nb).unwrap();
    assert_eq!(e(b"t'a", Charset::Utf8mb4, false), b"t\\'a");
    assert_eq!(
        e(b"\0\n\r\\\"\x1ax", Charset::Utf8mb4, false),
        b"\\0\\n\\r\\\\\\\"\\Zx"
    );
    assert_eq!(
        e("é😀".as_bytes(), Charset::Utf8mb4, false),
        "é😀".as_bytes()
    );
    assert_eq!(e(b"\xc3(", Charset::Utf8mb4, false), b"\xc3(");
    assert_eq!(e(b"\xf0\x9f'", Charset::Utf8mb4, false), b"\xf0\x9f\\'");
    assert_eq!(
        e("😀'".as_bytes(), Charset::Utf8mb3, false),
        "😀\\'".as_bytes()
    );
    assert_eq!(e(b"\xe9'\\", Charset::Single, false), b"\xe9\\'\\\\");
    assert_eq!(e(b"it's \\ \n", Charset::Utf8mb4, true), b"it''s \\ \n");
    assert_eq!(e(b"x'y", Charset::Multibyte, false), b"x\\'y");
    assert!(value::escape(b"\xa4\x5c", Charset::Multibyte, false).is_err());
    assert_eq!(
        value::charset(b"UTF8MB4"),
        Some(("utf8mb4", Charset::Utf8mb4))
    );
    assert_eq!(value::charset(b"utf8"), Some(("utf8", Charset::Utf8mb3)));
    assert_eq!(value::charset(b"latin1"), Some(("latin1", Charset::Single)));
    assert_eq!(value::charset(b"invalid_name"), None);
}

#[test]
fn rows_of_the_text_and_the_binary_protocol() {
    let row = wire::text_row(b"\x011\xfb\x00\xfc\x03\x00abc", 4).unwrap();
    assert_eq!(
        row,
        vec![
            Some(b"1".to_vec()),
            None,
            Some(Vec::new()),
            Some(b"abc".to_vec())
        ]
    );
    assert!(wire::text_row(b"\x05ab", 1).is_err());
    let col = |kind, flags, decimals| Column {
        name: Vec::new(),
        meta: Meta {
            kind,
            flags,
            decimals,
            length: 0,
        },
    };
    let columns = [
        col(kind::TINY, 0, 0),
        col(kind::SHORT, value::UNSIGNED, 0),
        col(kind::LONG, 0, 0),
        col(kind::LONGLONG, 0, 0),
        col(kind::DOUBLE, 0, 31),
        col(kind::DATETIME, 0, 0),
        col(kind::TIME, 0, 0),
        col(kind::STRING, 0, 0),
        col(kind::LONG, 0, 0),
    ];
    let mut p = vec![0x00, 0x00, 0x04];
    p.push(0xff);
    p.extend_from_slice(&65535u16.to_le_bytes());
    p.extend_from_slice(&(-7i32).to_le_bytes());
    p.extend_from_slice(&(1i64 << 40).to_le_bytes());
    p.extend_from_slice(&0.5f64.to_le_bytes());
    p.extend_from_slice(&[7, 0xe8, 0x07, 1, 31, 12, 34, 56]);
    p.extend_from_slice(&[8, 1, 1, 0, 0, 0, 2, 3, 4]);
    p.extend_from_slice(b"\x02hi");
    let got: Vec<Option<String>> = wire::binary_row(&p, &columns)
        .unwrap()
        .into_iter()
        .map(|f| f.map(|b| text(&b)))
        .collect();
    let want: Vec<Option<&str>> = vec![
        Some("-1"),
        Some("65535"),
        Some("-7"),
        Some("1099511627776"),
        Some("0.5"),
        Some("2024-01-31 12:34:56"),
        Some("-26:03:04"),
        Some("hi"),
        None,
    ];
    assert_eq!(
        got,
        want.into_iter()
            .map(|w| w.map(String::from))
            .collect::<Vec<_>>()
    );
}

#[test]
fn scrambles_and_the_encrypted_password() {
    let nonce: Vec<u8> = (1..=20).collect();
    assert_eq!(
        hex::encode(auth::native(b"secret", &nonce)),
        "b32bb3a583e1340c0a1108d58b1be49781ad8c2f"
    );
    assert_eq!(
        hex::encode(auth::caching_sha2(b"secret", &nonce)),
        "746ebe205d56a0707acb3e796e834e0dd7b1d61743b26bd5202c7a623230c7c9"
    );
    assert!(auth::native(b"", &nonce).is_empty());
    assert!(auth::caching_sha2(b"", &nonce).is_empty());

    let pem = b"-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDRQfqdss4XeYfatl3LIXaQjD/w\nE32BgLXkDC1J9gQfrVvMCevefolDMACWBMDKfovA1l9SlM7E5fcu4SN+a5605/GL\n9JSmDYwmPQGl5ipvnLVAGiCkcN9VeNOGLn42okbmAwi6dL5+9NtfOHgMNM2Bz5Jv\n0GYweDBFHYMvHCaM+QIDAQAB\n-----END PUBLIC KEY-----\n";
    let (n, e) = auth::public_key(pem).unwrap();
    assert_eq!(
        hex::encode(&n),
        "d141fa9db2ce177987dab65dcb2176908c3ff0137d8180b5e40c2d49f6041fad5bcc09ebde7e894330009604c0ca7e8bc0d65f5294cec4e5f72ee1237e6b9eb4e7f18bf494a60d8c263d01a5e62a6f9cb5401a20a470df5578d3862e7e36a246e60308ba74be7ef4db5f38780c34cd81cf926fd066307830451d832f1c268cf9"
    );
    assert_eq!(e, vec![1, 0, 1]);
    let message = b"pass\0word";
    let c = auth::oaep_encrypt(&n, &e, message, &[7; 20]).unwrap();
    assert_eq!(
        hex::encode(&c),
        "04110db24fe64e080f29285838905e82dcd8646e3bad5e889dd2d599829bbce18552c8cddce64d9cd09571e10f39e61f1a5ebd3ca11f5edf270303bb67d552cfc0e67731ceed62fee22016970c34af381bda6d7881626f7662d03a9d9477df63bc72ee59e96631ddc5afe5ffbd53fee5a267cb5ada689b849354abc9c75898e9",
        "computed with Python's hashlib and pow, decrypted by OpenSSL and by cryptography"
    );
    let d = hex::decode("17e70f777ffb26974d3b60656a54923484b0b0fda8f267ff5ee33d311b96f0ff5e62fa27252d1304c7b0e971f58babd4cedc2ddb430033618950a38ae85fe6aad28aa352226dabea1e2b9d5e8b6e5b79823cbf383e2d9988bc255220e8e13089f5abbb11173d663df781d101f07eab98d75f46ee28a1235ed67365df52a95e01").unwrap();
    let em = num_bigint::BigUint::from_bytes_be(&c)
        .modpow(
            &num_bigint::BigUint::from_bytes_be(&d),
            &num_bigint::BigUint::from_bytes_be(&n),
        )
        .to_bytes_be();
    let mut em_full = vec![0u8; 128 - em.len()];
    em_full.extend(em);
    let mgf1 = |seed: &[u8], len: usize| {
        use sha1::Digest;
        let mut out = Vec::new();
        for counter in 0u32.. {
            if out.len() >= len {
                break;
            }
            let mut h = sha1::Sha1::new();
            h.update(seed);
            h.update(counter.to_be_bytes());
            out.extend_from_slice(&h.finalize());
        }
        out.truncate(len);
        out
    };
    assert_eq!(em_full[0], 0);
    let (masked_seed, masked_db) = em_full[1..].split_at(20);
    let seed: Vec<u8> = masked_seed
        .iter()
        .zip(mgf1(masked_db, 20))
        .map(|(a, b)| a ^ b)
        .collect();
    assert_eq!(seed, vec![7; 20]);
    let db: Vec<u8> = masked_db
        .iter()
        .zip(mgf1(&seed, masked_db.len()))
        .map(|(a, b)| a ^ b)
        .collect();
    assert_eq!(
        hex::encode(&db[..20]),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        "the hash of the empty label"
    );
    let one = db[20..].iter().position(|b| *b == 1).unwrap() + 20;
    assert!(db[20..one].iter().all(|b| *b == 0));
    assert_eq!(&db[one + 1..], message);
    assert!(auth::oaep_encrypt(&n, &e, &[1; 100], &[0; 20]).is_err());
    let plain = auth::encrypt_password(b"x", b"ab", pem).unwrap();
    assert_eq!(plain.len(), 128);
    assert!(auth::public_key(b"nothing").is_none());
}

#[test]
fn server_versions_as_numbers() {
    assert_eq!(server_version(b"11.8.9-MariaDB-ubu2404"), 110809);
    assert_eq!(server_version(b"8.4.7"), 80407);
    assert_eq!(server_version(b"5.7"), 50700);
    assert_eq!(server_version(b""), 0);
    assert_eq!(
        server_version(b"99999999999.1.2"),
        (99999999999u64 as u32)
            .wrapping_mul(10000)
            .wrapping_add(102)
    );
}

#[test]
fn objects_and_their_errors_before_a_connection() {
    let rt = Rt::new();
    rt.run(
        r#"
        eq('version', mysqloo.VERSION .. '.' .. mysqloo.MINOR_VERSION, '9.7')
        eq('constants', mysqloo.DATABASE_CONNECTED .. mysqloo.DATABASE_CONNECTING .. mysqloo.DATABASE_NOT_CONNECTED
          .. mysqloo.DATABASE_CONNECTION_FAILED, '0123')
        eq('query constants', mysqloo.QUERY_NOT_RUNNING .. mysqloo.QUERY_RUNNING .. mysqloo.QUERY_COMPLETE
          .. mysqloo.QUERY_ABORTED .. mysqloo.QUERY_WAITING, '01345')
        eq('options', mysqloo.OPTION_NUMERIC_FIELDS + mysqloo.OPTION_NAMED_FIELDS * 10 + mysqloo.OPTION_INTERPRET_DATA * 100
          + mysqloo.OPTION_CACHE * 1000, 8421)
        eq('hook', type(hook.GetTable().Think.__MySQLOOThinkHook), 'function')
        eq('convar', GetConVar('sv_mysqloo_versioncheck'):GetString(), '1')
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database', 3306)
        eq('a table', type(db), 'table')
        eq('the handle', type(db.__CppObject), 'MySQLOO UserData')
        eq('tostring', tostring(db), 'Database ')
        eq('meta name', getmetatable(db).MetaName, 'MySQLOO Database')
        eq('status', db:status(), mysqloo.DATABASE_NOT_CONNECTED)
        eq('weak table', mysqloo.__weakDatabases[db], true)
        eq('escape', message(db.escape, db, 'x'), 'Cannot escape using database that is not connected')
        eq('escape nil', message(db.escape, db), 'Cannot escape using database that is not connected')
        eq('serverVersion', message(db.serverVersion, db), 'Tried to get server version when client is not connected to server yet!')
        eq('hostInfo', message(db.hostInfo, db), 'Tried to get server info when client is not connected to server yet!')
        eq('wait', message(db.wait, db), 'Tried to wait for database connection to finish without starting the connection!')
        eq('setCharacterSet', message(db.setCharacterSet, db, 'utf8'), 'Database needs to be connected to change charset.')
        eq('connect without strings', message(mysqloo.connect), "bad argument #1 to '?' (string expected, got no value)")
        eq('connect with a number', message(mysqloo.connect, 'a', 'b', 'c', 5), "bad argument #4 to '?' (string expected, got number)")
        eq('query of a number', message(db.query, db, 5), "bad argument #2 to '?' (string expected, got number)")
        eq('not a table', message(db.query, 5, 'x'), "bad argument #1 to '?' (table expected, got number)")
        eq('another table', message(db.query, {}, 'x'), '[MySQLOO] Expected MySQLOO table')
        eq('a boolean', message(db.setAutoReconnect, db, nil), "bad argument #2 to '?' (bool expected, got nil)")
        local q = db:query('SELECT 1')
        local p = db:prepare('SELECT ?')
        local t = db:createTransaction()
        eq('query tostring', tostring(q) .. '|' .. tostring(p) .. '|' .. tostring(t), 'MySQLOO Query |MySQLOO Prepared Query |MySQLOO Transaction ')
        eq('meta ids', getmetatable(db).MetaID .. getmetatable(q).MetaID .. getmetatable(p).MetaID .. getmetatable(t).MetaID, '47484950')
        eq('database method on a query', message(db.query, q, 'x'), '[MySQLOO] Invalid CPP Object')
        eq('query method on a database', message(q.start, db), '[MySQLOO] Invalid CPP Object')
        eq('rows of a transaction', message(q.getData, t), '[MySQLOO] Invalid CPP Object')
        eq('setOption', message(q.setOption, q, 3), 'Invalid Option')
        eq('setOption enabled', message(q.setOption, q, 1, nil), "bad argument #3 to '?' (bool expected, got nil)")
        eq('setOption 1.9', pcall(q.setOption, q, 1.9), true)
        eq('index 0', message(p.setNumber, p, 0, 1), 'Index must be greater than 0')
        eq('index 0.5', message(p.setNumber, p, 0.5, 1), 'Index must be greater than 0')
        eq('index -1', pcall(p.setNumber, p, -1, 1), true)
        eq('setString of a number', message(p.setString, p, 1, 5), "bad argument #3 to '?' (string expected, got number)")
        eq('setBoolean', message(p.setBoolean, p, 1), "bad argument #3 to '?' (bool expected, got no value)")
        eq('wait', message(q.wait, q), 'Query not started')
        eq('hasMoreResults', message(q.hasMoreResults, q), 'Query not completed yet')
        eq('getNextResults', message(q.getNextResults, q), 'C++ exception')
        eq('before a run', tostring(q:isRunning()) .. q:error() .. tostring(q:getData()) .. q:lastInsert() .. q:affectedRows(), 'falsenil00')
        eq('abort', q:abort(), false)
        eq('getQueries', count(t:getQueries()) + count(t.getQueries({})), 0)
        eq('getQueries of nothing', message(t.getQueries), 'attempt to index a nil value')
        db:setAutoReconnect(false)
        db:setMultiStatements(false)
        db:setCachePreparedStatements(false)
        eq('capath', message(db.setSSLSettings, db, nil, nil, nil, '/etc/ssl'), "fpkg's mysqloo module does not implement the capath of database:setSSLSettings")
        db:setSSLSettings('', '', '')
        eq('counters', mysqloo.objectCount() > 0 and mysqloo.referenceCreatedCount() >= mysqloo.referenceFreedCount(), true)
        "#,
    );
    let db = rt.db("db");
    assert!(!db.reconnect.get() && !db.keep_statements.get());
    let settings = db.settings.borrow();
    assert!(!settings.multi_statements);
    assert_eq!(settings.tls, None);
}

#[test]
fn rows_of_made_up_results() {
    let rt = Rt::new();
    rt.run(
        r#"
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database')
        q = db:query('SELECT')
        q:start()
        n = db:query('SELECT')
        n:setOption(mysqloo.OPTION_NUMERIC_FIELDS)
        n:start()
        "#,
    );
    let typed = set(
        &[
            ("i", kind::LONG),
            ("big", kind::LONGLONG),
            ("f", kind::DOUBLE),
            ("dec", 246),
            ("b", kind::BIT),
            ("s", 253),
            ("n", kind::LONG),
            ("dup", kind::LONG),
            ("dup", kind::LONG),
            ("gone", kind::LONG),
            ("gone", kind::LONG),
            ("na\0me", 253),
        ],
        &[
            &[
                Some("1"),
                Some("9007199254740993"),
                Some("0.5"),
                Some("1.50"),
                Some("\x01"),
                Some("x"),
                None,
                Some("1"),
                Some("2"),
                Some("3"),
                None,
                Some("cut"),
            ],
            &[
                Some("-2"),
                Some("0"),
                Some("1e3"),
                Some("0"),
                Some("\u{80}"),
                Some(""),
                Some("7"),
                None,
                Some("4"),
                None,
                Some("5"),
                Some("x"),
            ],
        ],
    );
    let mut typed_bytes = typed.clone();
    typed_bytes.rows[1][4] = Some(vec![0x80]);
    let second = set(&[("b", kind::LONG)], &[&[Some("2")]]);
    rt.finish(
        "db",
        vec![
            Ok(results(vec![
                (typed_bytes.clone(), 7, 2),
                (second, 7, 1),
                (Set::default(), 0, 0),
            ])),
            Ok(results(vec![(typed_bytes, 0, 2)])),
        ],
    );
    rt.run(
        r#"
        eq('nothing before the hook', q:getData(), nil)
        hook.GetTable().Think.__MySQLOOThinkHook()
        local data = q:getData()
        eq('the same table each time', data, q:getData())
        eq('rows', #data, 2)
        local r = data[1]
        eq('int', r.i, 1) eq('bigint beyond 2^53', r.big, 9007199254740992) eq('double', r.f, 0.5)
        eq('decimal', r.dec, '1.50') eq('bit', r.b, 1) eq('string', r.s, 'x') eq('null', r.n, nil)
        eq('the last of a name', r.dup, 2) eq('a later NULL removes the field', r.gone, nil)
        eq('names stop at a zero byte', r.na, 'cut')
        eq('fields', count(r), 8)
        local r2 = data[2]
        eq('negative', r2.i, -2) eq('exponent', r2.f, 1000) eq('bit 0x80', r2.b, -128) eq('empty', r2.s, '')
        eq('first of a name NULL', r2.dup, 4) eq('later value', r2.gone, 5)
        eq('insert id', q:lastInsert(), 7) eq('affected', q:affectedRows(), 2)
        eq('more', q:hasMoreResults(), true)
        q:getNextResults()
        local nextData = q:getData()
        eq('the next set', nextData[1].b, 2) eq('a new table', nextData ~= data, true) eq('its affected', q:affectedRows(), 1)
        q:getNextResults()
        eq('an empty set', count(q:getData()), 0) eq('more after it', q:hasMoreResults(), true)
        q:getNextResults()
        eq('none left', q:hasMoreResults(), false)
        eq('no rows without a set', count(q:getData()), 0)
        eq('past the end', message(q.getNextResults, q), 'C++ exception')
        local nr = n:getData()[1]
        eq('numeric', nr[1], 1) eq('numeric NULL', nr[7], nil) eq('numeric dup', nr[8] .. nr[9], '12')
        eq('numeric keys', count(nr), 10) eq('no names', nr.i, nil)
        "#,
    );
}

#[test]
fn callbacks_wait_for_think_and_come_in_their_order() {
    let rt = Rt::new();
    rt.run(
        r#"
        calls = {}
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database')
        q = db:query('SELECT 1')
        function q:onData(row) calls[#calls + 1] = 'data ' .. row.a .. ' ' .. tostring(self == q) end
        function q:onSuccess(data)
          calls[#calls + 1] = 'success ' .. #data .. ' ' .. tostring(data == q:getData()) .. ' ' .. tostring(q:isRunning())
        end
        q:start()
        eq('running', q:isRunning(), true)
        eq('queued while there is no connection', db:queueSize(), 1)
        bad = db:query('SELECT nope')
        bad:start()
        function bad:onError(err, sql, ...) calls[#calls + 1] = 'error ' .. err .. ' | ' .. sql .. ' ' .. select('#', ...) end
        failing = db:query('SELECT 2')
        function failing:onSuccess() error('part of the test') end
        failing:start()
        after = db:query('SELECT 3')
        after:start()
        function after:onSuccess() calls[#calls + 1] = 'after the error' end
        "#,
    );
    let row = set(&[("a", kind::LONG)], &[&[Some("1")], &[Some("2")]]);
    rt.finish(
        "db",
        vec![
            Ok(results(vec![(row.clone(), 0, 2)])),
            Err("Unknown column 'nope' in 'SELECT'"),
            Ok(results(vec![(row.clone(), 0, 2)])),
            Ok(results(vec![(row, 0, 2)])),
        ],
    );
    rt.run("eq('nothing before Think', #calls, 0)");
    rt.rt.pump();
    rt.run(
        r#"
        eq('order', table.concat(calls, ','), 'data 1 true,data 2 true,success 2 true true,'
          .. "error Unknown column 'nope' in 'SELECT' | SELECT nope 0,after the error")
        eq('done', q:isRunning(), false)
        eq('a new table after the callback', q:getData() ~= nil, true)
        eq('the error', bad:error(), "Unknown column 'nope' in 'SELECT'")
        eq('no rows of an error', bad:getData(), nil)
        eq('error has no more results', bad:hasMoreResults(), false)
        eq('wait after it', message(q.wait, q), 'Query not started')
        "#,
    );
    assert!(rt.err().contains("part of the test"), "{}", rt.err());
    assert!(
        rt.err().starts_with("[ERROR] mysqloo_test.lua:"),
        "{}",
        rt.err()
    );
    assert_eq!(rt.rt.reported_errors(), 1);

    rt.run(
        r#"
        calls = {}
        local aborted = db:query('SELECT')
        function aborted:onAborted(...) calls[#calls + 1] = 'aborted ' .. select('#', ...) end
        aborted:start()
        eq('abort a queued run', aborted:abort(), true)
        eq('onAborted at once', calls[1], 'aborted 0')
        eq('not running', aborted:isRunning(), false)
        local all = db:query('SELECT')
        function all:onAborted() calls[#calls + 1] = 'all' end
        all:start()
        local late = db:query('SELECT')
        late:start()
        function late:onAborted() calls[#calls + 1] = 'set after start' end
        eq('abortAllQueries', db:abortAllQueries(), 2)
        eq('in order', table.concat(calls, ','), 'aborted 0,all,set after start')
        eq('empty queue', db:queueSize(), 0)
        "#,
    );
    rt.run(
        r#"
        calls = {}
        local w = db:query('SELECT')
        function w:onError(err) calls[#calls + 1] = err end
        w:start()
        w:wait()
        eq('wait without a connection', calls[1], 'Can not wait on query, database is not connected or connection failed.')
        eq('still queued, as in MySQLOO', db:queueSize(), 1)
        eq('ping without a connection', db:ping(), false)
        eq('the ping is queued', db:queueSize(), 2)
        "#,
    );
}

#[test]
fn connection_callbacks_wait_for_think() {
    let rt = Rt::new();
    let refused = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    rt.lua().globals().set("REFUSED", refused).unwrap();
    rt.run(
        r#"
        got = {}
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database', REFUSED)
        function db:onConnectionFailed(err, ...) got[#got + 1] = err .. ' ' .. select('#', ...) .. ' ' .. tostring(self == db) end
        function db:onConnected() got[#got + 1] = 'connected' end
        q = db:query('SELECT 1')
        function q:onError(err) got[#got + 1] = err end
        q:start()
        db:connect()
        eq('not inside connect', #got, 0)
        eq('failed', db:status(), mysqloo.DATABASE_CONNECTION_FAILED)
        eq('connect again', message(db.connect, db), 'Database already connected.')
        eq('settings after it', message(db.setMultiStatements, db, true), 'Database already connected.')
        eq('nothing known', db:serverVersion() .. db:serverInfo() .. db:hostInfo(), '0')
        eq('escape after a failed connect', db:escape("a'b"), "a\\'b")
        q:wait()
        eq('the failure, then the query', table.concat(got, ','), "Can't connect to server on '127.0.0.1' (115) 0 true,"
          .. 'Can not wait on query, database is not connected or connection failed.')
        db:wait()
        eq('once', #got, 2)
        local socket = mysqloo.connect('localhost', 'u', 'p', 'd', 3306, '/nonexistent/fpkg.sock')
        local err
        function socket:onConnectionFailed(e) err = e end
        socket:connect()
        socket:wait()
        eq('a socket for localhost', err, "Can't connect to local server through socket '/nonexistent/fpkg.sock' (2)")
        "#,
    );
}

#[test]
fn a_delivered_run_lets_go_of_what_it_held_though_it_stays_queued() {
    let rt = Rt::new();
    let refused = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    rt.lua().globals().set("REFUSED", refused).unwrap();
    rt.run(
        r#"
        local function counts()
          for _ = 1, 10 do collectgarbage() end
          return mysqloo.objectCount(), mysqloo.referenceCreatedCount() - mysqloo.referenceFreedCount()
        end
        local objects, references = counts()
        do
          local db = mysqloo.connect('127.0.0.1', 'u', 'p', 'd', REFUSED)
          db:connect()
          local q = db:query('SELECT 1')
          q:start()
          local got
          function q:onError(err) got = err end
          q:wait()
          eq('delivered', got, 'Can not wait on query, database is not connected or connection failed.')
          eq('still queued', db:queueSize(), 1)
          eq('not running', q:isRunning(), false)
        end
        local objects_after, references_after = counts()
        eq('objects freed', objects_after - objects, 0)
        eq('references let go', references_after - references, 0)
        db = mysqloo.connect('127.0.0.1', 'u', 'p', 'd')
        q = db:query('SELECT 1 AS a')
        successes = 0
        function q:onSuccess() successes = successes + 1 end
        function q:onError() end
        q:start()
        q:wait()
        eq('failed by the wait', q:error(), 'Can not wait on query, database is not connected or connection failed.')
        "#,
    );
    rt.finish(
        "db",
        vec![Ok(results(vec![(
            set(&[("a", kind::LONG)], &[&[Some("1")]]),
            0,
            1,
        )]))],
    );
    rt.rt.pump();
    rt.run(
        r#"
        eq('run again without callbacks', successes, 0)
        eq('but shown', q:getData()[1].a, 1)
        eq('without the error', q:error(), '')
        "#,
    );
}

#[test]
fn parameters_of_prepared_queries_go_as_they_were_set() {
    let rt = Rt::new();
    rt.run(
        r#"
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database')
        p = db:prepare('SELECT ?, ?, ?, ?')
        p:setNumber(1, 5)
        p:setString(2, 'a\0b')
        p:setBoolean(3, true)
        p:setNull(4)
        p:setNumber(9, 1)
        p:setNumber(1.7, 6)
        p:start()
        p:setBoolean(3, false)
        p:putNewParameters()
        p:setNumber(1, 2)
        p:start()
        p:clearParameters()
        p:start()
        "#,
    );
    let params: Vec<Vec<super::ParamSet>> = rt
        .db("db")
        .queue
        .borrow()
        .iter()
        .map(|e| match e {
            Queued::Run(run) => match &run.input {
                Input::Prepared(sets) => sets.clone(),
                _ => panic!("not a prepared query"),
            },
            _ => panic!("not a run"),
        })
        .collect();
    let first = &params[0][0];
    assert_eq!(first.get(&1), Some(&Arg::Number(6.0)));
    assert_eq!(first.get(&2), Some(&Arg::Text(b"a\0b".to_vec())));
    assert_eq!(first.get(&3), Some(&Arg::Flag(true)));
    assert_eq!(first.get(&4), Some(&Arg::Null));
    assert_eq!(first.get(&9), Some(&Arg::Number(1.0)));
    assert_eq!(params[1].len(), 2);
    assert_eq!(params[1][0].get(&3), Some(&Arg::Flag(false)));
    assert_eq!(params[1][1].get(&1), Some(&Arg::Number(2.0)));
    assert_eq!(params[1][1].get(&3), None);
    assert_eq!(params[2].len(), 1);
    assert!(params[2][0].is_empty());
    assert_eq!(Arg::Flag(true).to_wire(), wire::Param::Long(1));
    assert_eq!(Arg::Flag(false).to_wire(), wire::Param::Long(0));
    assert_eq!(Arg::Number(0.5).to_wire(), wire::Param::Double(0.5));
}

#[test]
fn transactions_of_made_up_results() {
    let rt = Rt::new();
    rt.run(
        r#"
        db = mysqloo.connect('127.0.0.1', 'user', 'password', 'database')
        tr = db:createTransaction()
        q1 = db:query('SELECT 1')
        q2 = db:prepare('SELECT ?')
        q2:setNumber(1, 2)
        tr:addQuery(q1)
        tr:addQuery(q2)
        eq('queries', #tr:getQueries(), 2)
        eq('in order', tr:getQueries()[2], q2)
        eq('a query only', message(tr.addQuery, tr, tr), '[MySQLOO] Invalid CPP Object')
        function tr:onSuccess(data, ...) got = { data, select('#', ...) } end
        tr:start()
        tr2 = db:createTransaction()
        tr2:addQuery(db:query('x'))
        function tr2:onError(err, sql, ...) failed = err .. '|' .. sql .. '|' .. select('#', ...) end
        tr2:start()
        "#,
    );
    let run = rt.next_run("db");
    let Input::Transaction(members) = &run.input else {
        panic!("not a transaction")
    };
    let each = (0..members.len())
        .map(|i| {
            let column = if i == 0 { "a" } else { "b" };
            results(vec![(set(&[(column, kind::LONG)], &[&[Some("1")]]), 0, 1)])
        })
        .collect();
    *run.outcome.borrow_mut() = Some(Outcome::Committed(each));
    rt.db("db").done.borrow_mut().push_back(run);
    rt.finish("db", vec![Err("You have an error")]);
    rt.rt.pump();
    rt.run(
        r#"
        eq('rows of each query', got[1][1][1].a .. got[1][2][1].b, '11')
        eq('one argument', got[2], 0)
        eq('rows of a query afterwards', q1:getData()[1].a, 1)
        eq('not running', q1:isRunning(), false)
        eq('error of a transaction', failed, 'You have an error||0')
        tr:clearQueries()
        eq('cleared', #tr:getQueries(), 0)
        "#,
    );
}

/// The server of `FPKG_TEST_MYSQL`, as the globals `MY_HOST`, `MY_PORT`, `MY_USER`,
/// `MY_PASSWORD` and `MY_DB`, with `connect`, `run` and `prepared`.
fn server() -> Rt {
    let v = std::env::var("FPKG_TEST_MYSQL")
        .expect("the server tests need FPKG_TEST_MYSQL=host:port:user:password:db");
    let parts: Vec<&str> = v.splitn(5, ':').collect();
    assert_eq!(parts.len(), 5, "FPKG_TEST_MYSQL=host:port:user:password:db");
    let rt = Rt::new();
    let g = rt.lua().globals();
    for (name, value) in ["MY_HOST", "MY_PORT", "MY_USER", "MY_PASSWORD", "MY_DB"]
        .into_iter()
        .zip(parts)
    {
        g.set(name, value).unwrap();
    }
    if let Ok(ca) = std::env::var("FPKG_TEST_MYSQL_CA") {
        g.set("MY_CA", ca).unwrap();
    }
    if let Ok(socket) = std::env::var("FPKG_TEST_MYSQL_SOCKET") {
        g.set("MY_SOCKET", socket).unwrap();
    }
    rt.run(
        r#"
        function connect(before)
          local db = mysqloo.connect(MY_HOST, MY_USER, MY_PASSWORD, MY_DB, tonumber(MY_PORT))
          if before then before(db) end
          local failure
          function db:onConnectionFailed(err) failure = err end
          db:connect()
          db:wait()
          if failure then error('connect: ' .. failure, 2) end
          return db
        end
        function run(db, sql)
          local q = db:query(sql)
          q:start()
          q:wait()
          if q:error() ~= '' then error(sql .. ': ' .. q:error(), 2) end
          return q
        end
        function prepared(db, sql, ...)
          local q = db:prepare(sql)
          for i = 1, select('#', ...) do
            local v = select(i, ...)
            if type(v) == 'number' then q:setNumber(i, v) elseif type(v) == 'boolean' then q:setBoolean(i, v)
            elseif v == nil then q:setNull(i) else q:setString(i, v) end
          end
          q:start()
          q:wait()
          if q:error() ~= '' then error(sql .. ': ' .. q:error(), 2) end
          return q
        end
        "#,
    );
    rt
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_connect_and_its_failures() {
    let rt = server();
    let refused = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    rt.lua().globals().set("REFUSED", refused).unwrap();
    rt.run(
        r#"
        local connected = 0
        local db = mysqloo.connect(MY_HOST, MY_USER, MY_PASSWORD, MY_DB, tonumber(MY_PORT))
        function db:onConnected(...) connected = connected + 1 + select('#', ...) end
        db:connect()
        eq('not inside connect', connected, 0)
        eq('connected', db:status(), mysqloo.DATABASE_CONNECTED)
        db:wait()
        eq('wait delivers it once', connected, 1)
        db:wait()
        eq('only once', connected, 1)
        assert(db:serverVersion() > 50000, db:serverVersion())
        assert(#db:serverInfo() > 0)
        eq('hostInfo', db:hostInfo(), MY_HOST .. ' via TCP/IP')
        eq('ping', db:ping(), true)
        local function failure(user, password, database, port)
          local f = mysqloo.connect(MY_HOST, user, password, database, port)
          local err
          function f:onConnectionFailed(e) err = e end
          f:connect()
          f:wait()
          eq('failed', f:status(), mysqloo.DATABASE_CONNECTION_FAILED)
          return err
        end
        local wrong = failure(MY_USER, 'wrong ' .. MY_PASSWORD, MY_DB, tonumber(MY_PORT))
        assert(wrong:find("^Access denied for user '" .. MY_USER .. "'@'[^']+' %(using password: YES%)$"), wrong)
        local unknown = failure(MY_USER, MY_PASSWORD, 'fpkgtest_nowhere', tonumber(MY_PORT))
        assert(unknown == "Unknown database 'fpkgtest_nowhere'"
          or unknown:find("^Access denied for user '" .. MY_USER .. "'@'[^']+' to database 'fpkgtest_nowhere'$"), unknown)
        local f = mysqloo.connect('127.0.0.1', MY_USER, MY_PASSWORD, MY_DB, REFUSED)
        local refused
        function f:onConnectionFailed(e) refused = e end
        f:connect()
        f:wait()
        eq('refused', refused, "Can't connect to server on '127.0.0.1' (115)")
        if MY_SOCKET then
          local s = mysqloo.connect('localhost', MY_USER, MY_PASSWORD, MY_DB, 0, MY_SOCKET)
          s:connect()
          s:wait()
          eq('socket', s:status(), mysqloo.DATABASE_CONNECTED)
          eq('socket hostInfo', s:hostInfo(), 'Localhost via UNIX socket')
          eq('socket query', run(s, 'SELECT 1 AS a'):getData()[1].a, 1)
        end
        db:disconnect(true)
        eq('disconnected', db:status(), mysqloo.DATABASE_NOT_CONNECTED)
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_column_types_in_both_protocols() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        run(db, [[CREATE TEMPORARY TABLE fpkgtest_types (
          ti TINYINT, si SMALLINT UNSIGNED, mi MEDIUMINT, i INT, bi BIGINT, bu BIGINT UNSIGNED, f FLOAT, d DOUBLE,
          de DECIMAL(10,2), ff FLOAT(7,4), b1 BIT(1), b8 BIT(8), y YEAR, dt DATE, dtm DATETIME, dt3 DATETIME(3),
          tm TIME, tmn TIME, c CHAR(3), vc VARCHAR(10), tx TEXT, bl BLOB, en ENUM('a','b'), st SET('x','y'),
          js JSON, bo BOOLEAN, zf INT(5) ZEROFILL, nul INT)]])
        run(db, [[INSERT INTO fpkgtest_types VALUES (-128, 65535, -8388608, 2147483647, -9223372036854775808,
          18446744073709551615, 0.1, 0.1, 1234.5, 1.2345, b'1', b'11111111', 2024, '2024-01-31', '2024-01-31 12:34:56',
          '2024-01-31 12:34:56.789', '12:34:56', '-838:59:59', 'ab', 'v\0x', 'text', X'00FF41', 'b', 'x,y', '{"a": 1}',
          TRUE, 42, NULL)]])
        local want = {
          ti = -128, si = 65535, mi = -8388608, i = 2147483647, bi = -9223372036854775808, bu = 18446744073709551615,
          f = 0.1, d = 0.1, de = '1234.50', ff = 1.2345, b1 = 1, b8 = -1, y = '2024', dt = '2024-01-31',
          dtm = '2024-01-31 12:34:56', dt3 = '2024-01-31 12:34:56.789', tm = '12:34:56', tmn = '-838:59:59',
          c = 'ab', vc = 'v\0x', tx = 'text', bl = '\0\255A', en = 'b', st = 'x,y', bo = 1, zf = 42 }
        for _, q in ipairs({ run(db, 'SELECT * FROM fpkgtest_types'), prepared(db, 'SELECT * FROM fpkgtest_types') }) do
          local r = q:getData()[1]
          for k, v in pairs(want) do eq(k, r[k], v) end
          assert(r.js == '{"a": 1}' or r.js == '{"a":1}', r.js)
          eq('null', r.nul, nil)
          eq('fields', count(r), count(want) + 1)
        end
        local text = run(db, 'SELECT * FROM fpkgtest_types')
        eq('affected rows of a select', text:affectedRows(), 1)
        local p = prepared(db, 'SELECT * FROM fpkgtest_types')
        eq('affected rows of a prepared select, as libmariadb has them', p:affectedRows(), 18446744073709551615)
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_queries_parameters_and_results() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local p = prepared(db, 'SELECT ? AS a, ? AS b, ? AS c, ? AS d, ? AS e', 2 ^ 53 + 1, '\0\1\255', true, false, nil)
        local r = p:getData()[1]
        eq('number', r.a, 2 ^ 53) eq('string', r.b, '\0\1\255') eq('true', r.c, 1) eq('false', r.d, 0) eq('null', r.e, nil)
        local q = db:prepare('SELECT ? AS a, ? AS b')
        q:setNumber(1, 0.5)
        q:start() q:wait()
        eq('unset is NULL', q:getData()[1].b, nil)
        eq('set', q:getData()[1].a, 0.5)
        local batch = db:prepare('SELECT ? AS a')
        batch:setNumber(1, 1) batch:putNewParameters() batch:setNumber(1, 2)
        batch:start() batch:wait()
        eq('first set', batch:getData()[1].a, 1)
        batch:getNextResults()
        eq('second set', batch:getData()[1].a, 2)
        batch:getNextResults()
        eq('no more', batch:hasMoreResults(), false)

        run(db, 'CREATE TEMPORARY TABLE fpkgtest_ids (id INT AUTO_INCREMENT PRIMARY KEY, v INT)')
        local ins = run(db, 'INSERT INTO fpkgtest_ids (v) VALUES (1), (2)')
        eq('lastInsert', ins:lastInsert(), 1) eq('affectedRows', ins:affectedRows(), 2)
        eq('a select keeps the insert id', run(db, 'SELECT * FROM fpkgtest_ids'):lastInsert(), 1)
        local pins = prepared(db, 'INSERT INTO fpkgtest_ids (v) VALUES (?)', 3)
        eq('prepared lastInsert', pins:lastInsert(), 3) eq('prepared affectedRows', pins:affectedRows(), 1)
        eq('update', run(db, 'UPDATE fpkgtest_ids SET v = 9 WHERE v < 3'):affectedRows(), 2)
        local multi = run(db, 'INSERT INTO fpkgtest_ids (v) VALUES (4); SELECT v FROM fpkgtest_ids WHERE v = 4; SELECT 1 AS one')
        eq('multi 1', multi:lastInsert(), 4) eq('more', multi:hasMoreResults(), true)
        multi:getNextResults()
        eq('multi 2', multi:getData()[1].v, 4) eq('multi 2 affected', multi:affectedRows(), 1)
        multi:getNextResults()
        eq('multi 3', multi:getData()[1].one, 1)
        multi:getNextResults()
        eq('multi done', multi:hasMoreResults(), false)

        run(db, 'DROP PROCEDURE IF EXISTS fpkgtest_proc')
        run(db, 'CREATE PROCEDURE fpkgtest_proc (IN p INT) BEGIN SELECT p AS a; SELECT 999 AS b; END')
        for _, call in ipairs({ run(db, 'CALL fpkgtest_proc(5)'), prepared(db, 'CALL fpkgtest_proc(?)', 5) }) do
          eq('call a', call:getData()[1].a, 5)
          call:getNextResults()
          eq('call b', call:getData()[1].b, 999)
          call:getNextResults()
          eq('the result of the call', call:hasMoreResults(), true)
          call:getNextResults()
          eq('call done', call:hasMoreResults(), false)
        end
        run(db, 'DROP PROCEDURE fpkgtest_proc')

        local order = {}
        local o = db:query('SELECT 1 AS a UNION ALL SELECT 2')
        function o:onData(row) order[#order + 1] = row.a end
        function o:onSuccess(data) order[#order + 1] = #data end
        o:start()
        eq('not inside start', #order, 0)
        o:wait()
        eq('onData, then onSuccess', table.concat(order, ','), '1,2,2')
        local async = db:query('SELECT 3 AS a')
        local got
        function async:onSuccess(data) got = data[1].a end
        async:start()
        eq('not yet', got, nil)
        hook.Run('Think')
        eq('Think delivers', got, 3)

        local tr = db:createTransaction()
        run(db, 'CREATE TEMPORARY TABLE fpkgtest_tr (id INT)')
        tr:addQuery(db:query('INSERT INTO fpkgtest_tr VALUES (1)'))
        tr:addQuery(db:query('gibberish'))
        local trError
        function tr:onError(err) trError = err end
        tr:start() tr:wait()
        assert(trError:find('gibberish', 1, true), trError)
        eq('rolled back', run(db, 'SELECT COUNT(*) AS n FROM fpkgtest_tr'):getData()[1].n, 0)
        local tr2 = db:createTransaction()
        tr2:addQuery(db:query('INSERT INTO fpkgtest_tr VALUES (2)'))
        tr2:addQuery(db:prepare('SELECT COUNT(*) AS n FROM fpkgtest_tr'))
        local trData
        function tr2:onSuccess(data) trData = data end
        tr2:start() tr2:wait()
        eq('committed', trData[2][1].n, 1)
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_errors_waits_and_disconnecting() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        local q = db:query('SELECT nope FROM fpkgtest_nowhere')
        local args
        function q:onError(...) args = { n = select('#', ...), ... } end
        q:start()
        eq('no error before Think', q:error(), '')
        q:wait()
        eq('onError gets the error and the query', args.n, 2)
        assert(args[1]:find("fpkgtest_nowhere' doesn't exist", 1, true), args[1])
        eq('sql', args[2], 'SELECT nope FROM fpkgtest_nowhere')
        eq('q:error()', q:error(), args[1])
        eq('no rows', q:getData(), nil)
        local p = db:prepare('SELEC 1')
        local perr
        function p:onError(err) perr = err end
        p:start() p:wait()
        assert(perr:find('SQL syntax', 1, true), perr)
        local two = db:prepare('SELECT 1; SELECT 2')
        two:start() two:wait()
        assert(two:error():find('SQL syntax', 1, true), two:error())

        local single = connect(function(d) d:setMultiStatements(false) end)
        local s = single:query('SELECT 1; SELECT 2')
        s:start() s:wait()
        assert(s:error():find('SQL syntax', 1, true), s:error())

        local victim = connect()
        local id = run(victim, 'SELECT CONNECTION_ID() AS id'):getData()[1].id
        run(db, 'KILL ' .. string.format('%d', id))
        eq('a lost connection is made again', run(victim, 'SELECT 5 AS a'):getData()[1].a, 5)
        local kept = connect(function(d) d:setAutoReconnect(false) end)
        run(db, 'KILL ' .. string.format('%d', run(kept, 'SELECT CONNECTION_ID() AS id'):getData()[1].id))
        local lost = kept:query('SELECT 1')
        lost:start() lost:wait()
        eq('without autoreconnect', lost:error(), 'Lost connection to server during query')
        eq('ping connects again', kept:ping(), true)
        eq('and it works', run(kept, 'SELECT 6 AS a'):getData()[1].a, 6)

        local before = db:query('SELECT 1 AS a')
        local ran
        function before:onSuccess() ran = true end
        before:start()
        db:disconnect(true)
        eq('disconnect(true) delivers', ran, true)
        eq('status', db:status(), mysqloo.DATABASE_NOT_CONNECTED)
        eq('escape', message(db.escape, db, 'x'), 'Cannot escape using database that is not connected')
        local after = db:query('SELECT 1')
        local afterError
        function after:onError(err) afterError = err end
        after:start()
        eq('queued after disconnect', db:queueSize(), 1)
        after:wait()
        eq('wait after disconnect', afterError, 'Can not wait on query, database is not connected or connection failed.')
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_character_sets_and_escaping() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        eq('the default', run(db, 'SELECT @@character_set_client AS c'):getData()[1].c, 'utf8mb4')
        local ok, second = db:setCharacterSet('utf8mb4')
        eq('setCharacterSet', ok, true) eq('and an empty string', second, '')
        eq('an unknown one', (db:setCharacterSet('fpkg_nothing')), false)
        run(db, 'CREATE TEMPORARY TABLE fpkgtest_text (s VARCHAR(20)) DEFAULT CHARSET=utf8mb4')
        local emoji = '😀 é'
        prepared(db, 'INSERT INTO fpkgtest_text VALUES (?)', emoji)
        run(db, "INSERT INTO fpkgtest_text VALUES ('" .. db:escape("it's " .. emoji) .. "')")
        local rows = run(db, 'SELECT s, CHAR_LENGTH(s) AS n FROM fpkgtest_text ORDER BY n'):getData()
        eq('4-byte UTF-8 back', rows[1].s, emoji) eq('as one character', rows[1].n, 3)
        eq('escaped', rows[2].s, "it's " .. emoji)
        eq('prepared too', prepared(db, 'SELECT s FROM fpkgtest_text WHERE s = ?', emoji):getData()[1].s, emoji)
        db:setCharacterSet('latin1')
        eq('latin1', run(db, 'SELECT @@character_set_client AS c'):getData()[1].c, 'latin1')
        run(db, "SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'")
        eq('no backslash escapes', db:escape("it's \\"), "it''s \\")
        run(db, 'SET SESSION sql_mode = DEFAULT')
        eq('backslashes', db:escape("it's \\"), "it\\'s \\\\")
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db and FPKG_TEST_MYSQL_CA=<the PEM of its CA or of its own certificate>"]
fn server_tls() {
    std::env::var("FPKG_TEST_MYSQL_CA").expect(
        "server_tls needs FPKG_TEST_MYSQL_CA=<the PEM of the server's CA, or of its certificate when it is its own>",
    );
    let rt = server();
    rt.run(
        r#"
        local db = connect(function(d) d:setSSLSettings(nil, nil, MY_CA) end)
        local cipher = run(db, "SHOW SESSION STATUS LIKE 'Ssl_cipher'"):getData()[1]
        assert(cipher and (cipher.Value or cipher.VALUE or '') ~= '', 'not encrypted')
        local plain = connect()
        local none = run(plain, "SHOW SESSION STATUS LIKE 'Ssl_cipher'"):getData()[1]
        eq('without settings there is no TLS', none.Value or none.VALUE, '')
        local wrong = mysqloo.connect(MY_HOST, MY_USER, MY_PASSWORD, MY_DB, tonumber(MY_PORT))
        wrong:setSSLSettings(nil, nil, '/nonexistent/ca.pem')
        local err
        function wrong:onConnectionFailed(e) err = e end
        wrong:connect()
        wrong:wait()
        assert(err:find('^SSL connection error: '), err)
        "#,
    );
}

#[test]
#[ignore = "needs a server: FPKG_TEST_MYSQL=host:port:user:password:db"]
fn server_out_and_inout_parameters_of_a_prepared_call() {
    let rt = server();
    rt.run(
        r#"
        local db = connect()
        run(db, 'DROP PROCEDURE IF EXISTS fpkgtest_out')
        run(db, [[CREATE PROCEDURE fpkgtest_out (IN a INT, OUT b INT, INOUT c VARCHAR(10))
          BEGIN SET b = a * 2; SET c = CONCAT(c, '!'); END]])
        for round = 1, 2 do
          local q = db:prepare('CALL fpkgtest_out(?, ?, ?)')
          q:setNumber(1, 4)
          q:setString(3, 'yo')
          q:start()
          q:wait()
          eq('no error', q:error(), '')
          local out = q:getData()[1]
          eq('OUT', out.b, 8) eq('INOUT', out.c, 'yo!')
          eq('more after the parameters', q:hasMoreResults(), true)
          q:getNextResults()
          eq('the result of the call', count(q:getData()), 0)
          q:getNextResults()
          eq('nothing after it', q:hasMoreResults(), false)
          eq('a query after it ' .. round, run(db, 'SELECT 7 AS after'):getData()[1].after, 7)
          eq('a prepared one after it ' .. round, prepared(db, 'SELECT ? AS p', 3):getData()[1].p, 3)
          eq('the affected rows of a delete', run(db, 'DO 1'):affectedRows(), 0)
        end
        run(db, 'DROP PROCEDURE fpkgtest_out')
        "#,
    );
}

/// A server that speaks just enough of the protocol to script what a test needs, on a port of
/// its own, plain or with TLS on certificates that each test makes with rcgen.
mod fake {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;

    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    /// Runs `script` on the first connection to a new port, in a thread of its own.
    pub(super) fn serve(script: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                script(stream);
            }
        });
        port
    }

    pub(super) fn send(s: &mut impl Write, seq: u8, payload: &[u8]) {
        let mut p = (payload.len() as u32).to_le_bytes()[..3].to_vec();
        p.push(seq);
        p.extend_from_slice(payload);
        let _ = s.write_all(&p);
        let _ = s.flush();
    }

    /// The next packet, or nothing once the client has gone.
    pub(super) fn recv(s: &mut impl Read) -> Option<Vec<u8>> {
        let mut head = [0u8; 4];
        s.read_exact(&mut head).ok()?;
        let len = u32::from_le_bytes([head[0], head[1], head[2], 0]) as usize;
        let mut p = vec![0u8; len];
        s.read_exact(&mut p).ok()?;
        Some(p)
    }

    /// The first packet of a server, which offers `plugin`, and TLS with `tls`.
    pub(super) fn greeting(plugin: &str, tls: bool) -> Vec<u8> {
        let caps: u32 = 1
            | 4
            | 8
            | 512
            | 8192
            | 32768
            | (1 << 16)
            | (1 << 17)
            | (1 << 18)
            | (1 << 19)
            | (1 << 21)
            | if tls { 2048 } else { 0 };
        let mut p = vec![10];
        p.extend_from_slice(b"8.4.0-fake\0");
        p.extend_from_slice(&7u32.to_le_bytes());
        p.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        p.push(0);
        p.extend_from_slice(&(caps as u16).to_le_bytes());
        p.push(45);
        p.extend_from_slice(&2u16.to_le_bytes());
        p.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        p.push(21);
        p.extend_from_slice(&[0; 10]);
        p.extend_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 0]);
        p.extend_from_slice(plugin.as_bytes());
        p.push(0);
        p
    }

    pub(super) const OK: &[u8] = &[0, 0, 0, 2, 0, 0, 0];

    /// The greeting, the client's answer and an OK packet: a connection that is ready.
    pub(super) fn greet(s: &mut TcpStream) {
        send(s, 0, &greeting("mysql_native_password", false));
        recv(s);
        send(s, 2, OK);
    }

    pub(super) fn eof(status: u16) -> Vec<u8> {
        let mut p = vec![0xfe, 0, 0];
        p.extend_from_slice(&status.to_le_bytes());
        p
    }

    /// The definition of a column of a type.
    pub(super) fn column(name: &str, kind: u8) -> Vec<u8> {
        let mut p = Vec::new();
        for part in ["def", "", "", ""] {
            p.push(part.len() as u8);
            p.extend_from_slice(part.as_bytes());
        }
        p.push(name.len() as u8);
        p.extend_from_slice(name.as_bytes());
        p.push(0);
        p.push(0x0c);
        p.extend_from_slice(&63u16.to_le_bytes());
        p.extend_from_slice(&20u32.to_le_bytes());
        p.push(kind);
        p.extend_from_slice(&0u16.to_le_bytes());
        p.push(0);
        p.extend_from_slice(&[0, 0]);
        p
    }

    fn der(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let n = content.len();
        if n < 0x80 {
            out.push(n as u8);
        } else {
            let bytes: Vec<u8> = n
                .to_be_bytes()
                .into_iter()
                .skip_while(|b| *b == 0)
                .collect();
            out.push(0x80 | bytes.len() as u8);
            out.extend(bytes);
        }
        out.extend_from_slice(content);
        out
    }

    /// The PEM of an RSA public key (SubjectPublicKeyInfo) with this modulus and exponent.
    pub(super) fn public_key_pem(n: &[u8], e: &[u8]) -> Vec<u8> {
        let int = |v: &[u8]| {
            let mut c = Vec::new();
            if v.first().is_some_and(|b| *b & 0x80 != 0) {
                c.push(0);
            }
            c.extend_from_slice(v);
            der(0x02, &c)
        };
        let key = der(0x30, &[int(n), int(e)].concat());
        let mut bits = vec![0];
        bits.extend(key);
        let algorithm = der(
            0x30,
            &[
                der(
                    0x06,
                    &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01],
                ),
                vec![0x05, 0x00],
            ]
            .concat(),
        );
        let spki = der(0x30, &[algorithm, der(0x03, &bits)].concat());
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(spki);
        let mut pem = b"-----BEGIN PUBLIC KEY-----\n".to_vec();
        for line in b64.as_bytes().chunks(64) {
            pem.extend_from_slice(line);
            pem.push(b'\n');
        }
        pem.extend_from_slice(b"-----END PUBLIC KEY-----\n");
        pem
    }

    /// A certificate that is its own authority, as `openssl req -x509` makes one: its PEM, and
    /// the TLS configuration of a server that presents it (and asks for a client certificate
    /// signed by it when `client_auth`).
    pub(super) fn self_signed(client_auth: bool) -> (String, Arc<rustls::ServerConfig>) {
        let mut p = CertificateParams::new(vec!["fpkgtest.example".to_string()]).unwrap();
        p.distinguished_name
            .push(DnType::CommonName, "fpkgtest.example");
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let cert = p.self_signed(&key).unwrap();
        let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(rustls::ALL_VERSIONS)
            .unwrap();
        let builder = if client_auth {
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(CertificateDer::from(cert.der().to_vec()))
                .unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                provider,
            )
            .build()
            .unwrap();
            builder.with_client_cert_verifier(verifier)
        } else {
            builder.with_no_client_auth()
        };
        let config = builder
            .with_single_cert(vec![cert.der().clone()], der)
            .unwrap();
        (cert.pem(), Arc::new(config))
    }

    /// A greeting with TLS, TLS once the client asks for it, then the client's answer and an
    /// OK packet over it; what the client should see.
    pub(super) fn greet_tls(mut s: TcpStream, config: Arc<rustls::ServerConfig>) {
        send(&mut s, 0, &greeting("mysql_native_password", true));
        if recv(&mut s).is_none() {
            return;
        }
        let Ok(conn) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut t = rustls::StreamOwned::new(conn, s);
        if recv(&mut t).is_some() {
            send(&mut t, 3, OK);
            recv(&mut t);
        }
    }
}

fn fake_settings(port: u16) -> wire::Settings {
    wire::Settings {
        host: b"127.0.0.1".to_vec(),
        user: b"u".to_vec(),
        password: b"p".to_vec(),
        database: b"d".to_vec(),
        port: port as u32,
        multi_statements: true,
        connect_timeout: std::time::Duration::from_secs(5),
        ..wire::Settings::default()
    }
}

#[test]
fn counts_from_the_wire_size_nothing_and_a_bad_packet_breaks_the_connection() {
    let short_row: Vec<Vec<u8>> = vec![
        vec![1],
        fake::column("a", 253),
        fake::eof(2),
        vec![5, b'a', b'b'],
        fake::eof(2),
    ];
    let headers: Vec<Vec<Vec<u8>>> = vec![
        vec![vec![0xfe, 0, 0, 0, 0, 0, 1, 0, 0], vec![1, 2, 3]],
        vec![vec![0xfe, 0, 0, 0, 0, 0, 0, 0, 0x40], vec![1, 2, 3]],
        vec![vec![0xfd, 0, 0, 1], vec![1, 2, 3]],
        short_row,
    ];
    for packets in headers {
        let port = fake::serve(move |mut s| {
            fake::greet(&mut s);
            fake::recv(&mut s);
            for (i, p) in packets.iter().enumerate() {
                fake::send(&mut s, 1 + i as u8, p);
            }
            fake::recv(&mut s);
        });
        let mut c = wire::Conn::connect(&fake_settings(port)).unwrap();
        let e = c.query(b"SELECT 1", &mut Vec::new()).unwrap_err();
        assert_eq!(e.code, wire::CR_MALFORMED_PACKET, "{e:?}");
        let e = c.query(b"SELECT 2", &mut Vec::new()).unwrap_err();
        assert_eq!(e.code, wire::CR_SERVER_GONE_ERROR, "{e:?}");
    }
}

#[test]
fn packets_are_put_together_up_to_the_largest_allowed() {
    let port = fake::serve(|mut s| {
        fake::greet(&mut s);
        fake::recv(&mut s);
        let chunk = vec![b'x'; 0xff_ffff];
        fake::send(&mut s, 1, &chunk);
        fake::send(&mut s, 2, &chunk);
        fake::send(&mut s, 3, &[]);
        fake::recv(&mut s);
    });
    let mut c = wire::Conn::connect(&fake_settings(port)).unwrap();
    c.set_max_packet(20 << 20);
    let e = c.query(b"SELECT 1", &mut Vec::new()).unwrap_err();
    assert_eq!(e.code, wire::CR_NET_PACKET_TOO_LARGE);
    assert_eq!(e.message, b"Got packet bigger than 'max_allowed_packet'");
    let e = c.query(b"SELECT 2", &mut Vec::new()).unwrap_err();
    assert_eq!(e.code, wire::CR_SERVER_GONE_ERROR);
    assert_eq!(wire::MAX_ALLOWED_PACKET, 1 << 30);

    let port = fake::serve(|mut s| {
        fake::greet(&mut s);
        fake::recv(&mut s);
    });
    let mut c = wire::Conn::connect(&fake_settings(port)).unwrap();
    c.set_max_packet(100);
    let e = c.query(&[b'x'; 200], &mut Vec::new()).unwrap_err();
    assert_eq!(e.code, wire::CR_NET_PACKET_TOO_LARGE);
}

#[test]
fn keys_of_the_server_are_bounded_before_any_arithmetic() {
    let odd = |bytes: usize| {
        let mut n = vec![0xc3; bytes];
        n[bytes - 1] = 0x01;
        n
    };
    let cases: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (odd(2049), vec![1, 0, 1]),
        (odd(8192), odd(8192)),
        (odd(256), vec![1; 9]),
        (vec![0xc2; 256], vec![1, 0, 1]),
        (odd(256), vec![1]),
        (odd(256), vec![0]),
    ];
    for (n, e) in cases {
        assert!(!auth::usable_key(&n, &e));
        let pem = fake::public_key_pem(&n, &e);
        assert_eq!(auth::public_key(&pem), None);
        let port = fake::serve(move |mut s| {
            fake::send(&mut s, 0, &fake::greeting("caching_sha2_password", false));
            fake::recv(&mut s);
            fake::send(&mut s, 2, &[0x01, 0x04]);
            fake::recv(&mut s);
            let mut p = vec![0x01];
            p.extend_from_slice(&pem);
            fake::send(&mut s, 4, &p);
            fake::recv(&mut s);
        });
        let started = std::time::Instant::now();
        let e = wire::Conn::connect(&fake_settings(port)).err().unwrap();
        assert_eq!(e.message, b"Couldn't read RSA public key from server");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
    assert!(auth::usable_key(&odd(2048), &[1, 0, 1]));
    assert!(auth::public_key(&[b'-'; 20000]).is_none());
}

#[test]
fn the_out_parameters_of_a_call_are_followed_by_one_more_result() {
    let port = fake::serve(|mut s| {
        fake::greet(&mut s);
        fake::recv(&mut s);
        let mut prepared = vec![0x00];
        prepared.extend_from_slice(&1u32.to_le_bytes());
        prepared.extend_from_slice(&0u16.to_le_bytes());
        prepared.extend_from_slice(&3u16.to_le_bytes());
        prepared.extend_from_slice(&[0, 0, 0]);
        fake::send(&mut s, 1, &prepared);
        for i in 0..3u8 {
            fake::send(&mut s, 2 + i, &fake::column("?", 253));
        }
        fake::send(&mut s, 5, &fake::eof(2));
        fake::recv(&mut s);
        fake::send(&mut s, 1, &[2]);
        fake::send(&mut s, 2, &fake::column("b", 8));
        fake::send(&mut s, 3, &fake::column("c", 253));
        fake::send(&mut s, 4, &fake::eof(0x100a));
        let mut row = vec![0x00, 0x00];
        row.extend_from_slice(&8i64.to_le_bytes());
        row.extend_from_slice(b"\x03yo!");
        fake::send(&mut s, 5, &row);
        fake::send(&mut s, 6, &fake::eof(0x0002));
        fake::send(&mut s, 7, &[0, 0, 0, 2, 0, 0, 0]);
        fake::recv(&mut s);
        fake::send(&mut s, 1, &[1]);
        fake::send(&mut s, 2, &fake::column("after", 8));
        fake::send(&mut s, 3, &fake::eof(2));
        fake::send(&mut s, 4, b"\x017");
        fake::send(&mut s, 5, &fake::eof(2));
        fake::recv(&mut s);
    });
    let mut c = wire::Conn::connect(&fake_settings(port)).unwrap();
    let st = c.prepare(b"CALL p(?, ?, ?)").unwrap();
    let mut r = Vec::new();
    c.execute(
        &st,
        &[
            wire::Param::Double(4.0),
            wire::Param::Null,
            wire::Param::Text(b"yo".to_vec()),
        ],
        &mut r,
    )
    .unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(
        r[0].set.rows,
        vec![vec![Some(b"8".to_vec()), Some(b"yo!".to_vec())]]
    );
    assert!(r[1].set.columns.is_empty());
    let mut after = Vec::new();
    c.query(b"SELECT 7 AS after", &mut after).unwrap();
    assert_eq!(after[0].set.rows, vec![vec![Some(b"7".to_vec())]]);
}

#[test]
fn the_connect_has_one_deadline() {
    let port = fake::serve(|mut s| {
        use std::io::Write;
        let mut packet = Vec::new();
        let greeting = fake::greeting("mysql_native_password", false);
        packet.extend_from_slice(&(greeting.len() as u32).to_le_bytes()[..3]);
        packet.push(0);
        packet.extend(greeting);
        for b in packet {
            if s.write_all(&[b]).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    });
    let mut settings = fake_settings(port);
    settings.connect_timeout = std::time::Duration::from_millis(1500);
    let started = std::time::Instant::now();
    let e = wire::Conn::connect(&settings).err().unwrap();
    let took = started.elapsed();
    assert!(
        took >= std::time::Duration::from_millis(1400) && took < std::time::Duration::from_secs(3),
        "{took:?}"
    );
    assert_eq!(e.code, wire::CR_SERVER_LOST_EXTENDED, "{e:?}");
    assert_eq!(
        String::from_utf8_lossy(&e.message),
        "Lost connection to server at 'handshake: reading initial communication packet': the connect timed out"
    );
}

#[test]
fn tls_takes_a_server_certificate_that_is_its_own_authority() {
    let dir = tempfile::tempdir().unwrap();
    let (pem, config) = fake::self_signed(false);
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, &pem).unwrap();
    let tls_settings = |ca: &std::path::Path| super::tls::Settings {
        ca: Some(ca.to_path_buf()),
        ..super::tls::Settings::default()
    };
    let port = fake::serve(move |s| fake::greet_tls(s, config));
    let mut settings = fake_settings(port);
    settings.tls = Some(tls_settings(&ca));
    wire::Conn::connect(&settings).unwrap();

    let (other, _) = fake::self_signed(false);
    let wrong = dir.path().join("other.pem");
    std::fs::write(&wrong, &other).unwrap();
    let (_, config) = fake::self_signed(false);
    let port = fake::serve(move |s| fake::greet_tls(s, config));
    let mut settings = fake_settings(port);
    settings.tls = Some(tls_settings(&wrong));
    let e = wire::Conn::connect(&settings).err().unwrap();
    assert_eq!(e.code, wire::CR_SSL_CONNECTION_ERROR);
    assert!(
        String::from_utf8_lossy(&e.message)
            .starts_with("SSL connection error: invalid peer certificate"),
        "{e:?}"
    );

    let (_, config) = fake::self_signed(true);
    let port = fake::serve(move |s| fake::greet_tls(s, config));
    let mut settings = fake_settings(port);
    settings.tls = Some(super::tls::Settings::default());
    settings.tls.as_mut().unwrap().key = Some(dir.path().join("none.key"));
    let e = wire::Conn::connect(&settings).err().unwrap();
    assert_eq!(e.code, wire::CR_SSL_CONNECTION_ERROR, "{e:?}");
    assert!(
        String::from_utf8_lossy(&e.message)
            .starts_with("SSL connection error: received fatal alert"),
        "{e:?}"
    );
}
