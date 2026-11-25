//! The `pg` binary module (gmsv_pg 2.1.0) as a built-in: the `pg` global with
//! `new_connection` and the version strings, and its three classes, `DatabaseConnection`,
//! `DatabaseQuery` and `PreparedQuery`, with the methods, fields, messages and result shapes
//! of the module (its README is the reference), over PostgreSQL's wire protocol.
//!
//! Parameters go as text: nil is NULL, booleans `true` and `false`, a whole number within the
//! 64-bit range as an integer, any other number in its shortest form. Results come as text and
//! become Lua values by the category of their type, read from `pg_type` on connect: booleans,
//! numbers (but whole numbers beyond 2^53, and what does not read as a number, stay strings),
//! strings; NULL fields are absent, and of columns of the same name the first is the one.
//!
//! A query runs at once and blocks, also an asynchronous one. Its listeners are not called from
//! `run`: its events wait for the `Think` hook `LuaEventEmitterManager_fpkg`, which the loop of
//! the runtime runs every turn and which Flux's adapter calls while it waits. A synchronous
//! query returns what its "success" or "error" listeners would get, and calls its "notice"
//! listeners before it returns. Listeners are called in the order they were added, those added
//! with `once` last, each in a protected call; an error in one is printed with `ErrorNoHalt`.
//!
//! Not implemented, and an error that says so: `listen`, `unlisten`, `cancel`, `activate`,
//! `deactivate`, and reconnecting a connection that was lost (what needs the server fails as
//! lost). libpq's environment variables and password file are not read.

mod conninfo;
mod session;
mod text;
mod tls;
mod wire;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use nl::mlua::{self, AnyUserData, Function, Lua, MultiValue, Table, UserData, Value};

use crate::runtime::{CLevel, function, lua_error, state, truthy};

use session::Session;
use text::Field;
use wire::{Done, Kind, Statement};

/// The version of gmsv_pg that this module mirrors: version, major, minor, patch, suffix.
pub(crate) const VERSION: [&str; 5] = ["2.1.0", "2", "1", "0", ""];

/// The name of the `Think` hook that delivers the events of asynchronous queries.
pub(crate) const HOOK: &str = "LuaEventEmitterManager_fpkg";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Connection,
    Query,
    Prepared,
}

impl Class {
    fn index(self) -> usize {
        self as usize
    }

    fn type_name(self) -> &'static str {
        match self {
            Class::Connection => "pg.DatabaseConnection",
            Class::Query => "pg.DatabaseQuery",
            Class::Prepared => "pg.PreparedQuery",
        }
    }

    fn shown(self) -> &'static str {
        match self {
            Class::Connection => "#<DatabaseConnection>",
            Class::Query => "#<DatabaseQuery>",
            Class::Prepared => "#<PreparedQuery>",
        }
    }

    fn of(ud: &AnyUserData) -> Option<Class> {
        if ud.is::<Connection>() {
            Some(Class::Connection)
        } else if ud.is::<Query>() {
            Some(Class::Query)
        } else if ud.is::<Prepared>() {
            Some(Class::Prepared)
        } else {
            None
        }
    }
}

/// A `DatabaseConnection`: its session and what `connect` was last called with.
struct Connection {
    session: Rc<RefCell<Session>>,
    host: Vec<u8>,
    user: Vec<u8>,
    password: Vec<u8>,
    database: Vec<u8>,
    port: Vec<u8>,
}

/// A `DatabaseQuery`: its text.
struct Query {
    session: Rc<RefCell<Session>>,
    text: Vec<u8>,
    sync: bool,
}

/// A `PreparedQuery`: the name of its statement.
struct Prepared {
    session: Rc<RefCell<Session>>,
    name: Vec<u8>,
    sync: bool,
}

fn add_metamethods<T, F: mlua::UserDataFields<T>>(fields: &mut F, class: Class) {
    for (i, name) in ["__index", "__newindex", "__tostring"]
        .into_iter()
        .enumerate()
    {
        fields.add_meta_field_with(name, move |lua| {
            Ok(module(lua)?.metas[class.index()][i].clone())
        });
    }
}

impl UserData for Connection {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        add_metamethods(fields, Class::Connection);
    }
}

impl UserData for Query {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        add_metamethods(fields, Class::Query);
    }
}

impl UserData for Prepared {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        add_metamethods(fields, Class::Prepared);
    }
}

/// An event that waits for `Think`: the object it is for, which it keeps alive until then,
/// and the arguments of its listeners.
struct Event {
    target: AnyUserData,
    name: &'static str,
    args: Vec<Value>,
}

/// What the module keeps for a Lua state (gloo's `LuaEventEmitterManager`).
struct Module {
    events: RefCell<VecDeque<Event>>,
    hooked: Cell<bool>,
    think: Function,
    /// `(f, ...)` to `ok, ...` or `false, error`, the error as it was raised.
    protect: Function,
    /// `__index`, `__newindex` and `__tostring` of each class.
    metas: [[Function; 3]; 3],
}

fn module(lua: &Lua) -> mlua::Result<Rc<Module>> {
    lua.app_data_ref::<Rc<Module>>()
        .map(|m| m.clone())
        .ok_or_else(|| mlua::Error::RuntimeError("the pg module is not open".into()))
}

/// An engine function of the module.
type Method = fn(&Lua, MultiValue) -> mlua::Result<MultiValue>;

fn values(items: impl IntoIterator<Item = Value>) -> mlua::Result<MultiValue> {
    Ok(items.into_iter().collect())
}

fn bytes(lua: &Lua, b: &[u8]) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(b)?))
}

/// Opens the module: sets the global `pg`.
pub(crate) fn open(lua: &Lua) -> mlua::Result<()> {
    if lua.app_data_ref::<Rc<Module>>().is_none() {
        let m = build(lua)?;
        lua.set_app_data(Rc::new(m));
    }
    let pg = lua.create_table()?;
    pg.set("new_connection", function(lua, new_connection)?)?;
    for (key, value) in [
        "version",
        "version_major",
        "version_minor",
        "version_patch",
        "version_suffix",
    ]
    .into_iter()
    .zip(VERSION)
    {
        pg.set(key, value)?;
    }
    lua.globals().set("pg", pg)
}

fn build(lua: &Lua) -> mlua::Result<Module> {
    let connection: [(&str, Method); 20] = [
        ("query", query),
        ("query_prepared", query_prepared),
        ("connect", connect),
        ("escape", |lua, a| escape(lua, a, Quoting::Escape)),
        ("escape_bytea", escape_bytea),
        ("unescape", unescape),
        ("quote", |lua, a| escape(lua, a, Quoting::Quote)),
        ("quote_name", |lua, a| escape(lua, a, Quoting::Name)),
        ("disconnect", disconnect),
        ("cancel", |lua, a| unimplemented(lua, a, "cancel")),
        ("protocol_version", |lua, a| {
            state_of(lua, a, Look::Protocol)
        }),
        ("server_version", |lua, a| state_of(lua, a, Look::Server)),
        ("deactivate", |lua, a| unimplemented(lua, a, "deactivate")),
        ("activate", |lua, a| unimplemented(lua, a, "activate")),
        ("is_open", |lua, a| state_of(lua, a, Look::Open)),
        ("prepare", prepare),
        ("unprepare", unprepare),
        ("listen", |lua, a| unimplemented(lua, a, "listen")),
        ("unlisten", |lua, a| unimplemented(lua, a, "unlisten")),
        ("set_encoding", set_encoding),
    ];

    let table = |class: Class| -> mlua::Result<Table> {
        let t = lua.create_table()?;
        let listeners: [(&str, Option<bool>); 3] = [
            ("on", Some(false)),
            ("once", Some(true)),
            ("add_listener", None),
        ];
        for (name, once) in listeners {
            let f = function(lua, move |lua, a: MultiValue| {
                add_listener(lua, a, class, once)
            })?;
            t.raw_set(name, f)?;
        }
        t.raw_set(
            "remove_listeners",
            function(lua, move |lua, a: MultiValue| {
                remove_listeners(lua, a, class)
            })?,
        )?;
        if class == Class::Connection {
            for (name, f) in connection {
                t.raw_set(name, function(lua, f)?)?;
            }
        } else {
            t.raw_set(
                "run",
                function(lua, move |lua, a: MultiValue| run(lua, a, class))?,
            )?;
            t.raw_set(
                "set_sync",
                function(lua, move |lua, a: MultiValue| set_sync(lua, a, class))?,
            )?;
        }
        Ok(t)
    };
    let methods = [
        table(Class::Connection)?,
        table(Class::Query)?,
        table(Class::Prepared)?,
    ];

    let metas_of = |class: Class| -> mlua::Result<[Function; 3]> {
        let methods = methods[class.index()].clone();
        let index = function(lua, move |lua, args: MultiValue| index(lua, &methods, args))?;
        let newindex = function(lua, |_, _: MultiValue| Ok(()))?;
        let tostring = function(lua, move |_, _: MultiValue| Ok(class.shown()))?;
        Ok([index, newindex, tostring])
    };
    let metas = [
        metas_of(Class::Connection)?,
        metas_of(Class::Query)?,
        metas_of(Class::Prepared)?,
    ];

    let (caller, _) = state(lua)
        .caller
        .borrow()
        .clone()
        .ok_or_else(|| mlua::Error::RuntimeError("the runtime has not started".into()))?;
    let protect: Function = lua
        .load(
            "local caller = ...\nlocal function raw(e) return e end\nreturn function(f, ...) return caller(f, raw, ...) end",
        )
        .set_name("=fpkg/pg")
        .call(caller)?;

    Ok(Module {
        events: RefCell::new(VecDeque::new()),
        hooked: Cell::new(false),
        think: function(lua, think)?,
        protect,
        metas,
    })
}

/// `__index`: a method, a field of a connection, or nil.
fn index(lua: &Lua, methods: &Table, args: MultiValue) -> mlua::Result<Value> {
    let Some(Value::String(key)) = args.get(1) else {
        return Ok(Value::Nil);
    };
    let found: Value = methods.raw_get(key.clone())?;
    if !found.is_nil() {
        return Ok(found);
    }
    if let Some(Value::UserData(ud)) = args.front()
        && let Ok(c) = ud.borrow::<Connection>()
    {
        let field = match &*key.as_bytes() {
            b"host" => &c.host,
            b"user" => &c.user,
            b"password" => &c.password,
            b"database" => &c.database,
            b"port" => &c.port,
            _ => return Ok(Value::Nil),
        };
        return bytes(lua, field);
    }
    Ok(Value::Nil)
}

/// The object a method is called on, or the module's error for anything else.
fn this(lua: &Lua, args: &MultiValue, class: Class) -> mlua::Result<AnyUserData> {
    match args.front() {
        Some(Value::UserData(ud)) if Class::of(ud) == Some(class) => Ok(ud.clone()),
        _ => Err(lua_error(
            lua,
            format!("bad argument #1 ({} expected)", class.type_name()),
        )),
    }
}

fn session_of(lua: &Lua, args: &MultiValue) -> mlua::Result<(AnyUserData, Rc<RefCell<Session>>)> {
    let ud = this(lua, args, Class::Connection)?;
    let session = ud.borrow::<Connection>()?.session.clone();
    Ok((ud, session))
}

/// `Peek`: the session of the connection, which has to have been connected.
fn peeked(lua: &Lua, args: &MultiValue) -> mlua::Result<(AnyUserData, Rc<RefCell<Session>>)> {
    let (ud, session) = session_of(lua, args)?;
    session.borrow().peek().map_err(|e| lua_error(lua, e))?;
    Ok((ud, session))
}

/// The string at argument `n` (counted from 1, the object being 1), if it is one.
fn string_at(args: &MultiValue, n: usize) -> Option<Vec<u8>> {
    match args.get(n - 1) {
        Some(Value::String(s)) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

/// `check_text`: a string without a zero byte, or the module's message.
fn check_text(args: &MultiValue, n: usize, invalid: &str, what: &str) -> Result<Vec<u8>, String> {
    let s = string_at(args, n).ok_or_else(|| invalid.to_string())?;
    if s.contains(&0) {
        return Err(format!("pg - {what} contains a zero byte"));
    }
    Ok(s)
}

/// `true`, or `false` and the message.
fn attempt(lua: &Lua, result: Result<(), Vec<u8>>) -> mlua::Result<MultiValue> {
    match result {
        Ok(()) => values([Value::Boolean(true)]),
        Err(e) => values([Value::Boolean(false), bytes(lua, &e)?]),
    }
}

fn new_userdata<T: UserData + 'static>(lua: &Lua, value: T) -> mlua::Result<AnyUserData> {
    let ud = lua.create_userdata(value)?;
    ud.set_user_value(lua.create_table()?)?;
    Ok(ud)
}

fn new_connection(lua: &Lua, _: MultiValue) -> mlua::Result<AnyUserData> {
    new_userdata(
        lua,
        Connection {
            session: Rc::new(RefCell::new(Session::default())),
            host: Vec::new(),
            user: Vec::new(),
            password: Vec::new(),
            database: Vec::new(),
            port: Vec::new(),
        },
    )
}

fn add_listener(
    lua: &Lua,
    args: MultiValue,
    class: Class,
    once: Option<bool>,
) -> mlua::Result<MultiValue> {
    let ud = this(lua, &args, class)?;
    let Some(Value::String(event)) = args.get(1) else {
        return Err(lua_error(lua, "bad argument #2 (string expected)"));
    };
    let Some(Value::Function(f)) = args.get(2) else {
        return Err(lua_error(lua, "bad argument #3 (function expected)"));
    };
    let once = once.unwrap_or(matches!(args.get(3), Some(Value::Boolean(true))));
    let field = if once { "listeners_once" } else { "listeners" };
    let store: Table = ud.user_value()?;
    let by_event = match store.raw_get::<Value>(field)? {
        Value::Table(t) => t,
        _ => {
            let t = lua.create_table()?;
            store.raw_set(field, t.clone())?;
            t
        }
    };
    let list = match by_event.raw_get::<Value>(event.clone())? {
        Value::Table(t) => t,
        _ => {
            let t = lua.create_table()?;
            by_event.raw_set(event.clone(), t.clone())?;
            t
        }
    };
    list.raw_set(list.raw_len() + 1, f.clone())?;
    Ok(MultiValue::new())
}

fn remove_listeners(lua: &Lua, args: MultiValue, class: Class) -> mlua::Result<MultiValue> {
    let ud = this(lua, &args, class)?;
    let store: Table = ud.user_value()?;
    store.raw_set("listeners", Value::Nil)?;
    store.raw_set("listeners_once", Value::Nil)?;
    Ok(MultiValue::new())
}

/// The listeners of an event on a list of one kind, as they are when it is delivered.
fn listeners_of(store: &Table, field: &str, event: &str) -> mlua::Result<Vec<Value>> {
    let Value::Table(by_event) = store.raw_get::<Value>(field)? else {
        return Ok(Vec::new());
    };
    let Value::Table(list) = by_event.raw_get::<Value>(event)? else {
        return Ok(Vec::new());
    };
    (1..=list.raw_len()).map(|i| list.raw_get(i)).collect()
}

/// Calls the listeners of the event on the object, those added with `once` last and then
/// forgotten. Each is called in a protected call, and an error in one is printed with
/// `ErrorNoHalt`; looking that up is not protected, as in the module.
fn call_listeners(
    lua: &Lua,
    target: &AnyUserData,
    event: &str,
    args: &[Value],
) -> mlua::Result<()> {
    let Ok(Value::Table(store)) = target.user_value::<Value>() else {
        return Ok(());
    };
    let always = listeners_of(&store, "listeners", event)?;
    let once = listeners_of(&store, "listeners_once", event)?;
    if !once.is_empty()
        && let Value::Table(by_event) = store.raw_get::<Value>("listeners_once")?
    {
        by_event.raw_set(event, Value::Nil)?;
    }
    let m = module(lua)?;
    for f in always.into_iter().chain(once) {
        let mut call: MultiValue = std::iter::once(f).collect();
        call.extend(args.iter().cloned());
        let error = match CLevel::enter() {
            Some(_level) => {
                let mut r = m.protect.call::<MultiValue>(call)?;
                match r.pop_front() {
                    Some(Value::Boolean(true)) => None,
                    _ => Some(r.pop_front().unwrap_or(Value::Nil)),
                }
            }
            None => Some(Value::String(lua.create_string("stack overflow")?)),
        };
        if let Some(e) = error {
            let print: Value = lua.globals().get("ErrorNoHalt")?;
            if let Value::Function(print) = print {
                let _ = m.protect.call::<MultiValue>((print, e, "\n"));
            }
        }
    }
    Ok(())
}

/// The `Think` hook: delivers the events that were there when it began, one at a time, so that
/// an error that gets out costs only the event it came from.
fn think(lua: &Lua, _: MultiValue) -> mlua::Result<()> {
    let m = module(lua)?;
    let count = m.events.borrow().len();
    for _ in 0..count {
        let event = m.events.borrow_mut().pop_front();
        let Some(event) = event else { break };
        call_listeners(lua, &event.target, event.name, &event.args)?;
    }
    Ok(())
}

/// Adds the `Think` hook, the first time an event is to be delivered.
fn hook_think(lua: &Lua, m: &Module) -> mlua::Result<()> {
    if m.hooked.get() {
        return Ok(());
    }
    let Value::Table(hook) = lua.globals().get::<Value>("hook")? else {
        return Err(lua_error(
            lua,
            "unable to listen for events, the hook library is not loaded",
        ));
    };
    let add: Value = hook.get("Add")?;
    let mut r = m
        .protect
        .call::<MultiValue>((add, "Think", HOOK, m.think.clone()))?;
    if !matches!(r.pop_front(), Some(Value::Boolean(true))) {
        return Err(lua_error(
            lua,
            "unable to listen for events, hook.Add failed",
        ));
    }
    m.hooked.set(true);
    Ok(())
}

/// Queues an event for the next `Think`, and has the loop turn once more for it.
fn queue(lua: &Lua, m: &Module, target: &AnyUserData, name: &'static str, args: Vec<Value>) {
    m.events.borrow_mut().push_back(Event {
        target: target.clone(),
        name,
        args,
    });
    let _ = state(lua).poster.send(Box::new(|_: &Lua| Ok(())));
}

fn query(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let text = check_text(&args, 2, "pg - query string is invalid", "query string")
        .map_err(|e| lua_error(lua, e))?;
    let ud = new_userdata(
        lua,
        Query {
            session,
            text,
            sync: false,
        },
    )?;
    values([Value::UserData(ud)])
}

fn query_prepared(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let name = check_text(
        &args,
        2,
        "pg - prepared query name is invalid",
        "prepared query name",
    )
    .map_err(|e| lua_error(lua, e))?;
    if name.is_empty() {
        return Err(lua_error(lua, "pg - prepared query name is empty"));
    }
    let ud = new_userdata(
        lua,
        Prepared {
            session,
            name,
            sync: false,
        },
    )?;
    values([Value::UserData(ud)])
}

fn connect(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (ud, session) = session_of(lua, &args)?;
    let host = string_at(&args, 2);
    let user = string_at(&args, 3);
    let password = string_at(&args, 4);
    let database = string_at(&args, 5);
    let extra = string_at(&args, 7);
    let port = match args.get(5) {
        Some(Value::Number(n)) => text::number_param(*n).into_bytes(),
        Some(Value::Integer(i)) => text::number_param(*i as f64).into_bytes(),
        _ => string_at(&args, 6).unwrap_or_default(),
    };
    let mut conninfo = b"connect_timeout=5 ".to_vec();
    {
        let mut c = ud.borrow_mut::<Connection>()?;
        c.host = host.unwrap_or_else(|| b"127.0.0.1".to_vec());
        c.user = user.unwrap_or_else(|| b"postgres".to_vec());
        c.password = password.clone().unwrap_or_default();
        c.database = database.clone().unwrap_or_default();
        c.port = port;
        conninfo.extend_from_slice(b"host=");
        conninfo.extend(conninfo::quoted(&c.host));
        conninfo.extend_from_slice(b" user=");
        conninfo.extend(conninfo::quoted(&c.user));
        if password.is_some() {
            conninfo.extend_from_slice(b" password=");
            conninfo.extend(conninfo::quoted(&c.password));
        }
        if database.is_some() {
            conninfo.extend_from_slice(b" dbname=");
            conninfo.extend(conninfo::quoted(&c.database));
        }
        if !c.port.is_empty() {
            conninfo.extend_from_slice(b" port=");
            conninfo.extend(conninfo::quoted(&c.port));
        }
    }
    if let Some(extra) = extra {
        conninfo.push(b' ');
        conninfo.extend(extra);
    }
    if conninfo.contains(&0) {
        return attempt(
            lua,
            Err(b"pg - connection parameters contain a zero byte".to_vec()),
        );
    }
    let result = session.borrow_mut().connect(&conninfo);
    attempt(lua, result.map_err(String::into_bytes))
}

#[derive(Clone, Copy)]
enum Quoting {
    Escape,
    Quote,
    Name,
}

fn escape(lua: &Lua, args: MultiValue, how: Quoting) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    if string_at(&args, 2).is_none() {
        return Ok(MultiValue::new());
    }
    let what = if matches!(how, Quoting::Name) {
        "name"
    } else {
        "string"
    };
    let text = check_text(&args, 2, "", what).map_err(|e| lua_error(lua, e))?;
    let session = session.borrow();
    let result = match how {
        Quoting::Escape => session.escape(&text),
        Quoting::Quote => session.escape(&text).map(|e| {
            let mut q = Vec::with_capacity(e.len() + 2);
            q.push(b'\'');
            q.extend(e);
            q.push(b'\'');
            q
        }),
        Quoting::Name => session.quote_name(&text),
    };
    drop(session);
    match result {
        Ok(s) => values([bytes(lua, &s)?]),
        Err(e) => Err(lua_error(lua, e)),
    }
}

fn escape_bytea(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    peeked(lua, &args)?;
    match string_at(&args, 2) {
        Some(raw) => values([bytes(lua, &text::escape_bytea(&raw))?]),
        None => Ok(MultiValue::new()),
    }
}

fn unescape(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    peeked(lua, &args)?;
    match string_at(&args, 2) {
        Some(escaped) => match text::unescape_bytea(&escaped) {
            Ok(raw) => values([bytes(lua, &raw)?]),
            Err(e) => Err(lua_error(lua, e)),
        },
        None => Ok(MultiValue::new()),
    }
}

fn disconnect(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    session.borrow_mut().disconnect();
    values([Value::Boolean(true)])
}

/// A method this module does not implement: an error that says so, once there is a
/// connection.
fn unimplemented(lua: &Lua, args: MultiValue, name: &str) -> mlua::Result<MultiValue> {
    peeked(lua, &args)?;
    Err(lua_error(
        lua,
        format!("fpkg's pg module does not implement connection:{name}"),
    ))
}

#[derive(Clone, Copy)]
enum Look {
    Open,
    Server,
    Protocol,
}

fn state_of(lua: &Lua, args: MultiValue, look: Look) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let s = session.borrow();
    let v = match look {
        Look::Open => Value::Boolean(s.is_open()),
        Look::Server => Value::Number(s.server_version() as f64),
        Look::Protocol => Value::Number(s.protocol_version() as f64),
    };
    values([v])
}

fn prepare(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let checked = check_text(
        &args,
        2,
        "pg - prepared query name is invalid",
        "prepared query name",
    )
    .and_then(|name| {
        check_text(
            &args,
            3,
            "pg - prepared query definition is invalid",
            "prepared query definition",
        )
        .map(|definition| (name, definition))
    });
    let result = match checked {
        Ok((name, definition)) => session.borrow_mut().prepare(&name, &definition),
        Err(e) => Err(e.into_bytes()),
    };
    attempt(lua, result)
}

fn unprepare(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let result = match check_text(
        &args,
        2,
        "pg - prepared query name is invalid",
        "prepared query name",
    ) {
        Ok(name) => session.borrow_mut().unprepare(&name),
        Err(e) => Err(e.into_bytes()),
    };
    attempt(lua, result)
}

fn set_encoding(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, session) = peeked(lua, &args)?;
    let result = match check_text(&args, 2, "invalid encoding", "encoding") {
        Ok(encoding) => session.borrow_mut().set_encoding(&encoding),
        Err(e) => Err(e.into_bytes()),
    };
    attempt(lua, result)
}

fn set_sync(lua: &Lua, args: MultiValue, class: Class) -> mlua::Result<MultiValue> {
    let ud = this(lua, &args, class)?;
    let sync = args.get(1).is_some_and(truthy);
    if class == Class::Prepared {
        ud.borrow_mut::<Prepared>()?.sync = sync;
    } else {
        ud.borrow_mut::<Query>()?.sync = sync;
    }
    Ok(MultiValue::new())
}

/// The parameters of `run`, from its second argument on, as the text they go as.
fn params(lua: &Lua, args: &MultiValue) -> mlua::Result<Vec<Option<Vec<u8>>>> {
    let mut out = Vec::with_capacity(args.len().saturating_sub(1));
    for (i, v) in args.iter().skip(1).enumerate() {
        let n = i + 1;
        out.push(match v {
            Value::Nil => None,
            Value::Boolean(b) => Some(if *b { b"true".to_vec() } else { b"false".to_vec() }),
            Value::Integer(i) => Some(text::number_param(*i as f64).into_bytes()),
            Value::Number(f) => Some(text::number_param(*f).into_bytes()),
            Value::String(s) => {
                let s = s.as_bytes().to_vec();
                if s.contains(&0) {
                    return Err(lua_error(
                        lua,
                        format!("pg - parameter #{n} contains a zero byte, binary data has to go through escape_bytea"),
                    ));
                }
                Some(s)
            }
            _ => {
                return Err(lua_error(
                    lua,
                    format!("pg - bad parameter #{n} (string, number, boolean or nil expected)"),
                ));
            }
        });
    }
    Ok(out)
}

fn run(lua: &Lua, args: MultiValue, class: Class) -> mlua::Result<MultiValue> {
    let ud = this(lua, &args, class)?;
    let (session, text, sync, prepared) = if class == Class::Prepared {
        let p = ud.borrow::<Prepared>()?;
        (p.session.clone(), p.name.clone(), p.sync, true)
    } else {
        let q = ud.borrow::<Query>()?;
        (q.session.clone(), q.text.clone(), q.sync, false)
    };
    let params = params(lua, &args)?;
    let kind = if prepared {
        Kind::Prepared
    } else if params.is_empty() {
        Kind::Plain
    } else {
        Kind::Parameterised
    };
    let statement = Statement { kind, text, params };

    if sync {
        let done = session.borrow_mut().execute(&statement);
        let types = session.borrow().types.clone();
        let result = Converted::of(lua, done, &types)?;
        for args in &result.notices {
            call_listeners(lua, &ud, "notice", args)?;
        }
        return values(result.returned);
    }

    let m = module(lua)?;
    hook_think(lua, &m)?;
    let done = session.borrow_mut().execute(&statement);
    let types = session.borrow().types.clone();
    let result = Converted::of(lua, done, &types)?;
    for args in result.notices {
        queue(lua, &m, &ud, "notice", args);
    }
    let mut returned = result.returned;
    let name = if matches!(returned.first(), Some(Value::Boolean(true))) {
        "success"
    } else {
        "error"
    };
    returned.remove(0);
    queue(lua, &m, &ud, name, returned);
    Ok(MultiValue::new())
}

/// What came of a statement as Lua values: what a synchronous `run` returns (`true, rows,
/// size, affected` or `false, message, details`) and the arguments of each notice.
struct Converted {
    returned: Vec<Value>,
    notices: Vec<Vec<Value>>,
}

impl Converted {
    fn of(
        lua: &Lua,
        done: Done,
        types: &std::collections::HashMap<u32, u8>,
    ) -> mlua::Result<Converted> {
        let mut notices = Vec::with_capacity(done.notices.len());
        for n in &done.notices {
            notices.push(vec![
                bytes(lua, &n.message)?,
                Value::Table(details(lua, Some(&n.fields), false)?),
            ]);
        }
        let returned = if done.success {
            let set = done.set.unwrap_or_default();
            let rows = lua.create_table_with_capacity(set.rows.len(), 0)?;
            let mut keys = Vec::with_capacity(set.columns.len());
            let mut seen = HashSet::new();
            for c in &set.columns {
                let first = seen.insert(c.name.clone());
                let key = if first {
                    Some(lua.create_string(&c.name)?)
                } else {
                    None
                };
                keys.push((key, types.get(&c.type_oid).copied().unwrap_or(0)));
            }
            for (i, row) in set.rows.iter().enumerate() {
                let fields = lua.create_table()?;
                for ((key, category), value) in keys.iter().zip(set.fields(row)) {
                    let (Some(key), Some(value)) = (key, value) else {
                        continue;
                    };
                    let v = match text::convert(*category, value) {
                        Field::Bool(b) => Value::Boolean(b),
                        Field::Number(n) => Value::Number(n),
                        Field::Text(t) => bytes(lua, t)?,
                    };
                    fields.raw_set(key.clone(), v)?;
                }
                rows.raw_set(i + 1, fields)?;
            }
            vec![
                Value::Boolean(true),
                Value::Table(rows),
                Value::Number(set.rows.len() as f64),
                Value::Number(text::affected(&set.tag)),
            ]
        } else {
            vec![
                Value::Boolean(false),
                bytes(lua, &done.error)?,
                Value::Table(details(lua, done.details.as_ref(), done.lost)?),
            ]
        };
        Ok(Converted { returned, notices })
    }
}

/// The details of an error or a notice: the fields the server sent, by the module's names,
/// and `connection_lost`.
fn details(lua: &Lua, fields: Option<&text::Fields>, lost: bool) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if let Some(fields) = fields {
        for (code, name) in text::DETAILS {
            let Some(v) = fields.get(*code) else {
                continue;
            };
            let value = if *code == b'P' {
                text::number(v).map_or_else(|| bytes(lua, v), |n| Ok(Value::Number(n)))?
            } else {
                bytes(lua, v)?
            };
            t.raw_set(*name, value)?;
        }
    }
    if lost {
        t.raw_set("connection_lost", true)?;
    }
    Ok(t)
}
