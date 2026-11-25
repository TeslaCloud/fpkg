//! The engine half of the game's `sql` library on SQLite: `sql.Query` and `sql.QueryTyped`,
//! over `garrysmod/sv.db`, opened at the first query with foreign keys off, as the game's
//! SQLite has them.
//!
//! The text of a query goes to SQLite byte for byte and text comes back byte for byte, so
//! bytes that are not UTF-8 are stored and read unchanged, as on the server.
//! `sql.Query` runs every statement of its text and returns the rows of all of them as
//! tables of strings (a NULL is the string `"NULL"`, a number is written as SQLite writes it
//! as text), `nil` when there are none, `false` on an error, whose text goes to
//! `sql.m_strError` (what `sql.LastError` returns). `sql.QueryTyped` runs one statement with
//! `?` parameters (nil, booleans as 1 and 0, numbers, strings) and returns typed rows,
//! without the NULL columns, an empty table when there are none; integers beyond 2^53 come
//! back as strings.
//!
//! The bundled SQLite is newer than the game's 3.26.0: SQL that needs a newer one
//! (`DROP COLUMN`, `RETURNING`, `STRICT` tables, `->>`) runs here and fails on the server.

use std::ffi::{CStr, c_char, c_int};
use std::rc::Rc;

use nl::mlua::{self, Lua, MultiValue, Table, Value};
use rusqlite::{Connection, ffi};

use crate::runtime::{State, as_bytes, bad_argument, state, type_name, until_nul};

/// The largest integer a Lua number holds exactly.
const EXACT: u64 = 9_007_199_254_740_991;

/// What a failed query says, as SQLite wrote it.
type SqlError = Vec<u8>;

fn with_db<T>(
    st: &Rc<State>,
    f: impl FnOnce(&Connection) -> Result<T, SqlError>,
) -> Result<T, SqlError> {
    let mut db = st.db.borrow_mut();
    if db.is_none() {
        let path = st.tree.database();
        let text = |e: &dyn std::fmt::Display| e.to_string().into_bytes();
        st.tree.prepare_for(&path).map_err(|e| text(&e))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| text(&e))?;
        }
        let c = Connection::open(&path).map_err(|e| text(&e))?;
        st.tree.changed();
        c.execute_batch("PRAGMA foreign_keys = OFF")
            .map_err(|e| text(&e))?;
        *db = Some(c);
    }
    f(db.as_ref().expect("database"))
}

fn set_error(lua: &Lua, msg: &[u8]) -> mlua::Result<()> {
    let sql: Table = lua.globals().get("sql")?;
    sql.raw_set("m_strError", lua.create_string(msg)?)
}

/// A prepared statement, finalized when it is dropped.
struct Statement {
    db: *mut ffi::sqlite3,
    stmt: *mut ffi::sqlite3_stmt,
}

impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: the statement came from sqlite3_prepare_v2 and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.stmt);
        }
    }
}

/// The bytes of a C string SQLite owns; empty for a null pointer.
///
/// SAFETY: `p` is null or a NUL-terminated string that lives while the result is used.
unsafe fn c_bytes<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { CStr::from_ptr(p).to_bytes() }
}

/// SQLite's message for the last error on `db`.
fn last_error(db: *mut ffi::sqlite3) -> SqlError {
    // SAFETY: the connection is open; the message is copied before the next call.
    unsafe { c_bytes(ffi::sqlite3_errmsg(db)).to_vec() }
}

/// The first statement of `sql` and the text after it; no statement when the text holds
/// only blanks and comments.
fn prepare<'a>(db: &Connection, sql: &'a [u8]) -> Result<(Option<Statement>, &'a [u8]), SqlError> {
    let len = c_int::try_from(sql.len()).map_err(|_| b"the query is too long".to_vec())?;
    // SAFETY: the handle stays open while `db` lives; SQLite reads `len` bytes of `sql` and
    // points `tail` into them.
    unsafe {
        let handle = db.handle();
        let mut stmt = std::ptr::null_mut();
        let mut tail: *const c_char = std::ptr::null();
        let rc = ffi::sqlite3_prepare_v2(handle, sql.as_ptr().cast(), len, &mut stmt, &mut tail);
        if rc != ffi::SQLITE_OK {
            return Err(last_error(handle));
        }
        let used = if tail.is_null() {
            sql.len()
        } else {
            (tail as usize - sql.as_ptr() as usize).min(sql.len())
        };
        let stmt = (!stmt.is_null()).then_some(Statement { db: handle, stmt });
        Ok((stmt, &sql[used..]))
    }
}

/// A value of a row, as `QueryTyped` gives it.
enum Typed {
    Int(i64),
    Real(f64),
    Bytes(Vec<u8>),
}

/// A row: column name and value, NULL columns left out by `QueryTyped`.
type Row<V> = Vec<(Vec<u8>, V)>;

impl Statement {
    fn names(&self) -> Vec<Vec<u8>> {
        // SAFETY: the statement is prepared; the names are copied at once.
        unsafe {
            (0..ffi::sqlite3_column_count(self.stmt))
                .map(|i| c_bytes(ffi::sqlite3_column_name(self.stmt, i)).to_vec())
                .collect()
        }
    }

    /// The bytes of a column of the current row: its text, or a blob as it is.
    fn bytes(&self, i: c_int, blob: bool) -> Vec<u8> {
        // SAFETY: a row is current; the pointer is valid until the next step and is read
        // for the length SQLite gives right after it.
        unsafe {
            let p: *const u8 = if blob {
                ffi::sqlite3_column_blob(self.stmt, i).cast()
            } else {
                ffi::sqlite3_column_text(self.stmt, i).cast()
            };
            let n = ffi::sqlite3_column_bytes(self.stmt, i);
            if p.is_null() || n <= 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(p, n as usize).to_vec()
        }
    }

    /// Steps through the rows, giving each one to `row`.
    fn each_row(&self, mut row: impl FnMut(&Statement)) -> Result<(), SqlError> {
        loop {
            // SAFETY: the statement is prepared and its parameters bound.
            match unsafe { ffi::sqlite3_step(self.stmt) } {
                ffi::SQLITE_ROW => row(self),
                ffi::SQLITE_DONE => return Ok(()),
                _ => return Err(last_error(self.db)),
            }
        }
    }

    fn column_type(&self, i: c_int) -> c_int {
        // SAFETY: a row is current.
        unsafe { ffi::sqlite3_column_type(self.stmt, i) }
    }

    /// `sql.Query`'s rows: every column as SQLite writes it as text, up to a NUL.
    fn text_rows(&self, rows: &mut Vec<Row<Vec<u8>>>) -> Result<(), SqlError> {
        let names = self.names();
        self.each_row(|s| {
            let row = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let i = i as c_int;
                    let text = if s.column_type(i) == ffi::SQLITE_NULL {
                        b"NULL".to_vec()
                    } else {
                        until_nul(&s.bytes(i, false)).to_vec()
                    };
                    (name.clone(), text)
                })
                .collect();
            rows.push(row);
        })
    }

    /// `sql.QueryTyped`'s rows.
    fn typed_rows(&self) -> Result<Vec<Row<Typed>>, SqlError> {
        let names = self.names();
        let mut rows = Vec::new();
        self.each_row(|s| {
            let mut row = Vec::new();
            for (i, name) in names.iter().enumerate() {
                let i = i as c_int;
                // SAFETY: a row is current.
                let v = match s.column_type(i) {
                    ffi::SQLITE_NULL => continue,
                    ffi::SQLITE_INTEGER => {
                        let n = unsafe { ffi::sqlite3_column_int64(s.stmt, i) };
                        if n.unsigned_abs() > EXACT {
                            Typed::Bytes(n.to_string().into_bytes())
                        } else {
                            Typed::Int(n)
                        }
                    }
                    ffi::SQLITE_FLOAT => {
                        Typed::Real(unsafe { ffi::sqlite3_column_double(s.stmt, i) })
                    }
                    ffi::SQLITE_BLOB => Typed::Bytes(s.bytes(i, true)),
                    _ => Typed::Bytes(s.bytes(i, false)),
                };
                row.push((name.clone(), v));
            }
            rows.push(row);
        })?;
        Ok(rows)
    }

    fn bind(&self, params: &[Param]) -> Result<(), SqlError> {
        // SAFETY: SQLite copies bound text (SQLITE_TRANSIENT) before returning.
        unsafe {
            let expected = ffi::sqlite3_bind_parameter_count(self.stmt);
            if expected as usize != params.len() {
                return Err(format!(
                    "the query has {expected} parameter(s), {} given",
                    params.len()
                )
                .into_bytes());
            }
            for (i, p) in params.iter().enumerate() {
                let i = i as c_int + 1;
                let rc = match p {
                    Param::Null => ffi::sqlite3_bind_null(self.stmt, i),
                    Param::Int(n) => ffi::sqlite3_bind_int64(self.stmt, i, *n),
                    Param::Real(f) => ffi::sqlite3_bind_double(self.stmt, i, *f),
                    Param::Text(t) => {
                        let len = c_int::try_from(t.len())
                            .map_err(|_| b"a parameter is too long".to_vec())?;
                        ffi::sqlite3_bind_text(
                            self.stmt,
                            i,
                            t.as_ptr().cast(),
                            len,
                            ffi::SQLITE_TRANSIENT(),
                        )
                    }
                };
                if rc != ffi::SQLITE_OK {
                    return Err(last_error(self.db));
                }
            }
        }
        Ok(())
    }
}

/// Every statement of the text, with the rows of all of them.
fn query(db: &Connection, text: &[u8]) -> Result<Vec<Row<Vec<u8>>>, SqlError> {
    let mut rows = Vec::new();
    let mut rest = text;
    loop {
        let (stmt, tail) = prepare(db, rest)?;
        match stmt {
            Some(s) => s.text_rows(&mut rows)?,
            None if tail.len() == rest.len() || tail.is_empty() => return Ok(rows),
            None => {}
        }
        if tail.is_empty() {
            return Ok(rows);
        }
        rest = tail;
    }
}

/// The first statement of the text, with its parameters.
fn query_typed(
    db: &Connection,
    text: &[u8],
    params: &[Param],
) -> Result<Vec<Row<Typed>>, SqlError> {
    let (Some(stmt), _) = prepare(db, text)? else {
        return Ok(Vec::new());
    };
    stmt.bind(params)?;
    stmt.typed_rows()
}

/// A `QueryTyped` parameter.
enum Param {
    Null,
    Int(i64),
    Real(f64),
    Text(Vec<u8>),
}

fn param(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Param> {
    Ok(match &args[n - 1] {
        Value::Nil => Param::Null,
        Value::Boolean(b) => Param::Int(i64::from(*b)),
        Value::Integer(i) => Param::Int(*i),
        Value::Number(f) if *f == f.trunc() && f.abs() <= EXACT as f64 => Param::Int(*f as i64),
        Value::Number(f) => Param::Real(*f),
        v @ Value::String(_) => Param::Text(as_bytes(v).unwrap_or_default()),
        other => {
            return Err(crate::runtime::arg_error(
                lua,
                n,
                &format!(
                    "nil, boolean, number or string expected, got {}",
                    type_name(other)
                ),
            ));
        }
    })
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let sql = lua.create_table()?;
    sql.set(
        "Query",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let Some(text) = args.front().and_then(as_bytes) else {
                return Err(bad_argument(lua, &args, 1, "string"));
            };
            let st = state(lua);
            match with_db(&st, |db| query(db, &text)) {
                Ok(rows) if rows.is_empty() => Ok(Value::Nil),
                Ok(rows) => {
                    let t = lua.create_table_with_capacity(rows.len(), 0)?;
                    for (i, row) in rows.into_iter().enumerate() {
                        let r = lua.create_table()?;
                        for (name, text) in row {
                            r.raw_set(lua.create_string(name)?, lua.create_string(text)?)?;
                        }
                        t.raw_set(i + 1, r)?;
                    }
                    Ok(Value::Table(t))
                }
                Err(e) => {
                    set_error(lua, &e)?;
                    Ok(Value::Boolean(false))
                }
            }
        })?,
    )?;
    sql.set(
        "QueryTyped",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let Some(text) = args.front().and_then(as_bytes) else {
                return Err(bad_argument(lua, &args, 1, "string"));
            };
            let mut params = Vec::new();
            for n in 2..=args.len() {
                params.push(param(lua, &args, n)?);
            }
            let st = state(lua);
            match with_db(&st, |db| query_typed(db, &text, &params)) {
                Ok(rows) => {
                    let t = lua.create_table_with_capacity(rows.len(), 0)?;
                    for (i, row) in rows.into_iter().enumerate() {
                        let r = lua.create_table()?;
                        for (name, v) in row {
                            let name = lua.create_string(name)?;
                            match v {
                                Typed::Int(n) => r.raw_set(name, n)?,
                                Typed::Real(f) => r.raw_set(name, f)?,
                                Typed::Bytes(b) => r.raw_set(name, lua.create_string(b)?)?,
                            }
                        }
                        t.raw_set(i + 1, r)?;
                    }
                    Ok(Value::Table(t))
                }
                Err(e) => {
                    set_error(lua, &e)?;
                    Ok(Value::Boolean(false))
                }
            }
        })?,
    )?;
    lua.globals().set("sql", sql)
}
