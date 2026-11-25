//! The `mysqloo` binary module (MySQLOO 9.7.6) as a built-in: the `mysqloo` global with
//! `connect`, the constants and the debug counters, and its four kinds of objects, databases,
//! queries, prepared queries and transactions, with the methods, messages, callbacks and result
//! shapes that Lua sees of the game's module (its README is the reference), over MySQL's
//! client/server protocol (`wire`).
//!
//! Every object is a table whose `__CppObject` field holds the module's handle (a userdata of
//! type `MySQLOO UserData`) and whose metatable is shared by its kind. Callbacks are fields that
//! Lua sets on those tables.
//!
//! Nothing runs on another thread. `db:connect()` connects before it returns and then runs the
//! work that was started before it; `q:start()` on a connected database runs the statement
//! before it returns. Work started without a connection (before `connect`, after a failed
//! connect or after `disconnect`) waits in the database's queue, which is what `queueSize`,
//! `abort` and `abortAllQueries` see. Callbacks are never called from the call that did the
//! work: what finished waits for a delivery, which the `Think` hook `__MySQLOOThinkHook` makes
//! every turn of the loop for each database of `mysqloo.__weakDatabases`, and `db:wait()`,
//! `q:wait()` and `db:disconnect(true)` make on demand. Each callback is called in a protected
//! call; an error in one is reported as an uncaught error of the runtime (the task fails) and the
//! others are called all the same. `onAborted` is the exception: it is called inside `abort` and
//! `abortAllQueries`.
//!
//! A statement that loses the connection (or that the server no longer knows as prepared) is
//! run once more on a new connection when autoreconnect is on. The connect has a deadline of
//! 10 seconds; nothing after it times out.
//!
//! Not implemented, and an error that says so: the `capath` and `cipher` of `setSSLSettings`,
//! `LOAD DATA LOCAL INFILE`, the authentication plugins other than `mysql_native_password`,
//! `caching_sha2_password` and `sha256_password`, and escaping text that is not ASCII in a
//! character set of several bytes other than UTF-8. The update check over HTTP is not made.

mod auth;
mod tls;
mod value;
mod wire;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::ops::Deref;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use nl::mlua::{
    self, AnyUserData, Function, IntoLuaMulti, Lua, MultiValue, Table, UserData, Value,
};

use crate::runtime::{
    as_bytes, bad_value, function, lua_error, protected_call, state, type_name, until_nul,
};

use value::{Charset, Field};

/// `mysqloo.VERSION` and `mysqloo.MINOR_VERSION`.
const VERSION: (&str, &str) = ("9", "7");

/// The name of the `Think` hook that delivers the callbacks.
const HOOK: &str = "__MySQLOOThinkHook";

/// The type of the handle in `__CppObject`, and the `MetaID` of its metatable.
const HANDLE: (&str, i64) = ("MySQLOO UserData", 46);

/// The one deadline of a connect: TCP, handshake, TLS and authentication.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The character set of a new connection.
const DEFAULT_CHARSET: (&str, Charset) = ("utf8mb4", Charset::Utf8mb4);

/// The errors after which a statement is run once more on a new connection: the connection
/// lost or gone, too many or unknown prepared statements (1461, 1243), an idle session ended by
/// the server (4031), a statement not prepared.
const RECONNECT_ON: [u32; 6] = [
    wire::CR_SERVER_LOST,
    wire::CR_SERVER_GONE_ERROR,
    1461,
    1243,
    4031,
    wire::CR_NO_PREPARE_STMT,
];

/// What a run gets when it is waited for while its database has no connection to run it on.
const NO_CONNECTION: &str =
    "Can not wait on query, database is not connected or connection failed.";

/// `mysqloo.QUERY_*`: readable as constants only.
const QUERY_STATES: [(&str, i64); 5] = [
    ("QUERY_NOT_RUNNING", 0),
    ("QUERY_RUNNING", 1),
    ("QUERY_COMPLETE", 3),
    ("QUERY_ABORTED", 4),
    ("QUERY_WAITING", 5),
];

/// `mysqloo.OPTION_*`; only `OPTION_NUMERIC_FIELDS` changes anything.
const OPTIONS: [(&str, i64); 4] = [
    ("OPTION_NUMERIC_FIELDS", 1),
    ("OPTION_NAMED_FIELDS", 2),
    ("OPTION_INTERPRET_DATA", 4),
    ("OPTION_CACHE", 8),
];

const NUMERIC_FIELDS: u8 = 1;

/// The state of a database's connection, as `db:status()` numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Connected = 0,
    Connecting = 1,
    NotConnected = 2,
    Failed = 3,
}

const STATUSES: [(&str, Status); 4] = [
    ("DATABASE_CONNECTED", Status::Connected),
    ("DATABASE_CONNECTING", Status::Connecting),
    ("DATABASE_NOT_CONNECTED", Status::NotConnected),
    ("DATABASE_CONNECTION_FAILED", Status::Failed),
];

/// The kind of a Lua object of the module, which its metatable tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Database,
    Query,
    Prepared,
    Transaction,
}

const KINDS: [Kind; 4] = [
    Kind::Database,
    Kind::Query,
    Kind::Prepared,
    Kind::Transaction,
];

/// The kinds a method of queries takes.
const ANY_QUERY: &[Kind] = &[Kind::Query, Kind::Prepared, Kind::Transaction];
const WITH_RESULTS: &[Kind] = &[Kind::Query, Kind::Prepared];
const PREPARED: &[Kind] = &[Kind::Prepared];
const TRANSACTION: &[Kind] = &[Kind::Transaction];

impl Kind {
    fn slot(self) -> usize {
        self as usize
    }

    /// `MetaName` and `MetaID` of the metatable, and what `tostring` gives.
    fn names(self) -> (&'static str, i64, &'static str) {
        match self {
            Kind::Database => ("MySQLOO Database", 47, "Database "),
            Kind::Query => ("MySQLOO Query", 48, "MySQLOO Query "),
            Kind::Prepared => ("MySQLOO Prepared Query", 49, "MySQLOO Prepared Query "),
            Kind::Transaction => ("MySQLOO Transaction", 50, "MySQLOO Transaction "),
        }
    }
}

/// The module's own bookkeeping for `mysqloo.allocationCount()` and the other counters: the
/// objects made and freed, and the Lua values held and let go.
#[derive(Default)]
struct Tally {
    made: Cell<u64>,
    freed: Cell<u64>,
    held: Cell<u64>,
    released: Cell<u64>,
}

fn bump(c: &Cell<u64>) {
    c.set(c.get().wrapping_add(1));
}

/// Counts an object of the module from when it is made until it is dropped.
struct Counted(Rc<Tally>);

impl Counted {
    fn new(tally: &Rc<Tally>) -> Counted {
        bump(&tally.made);
        Counted(tally.clone())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        bump(&self.0.freed);
    }
}

/// A Lua value that the module holds on to, counted while it is held.
struct Held<T> {
    value: T,
    tally: Rc<Tally>,
}

impl<T> Held<T> {
    fn new(value: T, tally: &Rc<Tally>) -> Held<T> {
        bump(&tally.held);
        Held {
            value,
            tally: tally.clone(),
        }
    }
}

impl<T: Clone> Clone for Held<T> {
    fn clone(&self) -> Held<T> {
        Held::new(self.value.clone(), &self.tally)
    }
}

impl<T> Deref for Held<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> Drop for Held<T> {
    fn drop(&mut self) {
        bump(&self.tally.released);
    }
}

/// The handle in an object's `__CppObject`: what the object is on the Rust side.
#[derive(Clone)]
enum Native {
    Database(Rc<Database>),
    Job(Rc<Job>),
}

impl Native {
    fn kind(&self) -> Kind {
        match self {
            Native::Database(_) => Kind::Database,
            Native::Job(job) => job.kind(),
        }
    }
}

impl UserData for Native {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", HANDLE.0);
        fields.add_meta_field("MetaID", HANDLE.1);
    }
}

/// What a connection that has been made tells, as `serverVersion`, `serverInfo` and `hostInfo`
/// give it: from the first connect, or nothing after a failed one.
#[derive(Clone, Default)]
struct Facts {
    version: u32,
    info: Vec<u8>,
    host: Vec<u8>,
}

/// The connection callback of a database: not asked for yet, due (with the database's table,
/// which it keeps alive until then, and how the connect went), or delivered.
enum Greeting {
    Unasked,
    Due(Held<Table>, Result<(), Vec<u8>>),
    Given,
}

/// What waits in a database's queue.
enum Queued {
    Run(Rc<Run>),
    Ping,
    Close,
}

/// A database: how it connects, its connection, its queue and what waits to be delivered.
struct Database {
    count: Counted,
    settings: RefCell<wire::Settings>,
    reconnect: Cell<bool>,
    keep_statements: Cell<bool>,
    status: Cell<Status>,
    /// Whether `connect()` has been called.
    begun: Cell<bool>,
    conn: RefCell<Option<wire::Conn>>,
    /// The number of the connection, which tells a prepared statement whether it is still on
    /// the server.
    epoch: Cell<u64>,
    charset: Cell<(&'static str, Charset)>,
    facts: RefCell<Option<Facts>>,
    greeting: RefCell<Greeting>,
    queue: RefCell<VecDeque<Queued>>,
    /// The runs that finished, in the order they did, until they are delivered.
    done: RefCell<VecDeque<Rc<Run>>>,
    /// The statements of prepared queries that were collected, to close on the connection
    /// they belong to.
    orphans: RefCell<Vec<(u64, wire::Statement)>>,
}

/// A parameter of a prepared query as it was set.
#[derive(Clone, Debug, PartialEq)]
enum Arg {
    Null,
    Number(f64),
    Text(Vec<u8>),
    Flag(bool),
}

impl Arg {
    /// The parameter as it is sent: a number as a double, a boolean as the integer 1 or 0.
    fn to_wire(&self) -> wire::Param {
        match self {
            Arg::Null => wire::Param::Null,
            Arg::Number(n) => wire::Param::Double(*n),
            Arg::Text(t) => wire::Param::Text(t.clone()),
            Arg::Flag(b) => wire::Param::Long(i32::from(*b)),
        }
    }
}

/// One set of parameters, by their index from 1.
type ParamSet = BTreeMap<u32, Arg>;

/// A query added to a transaction, with the parameters it had then.
#[derive(Clone)]
struct Member {
    job: Rc<Job>,
    sets: Vec<ParamSet>,
}

/// What is particular to a kind of query.
enum Part {
    Query,
    Prepared {
        sets: RefCell<Vec<ParamSet>>,
        /// The statement on the server, with the number of the connection it was made on.
        statement: RefCell<Option<(u64, wire::Statement)>>,
    },
    Transaction {
        members: RefCell<Vec<Member>>,
    },
}

/// What the read methods of a query show: the run delivered last.
#[derive(Default)]
struct View {
    delivered: bool,
    failed: bool,
    error: Vec<u8>,
    /// The results still kept, the current one first.
    entries: VecDeque<wire::Answer>,
    /// The table `getData` returns until the next delivery or `getNextResults`.
    rows: Option<Held<Table>>,
    /// Counts the deliveries, so that a callback can tell whether a later one replaced its view.
    serial: u64,
}

/// A query, a prepared query or a transaction.
struct Job {
    count: Counted,
    db: Rc<Database>,
    /// The database's table, held so that a query keeps its database alive.
    _owner: Held<Table>,
    sql: Vec<u8>,
    options: Cell<u8>,
    started: Cell<bool>,
    /// The runs not delivered yet: `isRunning()`.
    running: Cell<usize>,
    latest: RefCell<Weak<Run>>,
    view: RefCell<View>,
    part: Part,
}

impl Job {
    fn kind(&self) -> Kind {
        match self.part {
            Part::Query => Kind::Query,
            Part::Prepared { .. } => Kind::Prepared,
            Part::Transaction { .. } => Kind::Transaction,
        }
    }

    fn tally(&self) -> &Rc<Tally> {
        &self.count.0
    }

    /// The SQL as `onError` gets it.
    fn shown_sql(&self) -> &[u8] {
        until_nul(&self.sql)
    }

    /// Shows a delivered run; the number that tells whether a later one replaced it.
    fn show(&self, mut view: View) -> u64 {
        let mut current = self.view.borrow_mut();
        view.serial = current.serial + 1;
        view.delivered = true;
        *current = view;
        current.serial
    }

    fn finish_one(&self) {
        self.running.set(self.running.get().saturating_sub(1));
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Part::Prepared { statement, .. } = &self.part
            && let Some(kept) = statement.borrow_mut().take()
            && let Ok(mut orphans) = self.db.orphans.try_borrow_mut()
        {
            orphans.push(kept);
        }
    }
}

/// The callbacks of a run.
#[derive(Clone, Copy)]
enum Callback {
    Success,
    Error,
    Data,
    Aborted,
}

impl Callback {
    const ALL: [Callback; 4] = [
        Callback::Success,
        Callback::Error,
        Callback::Data,
        Callback::Aborted,
    ];

    fn field(self) -> &'static str {
        match self {
            Callback::Success => "onSuccess",
            Callback::Error => "onError",
            Callback::Data => "onData",
            Callback::Aborted => "onAborted",
        }
    }
}

/// A query of a transaction as a run refers to it: weakly, so that a run left in the queue
/// keeps nothing alive once it has been delivered.
struct Step {
    job: Weak<Job>,
    sets: Vec<ParamSet>,
}

/// What a run runs with.
enum Input {
    Text,
    Prepared(Vec<ParamSet>),
    Transaction(Vec<Step>),
}

/// How a run went.
enum Outcome {
    Rows(Vec<wire::Answer>),
    Committed(Vec<Vec<wire::Answer>>),
    /// The error, and the results of the statements before it.
    Failed(Vec<u8>, Vec<wire::Answer>),
}

/// What a run holds on to until it is delivered: its query and the query's table, which stay
/// alive until then, the callbacks it was started with, and the queries of a transaction.
struct Hold {
    _job: Rc<Job>,
    table: Held<Table>,
    callbacks: [Option<Held<Function>>; 4],
    _members: Vec<Rc<Job>>,
}

/// One start of a query: the query, what it holds on to until it is delivered, its input and
/// how it went. A run that is executed after it was delivered (one that `q:wait()` failed and
/// that stayed in the queue) is delivered again without callbacks.
struct Run {
    job: Weak<Job>,
    hold: RefCell<Option<Hold>>,
    /// The first start of a query looks up a callback it did not have at the delivery.
    late: bool,
    input: Input,
    outcome: RefCell<Option<Outcome>>,
    delivered: Cell<bool>,
}

impl Run {
    fn is_of(&self, job: &Rc<Job>) -> bool {
        std::ptr::eq(self.job.as_ptr(), Rc::as_ptr(job))
    }

    /// The query's table, while the run holds on to it.
    fn table(&self) -> Option<Table> {
        self.hold.borrow().as_ref().map(|h| h.table.value.clone())
    }

    fn callback(&self, which: Callback) -> Option<Function> {
        let table = {
            let hold = self.hold.borrow();
            let hold = hold.as_ref()?;
            if let Some(f) = &hold.callbacks[which as usize] {
                return Some(f.value.clone());
            }
            if !self.late {
                return None;
            }
            hold.table.value.clone()
        };
        match table.get::<Value>(which.field()) {
            Ok(Value::Function(f)) => Some(f),
            _ => None,
        }
    }

    /// Marks the run as delivered: it no longer counts for `isRunning()` (once), and lets go
    /// of what it held.
    fn settle(&self) {
        let first = !self.delivered.replace(true);
        let hold = self.hold.borrow_mut().take();
        if first && let Some(job) = self.job.upgrade() {
            job.finish_one();
        }
        drop(hold);
    }

    /// The queries of a transaction that this run counted as running.
    fn release_members(&self) {
        if let Input::Transaction(steps) = &self.input {
            for job in steps.iter().filter_map(|s| s.job.upgrade()) {
                job.finish_one();
            }
        }
    }
}

/// An error made here as the protocol layer makes its own.
fn gone() -> wire::Error {
    wire::Error::new(wire::CR_SERVER_GONE_ERROR, "Server has gone away")
}

/// A run that failed with an error, after the results that came before it.
fn failed(e: wire::Error, before: Vec<wire::Answer>) -> (Outcome, Option<u32>) {
    (Outcome::Failed(e.message, before), Some(e.code))
}

impl Database {
    fn new(settings: wire::Settings, tally: &Rc<Tally>) -> Database {
        Database {
            count: Counted::new(tally),
            settings: RefCell::new(settings),
            reconnect: Cell::new(true),
            keep_statements: Cell::new(true),
            status: Cell::new(Status::NotConnected),
            begun: Cell::new(false),
            conn: RefCell::new(None),
            epoch: Cell::new(0),
            charset: Cell::new(DEFAULT_CHARSET),
            facts: RefCell::new(None),
            greeting: RefCell::new(Greeting::Unasked),
            queue: RefCell::new(VecDeque::new()),
            done: RefCell::new(VecDeque::new()),
            orphans: RefCell::new(Vec::new()),
        }
    }

    fn tally(&self) -> &Rc<Tally> {
        &self.count.0
    }

    fn connected(&self) -> bool {
        self.status.get() == Status::Connected
    }

    /// The first connect, with what it tells kept for `serverVersion` and the others.
    fn open(&self) -> Result<(), wire::Error> {
        let made = wire::Conn::connect(&self.settings.borrow());
        let facts = match &made {
            Ok(conn) => Facts {
                version: server_version(&conn.server_info),
                info: conn.server_info.clone(),
                host: conn.host_info.clone(),
            },
            Err(_) => Facts::default(),
        };
        *self.facts.borrow_mut() = Some(facts);
        self.install(made?);
        Ok(())
    }

    fn install(&self, conn: wire::Conn) {
        self.epoch.set(self.epoch.get() + 1);
        *self.conn.borrow_mut() = Some(conn);
    }

    /// A new connection in place of the one there was, with the same settings and the
    /// character set last set.
    fn reopen(&self) -> Result<(), wire::Error> {
        drop(self.conn.borrow_mut().take());
        let mut conn = wire::Conn::connect(&self.settings.borrow())?;
        let (name, _) = self.charset.get();
        if name != DEFAULT_CHARSET.0 {
            let sql = format!("SET NAMES {name}");
            let _ = conn.query(sql.as_bytes(), &mut Vec::new());
        }
        self.install(conn);
        Ok(())
    }

    /// Whether the server answers; when it does not, whether a new connection could be made.
    fn ping_now(&self) -> bool {
        let answered = self
            .conn
            .borrow_mut()
            .as_mut()
            .is_some_and(|c| c.ping().is_ok());
        answered || self.reopen().is_ok()
    }

    fn close_now(&self) {
        if let Some(mut conn) = self.conn.borrow_mut().take() {
            conn.quit();
        }
        self.status.set(Status::NotConnected);
    }

    /// Runs what waits in the queue, in order, while there is a connection: runs, pings, and a
    /// disconnect, which ends it.
    fn work_off(&self, lua: &Lua) {
        while self.connected() {
            let next = self.queue.borrow_mut().pop_front();
            match next {
                None => break,
                Some(Queued::Run(run)) => self.complete(lua, run),
                Some(Queued::Ping) => {
                    let _ = self.ping_now();
                }
                Some(Queued::Close) => self.close_now(),
            }
        }
    }

    /// Runs a run now, or queues it when there is no connection to run it on.
    fn submit(&self, lua: &Lua, run: Rc<Run>) {
        if self.connected() && self.queue.borrow().is_empty() {
            self.complete(lua, run);
        } else {
            self.queue.borrow_mut().push_back(Queued::Run(run));
        }
    }

    /// Runs a run and has it wait for the next delivery; a run whose query is gone is dropped.
    fn complete(&self, lua: &Lua, run: Rc<Run>) {
        if let Some(outcome) = self.perform(&run) {
            *run.outcome.borrow_mut() = Some(outcome);
            self.finished(lua, run);
        }
    }

    fn finished(&self, lua: &Lua, run: Rc<Run>) {
        self.done.borrow_mut().push_back(run);
        wake(lua);
    }

    /// Whether a run is still in the queue.
    fn is_queued(&self, run: &Rc<Run>) -> bool {
        self.queue
            .borrow()
            .iter()
            .any(|q| matches!(q, Queued::Run(r) if Rc::ptr_eq(r, run)))
    }

    fn to_front(&self, run: &Rc<Run>) {
        let mut queue = self.queue.borrow_mut();
        if let Some(at) = queue
            .iter()
            .position(|q| matches!(q, Queued::Run(r) if Rc::ptr_eq(r, run)))
            && let Some(entry) = queue.remove(at)
        {
            queue.push_front(entry);
        }
    }

    /// Takes the runs of a query out of the queue.
    fn take_runs_of(&self, job: &Rc<Job>) -> Vec<Rc<Run>> {
        let mut queue = self.queue.borrow_mut();
        let mut taken = Vec::new();
        queue.retain(|q| match q {
            Queued::Run(r) if r.is_of(job) => {
                taken.push(r.clone());
                false
            }
            _ => true,
        });
        taken
    }

    /// A run on the connection, and once more on a new connection when it lost the old one
    /// and autoreconnect is on.
    fn perform(&self, run: &Run) -> Option<Outcome> {
        let (outcome, code) = self.attempt(run)?;
        if code.is_some_and(|c| RECONNECT_ON.contains(&c))
            && self.reconnect.get()
            && self.reopen().is_ok()
        {
            return self.attempt(run).map(|(again, _)| again);
        }
        Some(outcome)
    }

    /// A run on the connection: how it went, and the code of its error when it failed; nothing
    /// when its query (or one of a transaction) is gone.
    fn attempt(&self, run: &Run) -> Option<(Outcome, Option<u32>)> {
        let job = run.job.upgrade()?;
        let members = match &run.input {
            Input::Transaction(steps) => Some(
                steps
                    .iter()
                    .map(|s| Some((s.job.upgrade()?, s.sets.as_slice())))
                    .collect::<Option<Vec<_>>>()?,
            ),
            _ => None,
        };
        let mut slot = self.conn.borrow_mut();
        let Some(conn) = slot.as_mut() else {
            return Some(failed(gone(), Vec::new()));
        };
        self.close_orphans(conn);
        if let Some(members) = members {
            return Some(self.transact(conn, &members));
        }
        let sets = match &run.input {
            Input::Prepared(sets) => sets.as_slice(),
            _ => &[],
        };
        let mut out = Vec::new();
        Some(match self.statement(conn, &job, sets, &mut out) {
            Ok(()) => (Outcome::Rows(out), None),
            Err(e) => failed(e, out),
        })
    }

    /// The statement of a query or a prepared query, its results added to `out`. A prepared
    /// query runs once for each parameter set.
    fn statement(
        &self,
        conn: &mut wire::Conn,
        job: &Job,
        sets: &[ParamSet],
        out: &mut Vec<wire::Answer>,
    ) -> Result<(), wire::Error> {
        let Part::Prepared { statement, .. } = &job.part else {
            return conn.query(&job.sql, out);
        };
        let epoch = self.epoch.get();
        let cached = match *statement.borrow() {
            Some((made_on, st)) if made_on == epoch => Some(st),
            _ => None,
        };
        let st = match cached {
            Some(st) => st,
            None => {
                let st = conn.prepare(until_nul(&job.sql))?;
                *statement.borrow_mut() = Some((epoch, st));
                st
            }
        };
        let result = sets.iter().try_for_each(|set| {
            let params: Vec<wire::Param> = (1..=u32::from(st.params))
                .map(|i| set.get(&i).map_or(wire::Param::Null, Arg::to_wire))
                .collect();
            conn.execute(&st, &params, out)
        });
        let lost = result
            .as_ref()
            .is_err_and(|e| RECONNECT_ON.contains(&e.code));
        if lost || !self.keep_statements.get() {
            statement.borrow_mut().take();
            if !lost {
                conn.close_statement(&st);
            }
        }
        result
    }

    /// The queries of a transaction between `SET autocommit=0` and `COMMIT`, or `ROLLBACK` at
    /// the first that fails; `SET autocommit=1` after either.
    fn transact(
        &self,
        conn: &mut wire::Conn,
        members: &[(Rc<Job>, &[ParamSet])],
    ) -> (Outcome, Option<u32>) {
        let mut scratch = Vec::new();
        if let Err(e) = conn.query(b"SET autocommit=0", &mut scratch) {
            return failed(e, Vec::new());
        }
        let mut each = Vec::with_capacity(members.len());
        let mut failure = None;
        for (job, sets) in members {
            let mut out = Vec::new();
            match self.statement(conn, job, sets, &mut out) {
                Ok(()) => each.push(out),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if failure.is_none()
            && let Err(e) = conn.query(b"COMMIT", &mut scratch)
        {
            failure = Some(e);
        }
        if failure.is_some() {
            let _ = conn.query(b"ROLLBACK", &mut scratch);
        }
        let _ = conn.query(b"SET autocommit=1", &mut scratch);
        match failure {
            Some(e) => failed(e, Vec::new()),
            None => (Outcome::Committed(each), None),
        }
    }

    /// Closes the statements of collected prepared queries that are on this connection.
    fn close_orphans(&self, conn: &mut wire::Conn) {
        let orphans = std::mem::take(&mut *self.orphans.borrow_mut());
        for (epoch, st) in orphans {
            if epoch == self.epoch.get() {
                conn.close_statement(&st);
            }
        }
    }
}

/// Has the loop turn once more, so that what finished is delivered by its `Think`.
fn wake(lua: &Lua) {
    let _ = state(lua).poster.send(Box::new(|_: &Lua| Ok(())));
}

/// What the module keeps for a Lua state.
struct Module {
    tally: Rc<Tally>,
    /// The metatable of each kind.
    metas: [Table; 4],
    think: Function,
    /// Whether the metatable of the handles shows only `MetaName`, `MetaID` and `__gc` yet.
    plain_handles: Cell<bool>,
}

fn module(lua: &Lua) -> mlua::Result<Rc<Module>> {
    lua.app_data_ref::<Rc<Module>>()
        .map(|m| m.clone())
        .ok_or_else(|| mlua::Error::RuntimeError("the mysqloo module is not open".into()))
}

/// An engine function of the module.
type Method = fn(&Lua, MultiValue) -> mlua::Result<MultiValue>;

/// What a debug counter reads from the tally.
type Reading = fn(&Tally) -> u64;

fn nothing() -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}

fn give(lua: &Lua, v: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    v.into_lua_multi(lua)
}

fn text(lua: &Lua, b: &[u8]) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(b)?))
}

/// Opens the module: sets the global `mysqloo`, adds the `Think` hook and makes the convar
/// `sv_mysqloo_versioncheck`.
pub(crate) fn open(lua: &Lua) -> mlua::Result<()> {
    let existing = lua.app_data_ref::<Rc<Module>>().map(|m| m.clone());
    let m = match existing {
        Some(m) => m,
        None => {
            let m = Rc::new(build(lua)?);
            lua.set_app_data(m.clone());
            m
        }
    };
    let t = lua.create_table()?;
    t.raw_set("VERSION", VERSION.0)?;
    t.raw_set("MINOR_VERSION", VERSION.1)?;
    for (name, status) in STATUSES {
        t.raw_set(name, status as i64)?;
    }
    for (name, n) in QUERY_STATES.into_iter().chain(OPTIONS) {
        t.raw_set(name, n)?;
    }
    t.raw_set("connect", function(lua, connect)?)?;
    let counters: [(&str, Reading); 5] = [
        ("objectCount", |t| t.made.get().wrapping_sub(t.freed.get())),
        ("allocationCount", |t| t.made.get()),
        ("deallocationCount", |t| t.freed.get()),
        ("referenceCreatedCount", |t| t.held.get()),
        ("referenceFreedCount", |t| t.released.get()),
    ];
    for (name, read) in counters {
        let tally = m.tally.clone();
        t.raw_set(
            name,
            function(lua, move |_, _: MultiValue| Ok(read(&tally) as f64))?,
        )?;
    }
    let weak = lua.create_table()?;
    let mode = lua.create_table()?;
    mode.raw_set("__mode", "k")?;
    weak.set_metatable(Some(mode))?;
    t.raw_set("__weakDatabases", weak)?;
    let g = lua.globals();
    g.set("mysqloo", t)?;

    let add: Function = g.get::<Table>("hook")?.get("Add")?;
    add.call::<()>(("Think", HOOK, m.think.clone()))?;
    let create: Function = g.get("CreateConVar")?;
    create.call::<()>((
        "sv_mysqloo_versioncheck",
        "1",
        128,
        "Enable or disable the MySQLOO update checker.",
        0,
        1,
    ))?;
    Ok(())
}

fn build(lua: &Lua) -> mlua::Result<Module> {
    let mut metas = Vec::with_capacity(KINDS.len());
    for kind in KINDS {
        metas.push(metatable(lua, kind)?);
    }
    let metas: [Table; 4] = metas
        .try_into()
        .map_err(|_| mlua::Error::RuntimeError("four metatables".into()))?;
    Ok(Module {
        tally: Rc::new(Tally::default()),
        metas,
        think: function(lua, think)?,
        plain_handles: Cell::new(false),
    })
}

/// The methods of a kind of object.
fn methods(kind: Kind) -> Vec<(&'static str, Method)> {
    match kind {
        Kind::Database => <[(&'static str, Method); 19]>::into([
            ("abortAllQueries", abort_all),
            ("connect", db_connect),
            ("createTransaction", |lua, a| {
                new_job(lua, a, Kind::Transaction)
            }),
            ("disconnect", disconnect),
            ("escape", escape),
            ("hostInfo", |lua, a| server_fact(lua, a, Fact::Host)),
            ("ping", ping),
            ("prepare", |lua, a| new_job(lua, a, Kind::Prepared)),
            ("query", |lua, a| new_job(lua, a, Kind::Query)),
            ("queueSize", queue_size),
            ("serverInfo", |lua, a| server_fact(lua, a, Fact::Info)),
            ("serverVersion", |lua, a| server_fact(lua, a, Fact::Version)),
            ("setAutoReconnect", |lua, a| {
                before_connect(lua, a, Setting::Reconnect)
            }),
            ("setCachePreparedStatements", set_cache),
            ("setCharacterSet", set_charset),
            ("setMultiStatements", |lua, a| {
                before_connect(lua, a, Setting::Multi)
            }),
            ("setSSLSettings", set_ssl),
            ("status", status),
            ("wait", db_wait),
        ]),
        Kind::Query | Kind::Prepared | Kind::Transaction => {
            let common: [(&'static str, Method); 6] = [
                ("abort", abort),
                ("error", error_text),
                ("isRunning", is_running),
                ("setOption", set_option),
                ("start", start),
                ("wait", job_wait),
            ];
            let mut list = common.to_vec();
            if kind != Kind::Transaction {
                list.extend::<[(&'static str, Method); 5]>([
                    ("affectedRows", |lua, a| counts(lua, a, Count::Affected)),
                    ("getData", get_data),
                    ("getNextResults", next_results),
                    ("hasMoreResults", more_results),
                    ("lastInsert", |lua, a| counts(lua, a, Count::InsertId)),
                ]);
            }
            if kind == Kind::Prepared {
                list.extend::<[(&'static str, Method); 6]>([
                    ("clearParameters", |lua, a| param_sets(lua, a, true)),
                    ("putNewParameters", |lua, a| param_sets(lua, a, false)),
                    ("setBoolean", |lua, a| set_param(lua, a, Slot::Boolean)),
                    ("setNull", |lua, a| set_param(lua, a, Slot::Null)),
                    ("setNumber", |lua, a| set_param(lua, a, Slot::Number)),
                    ("setString", |lua, a| set_param(lua, a, Slot::String)),
                ]);
            }
            if kind == Kind::Transaction {
                list.extend::<[(&'static str, Method); 3]>([
                    ("addQuery", add_query),
                    ("clearQueries", clear_queries),
                    ("getQueries", get_queries),
                ]);
            }
            list
        }
    }
}

/// The metatable of a kind: `MetaName`, `MetaID`, `__index` (itself), `__tostring` and the
/// methods.
fn metatable(lua: &Lua, kind: Kind) -> mlua::Result<Table> {
    let (name, id, shown) = kind.names();
    let t = lua.create_table()?;
    t.raw_set("MetaName", name)?;
    t.raw_set("MetaID", id)?;
    t.raw_set("__index", t.clone())?;
    t.raw_set(
        "__tostring",
        function(lua, move |_, _: MultiValue| Ok(shown))?,
    )?;
    for (method, f) in methods(kind) {
        t.raw_set(method, function(lua, f)?)?;
    }
    Ok(t)
}

/// A new object: a table with the handle in `__CppObject` and the metatable of its kind.
fn new_object(lua: &Lua, native: Native) -> mlua::Result<Table> {
    let m = module(lua)?;
    let kind = native.kind();
    let handle = lua.create_userdata(native)?;
    if !m.plain_handles.get() {
        plain_metatable(lua, &handle)?;
        m.plain_handles.set(true);
    }
    let t = lua.create_table()?;
    t.raw_set("__CppObject", handle)?;
    t.set_metatable(Some(m.metas[kind.slot()].clone()))?;
    Ok(t)
}

/// Lets `getmetatable` see the metatable of the handles, which then has only `MetaName`,
/// `MetaID` and `__gc`.
fn plain_metatable(lua: &Lua, handle: &AnyUserData) -> mlua::Result<()> {
    // SAFETY: the handle is the one value on the stack; its metatable is a plain table and
    // setting a field of it to nil raises nothing.
    unsafe {
        lua.exec_raw::<()>(handle.clone(), |s| {
            if mlua::ffi::lua_getmetatable(s, -1) != 0 {
                for field in [c"__metatable", c"__name"] {
                    mlua::ffi::lua_pushnil(s);
                    mlua::ffi::lua_setfield(s, -2, field.as_ptr());
                }
            }
            mlua::ffi::lua_settop(s, 0);
        })
    }
}

/// The object at position `n` as the module checks one: a table (or the game's "bad
/// argument"), with a handle in `__CppObject`, looked up as Lua would.
fn object_at(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<(Table, Native)> {
    let v = args.get(n - 1);
    let Some(Value::Table(t)) = v else {
        return Err(bad_value(lua, n, "table", v));
    };
    object_of(lua, t)
}

fn object_of(lua: &Lua, t: &Table) -> mlua::Result<(Table, Native)> {
    let expected = || lua_error(lua, "[MySQLOO] Expected MySQLOO table");
    let Value::UserData(handle) = t.get::<Value>("__CppObject")? else {
        return Err(expected());
    };
    let native = handle
        .borrow::<Native>()
        .map(|n| Native::clone(&n))
        .map_err(|_| expected())?;
    Ok((t.clone(), native))
}

fn wrong_kind(lua: &Lua) -> mlua::Error {
    lua_error(lua, "[MySQLOO] Invalid CPP Object")
}

fn database_at(lua: &Lua, args: &MultiValue) -> mlua::Result<(Table, Rc<Database>)> {
    match object_at(lua, args, 1)? {
        (t, Native::Database(db)) => Ok((t, db)),
        _ => Err(wrong_kind(lua)),
    }
}

fn job_at(
    lua: &Lua,
    args: &MultiValue,
    n: usize,
    kinds: &[Kind],
) -> mlua::Result<(Table, Rc<Job>)> {
    match object_at(lua, args, n)? {
        (t, Native::Job(job)) if kinds.contains(&job.kind()) => Ok((t, job)),
        _ => Err(wrong_kind(lua)),
    }
}

/// A string argument: a string and nothing else.
fn need_string(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Vec<u8>> {
    match args.get(n - 1) {
        Some(Value::String(s)) => Ok(s.as_bytes().to_vec()),
        other => Err(bad_value(lua, n, "string", other)),
    }
}

fn need_number(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<f64> {
    match args.get(n - 1) {
        Some(Value::Number(x)) => Ok(*x),
        Some(Value::Integer(i)) => Ok(*i as f64),
        other => Err(bad_value(lua, n, "number", other)),
    }
}

fn need_bool(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<bool> {
    match args.get(n - 1) {
        Some(Value::Boolean(b)) => Ok(*b),
        other => Err(bad_value(lua, n, "bool", other)),
    }
}

/// A string argument that counts only when it is one, read up to its first zero byte.
fn optional_text(args: &MultiValue, n: usize) -> Option<Vec<u8>> {
    match args.get(n - 1) {
        Some(Value::String(s)) => Some(until_nul(&s.as_bytes()).to_vec()),
        _ => None,
    }
}

/// `mysqloo.connect(host, username, password, database[, port[, socket]])`: a database that
/// is not connected yet.
fn connect(lua: &Lua, args: MultiValue) -> mlua::Result<Table> {
    let mut parts = Vec::with_capacity(4);
    for n in 1..=4 {
        parts.push(until_nul(&need_string(lua, &args, n)?).to_vec());
    }
    let port = match args.get(4) {
        Some(Value::Number(x)) => (*x as i64) as u32,
        Some(Value::Integer(i)) => *i as u32,
        _ => 3306,
    };
    let [host, user, password, database]: [Vec<u8>; 4] = parts
        .try_into()
        .map_err(|_| mlua::Error::RuntimeError("four strings".into()))?;
    let settings = wire::Settings {
        host,
        user,
        password,
        database,
        port,
        socket: optional_text(&args, 6).unwrap_or_default(),
        multi_statements: true,
        tls: None,
        connect_timeout: CONNECT_TIMEOUT,
    };
    let m = module(lua)?;
    let db = Rc::new(Database::new(settings, &m.tally));
    let t = new_object(lua, Native::Database(db))?;
    if let Value::Table(registry) = lua.globals().get::<Value>("mysqloo")?
        && let Value::Table(weak) = registry.get::<Value>("__weakDatabases")?
    {
        weak.raw_set(t.clone(), true)?;
    }
    Ok(t)
}

/// `db:connect()`: connects, then runs what was started before.
fn db_connect(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (t, db) = database_at(lua, &args)?;
    if db.begun.get() {
        return Err(lua_error(lua, "Database already connected."));
    }
    db.begun.set(true);
    db.status.set(Status::Connecting);
    let result = db.open();
    db.status.set(if result.is_ok() {
        Status::Connected
    } else {
        Status::Failed
    });
    *db.greeting.borrow_mut() =
        Greeting::Due(Held::new(t, db.tally()), result.map_err(|e| e.message));
    wake(lua);
    db.work_off(lua);
    nothing()
}

/// `db:disconnect([wait])`: closes the connection now, or once it has been made when there
/// is none yet.
fn disconnect(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    if db.connected() {
        db.close_now();
    } else if !db.begun.get() || db.status.get() == Status::Failed {
        db.queue.borrow_mut().push_back(Queued::Close);
    }
    if matches!(args.get(1), Some(Value::Boolean(true))) {
        deliver(lua, &db)?;
    }
    nothing()
}

fn db_wait(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    if !db.begun.get() {
        return Err(lua_error(
            lua,
            "Tried to wait for database connection to finish without starting the connection!",
        ));
    }
    deliver(lua, &db)?;
    nothing()
}

fn status(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    give(lua, db.status.get() as i64)
}

fn queue_size(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let size = db.queue.borrow().len();
    give(lua, size as f64)
}

/// `db:abortAllQueries()`: empties the queue, calling `onAborted` of each run in it; the number
/// of runs and pings that were in it. A disconnect that waited there is dropped uncounted.
fn abort_all(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let taken: Vec<Queued> = db.queue.borrow_mut().drain(..).collect();
    let mut removed = 0usize;
    let mut runs = Vec::new();
    for q in taken {
        match q {
            Queued::Run(run) => {
                removed += 1;
                runs.push(run);
            }
            Queued::Ping => removed += 1,
            Queued::Close => {}
        }
    }
    for run in &runs {
        aborted(lua, run);
    }
    give(lua, removed as f64)
}

/// `db:ping()`: whether the server answers, or a new connection could be made; false, with a
/// ping left in the queue, without a connection.
fn ping(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    if !db.connected() {
        db.queue.borrow_mut().push_back(Queued::Ping);
        return give(lua, false);
    }
    give(lua, db.ping_now())
}

/// `db:escape(str)`: a number is escaped as its `tostring`, anything else that is not a
/// string as `""`.
fn escape(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    if !matches!(db.status.get(), Status::Connected | Status::Failed) {
        return Err(lua_error(
            lua,
            "Cannot escape using database that is not connected",
        ));
    }
    let raw = args.get(1).and_then(as_bytes).unwrap_or_default();
    let no_backslash = db
        .conn
        .borrow()
        .as_ref()
        .is_some_and(wire::Conn::no_backslash_escapes);
    let (_, class) = db.charset.get();
    match value::escape(&raw, class, no_backslash) {
        Ok(escaped) => give(lua, text(lua, &escaped)?),
        Err(e) => Err(lua_error(lua, e)),
    }
}

/// `db:setCharacterSet(name)`: whether the server took it, and `""`; a name the module does not
/// know is not sent.
fn set_charset(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let name = need_string(lua, &args, 2)?;
    if !db.connected() {
        return Err(lua_error(
            lua,
            "Database needs to be connected to change charset.",
        ));
    }
    let Some(charset) = value::charset(until_nul(&name)) else {
        return give(lua, (false, ""));
    };
    let sql = format!("SET NAMES {}", charset.0);
    let taken = db
        .conn
        .borrow_mut()
        .as_mut()
        .is_some_and(|c| c.query(sql.as_bytes(), &mut Vec::new()).is_ok());
    if taken {
        db.charset.set(charset);
    }
    give(lua, (taken, ""))
}

/// `db:setSSLSettings(key, cert, ca, capath, cipher)`: TLS with the files given as strings.
fn set_ssl(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    if db.begun.get() {
        return Err(lua_error(lua, "Cannot set SSL settings after connecting!"));
    }
    let given = |n| optional_text(&args, n).filter(|b| !b.is_empty());
    for (n, what) in [(5, "capath"), (6, "cipher")] {
        if given(n).is_some() {
            return Err(lua_error(
                lua,
                format!(
                    "fpkg's mysqloo module does not implement the {what} of database:setSSLSettings"
                ),
            ));
        }
    }
    let path = |n| given(n).map(|b| PathBuf::from(String::from_utf8_lossy(&b).into_owned()));
    let files = tls::Settings {
        key: path(2),
        cert: path(3),
        ca: path(4),
    };
    db.settings.borrow_mut().tls = (files != tls::Settings::default()).then_some(files);
    nothing()
}

#[derive(Clone, Copy)]
enum Setting {
    Reconnect,
    Multi,
}

/// `db:setAutoReconnect(bool)` and `db:setMultiStatements(bool)`, before `connect()` only.
fn before_connect(lua: &Lua, args: MultiValue, which: Setting) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let on = need_bool(lua, &args, 2)?;
    if db.begun.get() {
        return Err(lua_error(lua, "Database already connected."));
    }
    match which {
        Setting::Reconnect => db.reconnect.set(on),
        Setting::Multi => db.settings.borrow_mut().multi_statements = on,
    }
    nothing()
}

fn set_cache(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let on = need_bool(lua, &args, 2)?;
    if db.status.get() != Status::NotConnected {
        return Err(lua_error(
            lua,
            "setCachePreparedStatements has to be called before db:start()!",
        ));
    }
    db.keep_statements.set(on);
    nothing()
}

#[derive(Clone, Copy)]
enum Fact {
    Version,
    Info,
    Host,
}

/// `db:serverVersion()`, `db:serverInfo()` and `db:hostInfo()`, once the connect is over.
fn server_fact(lua: &Lua, args: MultiValue, which: Fact) -> mlua::Result<MultiValue> {
    let (_, db) = database_at(lua, &args)?;
    let Some(facts) = db.facts.borrow().clone() else {
        let what = match which {
            Fact::Version => "server version",
            Fact::Info | Fact::Host => "server info",
        };
        return Err(lua_error(
            lua,
            format!("Tried to get {what} when client is not connected to server yet!"),
        ));
    };
    match which {
        Fact::Version => give(lua, f64::from(facts.version)),
        Fact::Info => give(lua, text(lua, &facts.info)?),
        Fact::Host => give(lua, text(lua, &facts.host)?),
    }
}

/// `db:query(sql)`, `db:prepare(sql)` and `db:createTransaction()`.
fn new_job(lua: &Lua, args: MultiValue, kind: Kind) -> mlua::Result<MultiValue> {
    let (t, db) = database_at(lua, &args)?;
    let sql = if kind == Kind::Transaction {
        Vec::new()
    } else {
        need_string(lua, &args, 2)?
    };
    let part = match kind {
        Kind::Prepared => Part::Prepared {
            sets: RefCell::new(vec![ParamSet::new()]),
            statement: RefCell::new(None),
        },
        Kind::Transaction => Part::Transaction {
            members: RefCell::new(Vec::new()),
        },
        _ => Part::Query,
    };
    let job = Rc::new(Job {
        count: Counted::new(db.tally()),
        _owner: Held::new(t, db.tally()),
        db,
        sql,
        options: Cell::new(0),
        started: Cell::new(false),
        running: Cell::new(0),
        latest: RefCell::new(Weak::new()),
        view: RefCell::new(View::default()),
        part,
    });
    give(lua, new_object(lua, Native::Job(job))?)
}

/// `q:start()`: a run of the query with the callbacks its table has now.
fn start(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (t, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    let tally = job.tally().clone();
    let mut callbacks: [Option<Held<Function>>; 4] = [None, None, None, None];
    for which in Callback::ALL {
        if let Value::Function(f) = t.get::<Value>(which.field())? {
            callbacks[which as usize] = Some(Held::new(f, &tally));
        }
    }
    let (input, members) = match &job.part {
        Part::Query => (Input::Text, Vec::new()),
        Part::Prepared { sets, .. } => {
            let all = sets.borrow().clone();
            let last = all.last().cloned().unwrap_or_default();
            *sets.borrow_mut() = vec![last];
            (Input::Prepared(all), Vec::new())
        }
        Part::Transaction { members } => {
            let all = members.borrow().clone();
            let mut steps = Vec::with_capacity(all.len());
            let mut held = Vec::with_capacity(all.len());
            for m in all {
                m.job.running.set(m.job.running.get() + 1);
                steps.push(Step {
                    job: Rc::downgrade(&m.job),
                    sets: m.sets,
                });
                held.push(m.job);
            }
            (Input::Transaction(steps), held)
        }
    };
    let run = Rc::new(Run {
        job: Rc::downgrade(&job),
        hold: RefCell::new(Some(Hold {
            _job: job.clone(),
            table: Held::new(t, &tally),
            callbacks,
            _members: members,
        })),
        late: !job.started.replace(true),
        input,
        outcome: RefCell::new(None),
        delivered: Cell::new(false),
    });
    job.running.set(job.running.get() + 1);
    *job.latest.borrow_mut() = Rc::downgrade(&run);
    job.db.submit(lua, run);
    nothing()
}

/// `q:wait([swap])`: the last run started, then a delivery for the whole database. A run
/// without a connection to run on fails at once and stays in the queue.
fn job_wait(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    if job.running.get() == 0 {
        return Err(lua_error(lua, "Query not started"));
    }
    let db = job.db.clone();
    let latest = job.latest.borrow().upgrade();
    if let Some(run) = latest
        && db.is_queued(&run)
    {
        if matches!(args.get(1), Some(Value::Boolean(true))) {
            db.to_front(&run);
        }
        if db.connected() {
            db.work_off(lua);
        } else {
            *run.outcome.borrow_mut() = Some(Outcome::Failed(
                NO_CONNECTION.as_bytes().to_vec(),
                Vec::new(),
            ));
            db.finished(lua, run);
        }
    }
    deliver(lua, &db)?;
    nothing()
}

/// `q:abort()`: takes the query's runs out of the queue, calling `onAborted` for each; whether
/// there was one.
fn abort(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    let runs = job.db.take_runs_of(&job);
    for run in &runs {
        aborted(lua, run);
    }
    give(lua, !runs.is_empty())
}

/// A run taken out of the queue: its `onAborted`, and it no longer counts as running.
fn aborted(lua: &Lua, run: &Run) {
    if let (Some(me), Some(f)) = (run.table(), run.callback(Callback::Aborted)) {
        call_back(lua, &f, vec![Value::Table(me)]);
    }
    run.release_members();
    run.settle();
}

fn is_running(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    give(lua, job.running.get() > 0)
}

fn error_text(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    let error = job.view.borrow().error.clone();
    give(lua, text(lua, &error)?)
}

/// `q:setOption(option[, enabled])`.
fn set_option(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, ANY_QUERY)?;
    let option = need_number(lua, &args, 2)?;
    let enabled = if args.len() >= 3 {
        need_bool(lua, &args, 3)?
    } else {
        true
    };
    let bit = match option as i64 {
        n @ (1 | 2 | 4 | 8) => n as u8,
        _ => return Err(lua_error(lua, "Invalid Option")),
    };
    let options = job.options.get();
    job.options.set(if enabled {
        options | bit
    } else {
        options & !bit
    });
    nothing()
}

/// `q:getData()`: the rows of the current result, the same table until the next delivery or
/// `getNextResults()`; nil before a delivery and after a run that failed.
fn get_data(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, WITH_RESULTS)?;
    give(lua, current_rows(lua, &job)?)
}

fn current_rows(lua: &Lua, job: &Job) -> mlua::Result<Value> {
    let built = {
        let view = job.view.borrow();
        if !view.delivered || view.failed {
            return Ok(Value::Nil);
        }
        if let Some(rows) = &view.rows {
            return Ok(Value::Table(rows.value.clone()));
        }
        rows_table(lua, job, view.entries.front())?
    };
    job.view.borrow_mut().rows = Some(Held::new(built.clone(), job.tally()));
    Ok(Value::Table(built))
}

fn next_results(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, WITH_RESULTS)?;
    let mut view = job.view.borrow_mut();
    if view.entries.pop_front().is_none() {
        return Err(mlua::Error::RuntimeError("C++ exception".into()));
    }
    view.rows = None;
    nothing()
}

fn more_results(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, WITH_RESULTS)?;
    let view = job.view.borrow();
    if !view.delivered {
        return Err(lua_error(lua, "Query not completed yet"));
    }
    give(lua, !view.entries.is_empty())
}

#[derive(Clone, Copy)]
enum Count {
    InsertId,
    Affected,
}

/// `q:lastInsert()` and `q:affectedRows()` of the current result; 0 without one.
fn counts(lua: &Lua, args: MultiValue, which: Count) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, WITH_RESULTS)?;
    let view = job.view.borrow();
    let n = view.entries.front().map_or(0, |e| match which {
        Count::InsertId => e.insert_id,
        Count::Affected => e.affected_rows,
    });
    give(lua, n as f64)
}

#[derive(Clone, Copy)]
enum Slot {
    Number,
    String,
    Boolean,
    Null,
}

/// `setNumber`, `setString`, `setBoolean` and `setNull` of a prepared query: a parameter of
/// the last set. The index is truncated to an unsigned 32-bit number.
fn set_param(lua: &Lua, args: MultiValue, slot: Slot) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, PREPARED)?;
    let index = need_number(lua, &args, 2)? as i64 as u32;
    let arg = match slot {
        Slot::Number => Arg::Number(need_number(lua, &args, 3)?),
        Slot::String => Arg::Text(need_string(lua, &args, 3)?),
        Slot::Boolean => Arg::Flag(need_bool(lua, &args, 3)?),
        Slot::Null => Arg::Null,
    };
    if index < 1 {
        return Err(lua_error(lua, "Index must be greater than 0"));
    }
    if let Part::Prepared { sets, .. } = &job.part {
        let mut sets = sets.borrow_mut();
        if sets.is_empty() {
            sets.push(ParamSet::new());
        }
        if let Some(last) = sets.last_mut() {
            last.insert(index, arg);
        }
    }
    nothing()
}

/// `clearParameters()` (one empty set) and `putNewParameters()` (one more set).
fn param_sets(lua: &Lua, args: MultiValue, clear: bool) -> mlua::Result<MultiValue> {
    let (_, job) = job_at(lua, &args, 1, PREPARED)?;
    if let Part::Prepared { sets, .. } = &job.part {
        let mut sets = sets.borrow_mut();
        if clear {
            sets.clear();
        }
        sets.push(ParamSet::new());
    }
    nothing()
}

/// `tr:addQuery(q)`: appends `q` to `tr.__queries` with the global `table.insert`, and keeps
/// its parameters as they are now.
fn add_query(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (t, tr) = job_at(lua, &args, 1, TRANSACTION)?;
    let (q, job) = match object_at(lua, &args, 2) {
        Ok((q, Native::Job(job))) if WITH_RESULTS.contains(&job.kind()) => (q, job),
        _ => return Err(wrong_kind(lua)),
    };
    let queries = match t.get::<Value>("__queries")? {
        Value::Nil => {
            let fresh = lua.create_table()?;
            t.set("__queries", fresh.clone())?;
            Value::Table(fresh)
        }
        other => other,
    };
    let insert: Function = lua.globals().get::<Table>("table")?.get("insert")?;
    insert.call::<()>((queries, q))?;
    let sets = match &job.part {
        Part::Prepared { sets, .. } => sets.borrow().clone(),
        _ => Vec::new(),
    };
    if let Part::Transaction { members } = &tr.part {
        members.borrow_mut().push(Member { job, sets });
    }
    nothing()
}

/// `tr:getQueries()`: `tr.__queries`, or a new table; its argument is not checked.
fn get_queries(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let found = match args.front() {
        Some(Value::Table(t)) => t.get::<Value>("__queries")?,
        Some(Value::String(_)) => Value::Nil,
        other => {
            let shown = other.map_or("nil", type_name);
            return Err(mlua::Error::RuntimeError(format!(
                "attempt to index a {shown} value"
            )));
        }
    };
    match found {
        Value::Nil => give(lua, lua.create_table()?),
        v => give(lua, v),
    }
}

fn clear_queries(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let (t, tr) = job_at(lua, &args, 1, TRANSACTION)?;
    t.set("__queries", lua.create_table()?)?;
    if let Part::Transaction { members } = &tr.part {
        members.borrow_mut().clear();
    }
    nothing()
}

/// The `Think` hook: a delivery for each database of `mysqloo.__weakDatabases`.
fn think(lua: &Lua, _: MultiValue) -> mlua::Result<()> {
    let Value::Table(registry) = lua.globals().get::<Value>("mysqloo")? else {
        return Ok(());
    };
    let Value::Table(weak) = registry.get::<Value>("__weakDatabases")? else {
        return Ok(());
    };
    let mut keys = Vec::new();
    for pair in weak.pairs::<Value, Value>() {
        keys.push(pair?.0);
    }
    for key in keys {
        let Value::Table(t) = key else {
            return Err(lua_error(lua, "[MySQLOO] Expected MySQLOO table"));
        };
        match object_of(lua, &t)? {
            (_, Native::Database(db)) => deliver(lua, &db)?,
            _ => return Err(wrong_kind(lua)),
        }
    }
    Ok(())
}

/// A delivery for a database: its connection callback, once, then every run that had finished,
/// in the order they did.
fn deliver(lua: &Lua, db: &Rc<Database>) -> mlua::Result<()> {
    let greeting = {
        let mut g = db.greeting.borrow_mut();
        match *g {
            Greeting::Due(..) => std::mem::replace(&mut *g, Greeting::Given),
            _ => Greeting::Given,
        }
    };
    if let Greeting::Due(t, result) = greeting {
        let me = Value::Table(t.value.clone());
        let (field, args) = match result {
            Ok(()) => ("onConnected", vec![me]),
            Err(message) => ("onConnectionFailed", vec![me, text(lua, &message)?]),
        };
        if let Value::Function(f) = t.get::<Value>(field)? {
            call_back(lua, &f, args);
        }
    }
    let finished: Vec<Rc<Run>> = db.done.borrow_mut().drain(..).collect();
    for run in finished {
        let r = hand_over(lua, &run);
        run.settle();
        r?;
    }
    Ok(())
}

/// Calls a callback in a protected call; an error in it is reported as an uncaught error.
fn call_back(lua: &Lua, f: &Function, args: Vec<Value>) {
    if let Err(failure) = protected_call(lua, f, args.into_iter().collect()) {
        state(lua).report(&failure);
    }
}

/// Delivers a finished run: what its query shows from now on, then its callbacks while the run
/// still holds them.
fn hand_over(lua: &Lua, run: &Run) -> mlua::Result<()> {
    let Some(outcome) = run.outcome.borrow_mut().take() else {
        return Ok(());
    };
    let Some(job) = run.job.upgrade() else {
        return Ok(());
    };
    let me = run.table().map(Value::Table);
    let with = |which| Some((me.clone()?, run.callback(which)?));
    match outcome {
        Outcome::Rows(answers) => {
            let entries = VecDeque::from(answers);
            let data = rows_table(lua, &job, entries.front())?;
            let serial = job.show(View {
                entries,
                rows: Some(Held::new(data.clone(), job.tally())),
                ..View::default()
            });
            if let Some((me, f)) = with(Callback::Data) {
                let rows: Vec<Value> = data.sequence_values::<Value>().collect::<Result<_, _>>()?;
                for row in rows {
                    call_back(lua, &f, vec![me.clone(), row]);
                }
            }
            if let Some((me, f)) = with(Callback::Success) {
                call_back(lua, &f, vec![me, Value::Table(data)]);
                let mut view = job.view.borrow_mut();
                if view.serial == serial {
                    view.rows = None;
                }
            }
        }
        Outcome::Committed(each) => {
            let data = lua.create_table()?;
            if let Input::Transaction(steps) = &run.input {
                for (i, (step, answers)) in steps.iter().zip(each).enumerate() {
                    let Some(member) = step.job.upgrade() else {
                        continue;
                    };
                    let entries = VecDeque::from(answers);
                    let rows = rows_table(lua, &member, entries.front())?;
                    member.show(View {
                        entries,
                        rows: Some(Held::new(rows.clone(), job.tally())),
                        ..View::default()
                    });
                    member.finish_one();
                    data.raw_set(i + 1, rows)?;
                }
            }
            job.show(View::default());
            if let Some((me, f)) = with(Callback::Success) {
                call_back(lua, &f, vec![me, Value::Table(data)]);
            }
        }
        Outcome::Failed(message, partial) => {
            job.show(View {
                failed: true,
                error: message.clone(),
                entries: VecDeque::from(partial),
                ..View::default()
            });
            if let Some((me, f)) = with(Callback::Error) {
                let sql = text(lua, job.shown_sql())?;
                call_back(lua, &f, vec![me, text(lua, &message)?, sql]);
            }
        }
    }
    Ok(())
}

/// The rows of a result as a Lua table: each row keyed by column name (up to its first zero
/// byte), or by position with `OPTION_NUMERIC_FIELDS`; a NULL field is absent, and of columns of
/// the same name the later one counts.
fn rows_table(lua: &Lua, job: &Job, entry: Option<&wire::Answer>) -> mlua::Result<Table> {
    let Some(entry) = entry else {
        return lua.create_table();
    };
    let set = &entry.set;
    let numeric = job.options.get() & NUMERIC_FIELDS != 0;
    let mut keys = Vec::with_capacity(set.columns.len());
    for (i, c) in set.columns.iter().enumerate() {
        keys.push(if numeric {
            Value::Integer((i + 1) as _)
        } else {
            text(lua, until_nul(&c.name))?
        });
    }
    let rows = lua.create_table_with_capacity(set.rows.len(), 0)?;
    for (i, fields) in set.rows.iter().enumerate() {
        let row = if numeric {
            lua.create_table_with_capacity(fields.len(), 0)?
        } else {
            lua.create_table_with_capacity(0, fields.len())?
        };
        for ((column, key), field) in set.columns.iter().zip(&keys).zip(fields) {
            let v = match field {
                None => Value::Nil,
                Some(raw) => match value::field(column.meta.kind, raw) {
                    Field::Number(n) => Value::Number(n),
                    Field::Text(t) => text(lua, t)?,
                    Field::Nil => Value::Nil,
                },
            };
            row.raw_set(key.clone(), v)?;
        }
        rows.raw_set(i + 1, row)?;
    }
    Ok(rows)
}

/// The number `db:serverVersion()` gives for a version text: major * 10000 + minor * 100 +
/// patch, each the digits at its place (`11.8.9-MariaDB` is 110809, `5.7` is 50700), computed
/// with wrapping arithmetic as an unsigned 32-bit number.
fn server_version(text: &[u8]) -> u32 {
    let mut parts = [0u32; 3];
    let mut rest = text;
    for part in &mut parts {
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        *part = rest[..digits].iter().fold(0u32, |n, d| {
            n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
        });
        rest = &rest[digits..];
        match rest.split_first() {
            Some((b'.', after)) => rest = after,
            _ => break,
        }
    }
    parts[0]
        .wrapping_mul(10000)
        .wrapping_add(parts[1].wrapping_mul(100))
        .wrapping_add(parts[2])
}
