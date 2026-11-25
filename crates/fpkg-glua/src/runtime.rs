//! The state, how Lua is called and its errors reported, and the event loop.
//!
//! An engine function (one written in Rust, made with `function`) raises its errors as the
//! game's do: a string, `bad argument #1 to 'Exists' (string expected, got no value)` with the
//! caller's `file:line:` in front when the caller is Lua, and the name the caller used for the
//! function (`'?'` when it has none, as under `pcall`). It is one C function on the Lua stack
//! that runs the Rust code and raises the message of a Rust error as a plain Lua string, so
//! the game's own `pcall`, `xpcall` and coroutines see only strings and stay as they are.
//!
//! The calls from Rust into Lua that can nest without bound (`include`, `RunString`,
//! `CompileFile` runs, `require` of a Lua module, the loop's timers and hooks, the
//! `tostring` of `print` and `Msg`) nest at most `MAX_NESTING` deep: a deeper one fails with
//! "stack overflow" instead of overflowing the process's stack. `pcall` and the rest of Lua
//! are not limited.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_int;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nl::mlua::{
    self, FromLuaMulti, Function, IntoLuaMulti, Lua, LuaOptions, MultiValue, StdLib, Table, Value,
    ffi,
};
use nl::{NlExt, NlOption};

use crate::Error;
use crate::output::{self, Output};
use crate::timer::Timers;
use crate::tree::Tree;

/// The game's tick: `FrameTime()`, `engine.TickInterval()` and the longest wait of a loop
/// turn.
pub(crate) const TICK: f64 = 0.015;

/// How deep calls from Rust into Lua may nest on one thread.
pub(crate) const MAX_NESTING: usize = 200;

/// How a runtime is made.
#[derive(Clone, Debug)]
pub struct Config {
    pub tree: Tree,
    pub output: Output,
    /// Whether `MsgC` writes its colors; a stream that does not take escape sequences gets
    /// them stripped all the same (see `Output`).
    pub color: bool,
}

/// What opens a built-in binary module for `require`: it sets the module's globals.
pub type Opener = Rc<dyn Fn(&Lua) -> mlua::Result<()>>;

/// A closure posted to the loop from another thread.
type Posted = Box<dyn FnOnce(&Lua) -> mlua::Result<()> + Send>;

/// Posts closures to a runtime's loop from any thread; they run, in order, at its next turn.
#[derive(Clone)]
pub struct Poster(mpsc::Sender<Posted>);

impl Poster {
    /// False when the runtime is gone.
    pub fn post(&self, f: impl FnOnce(&Lua) -> mlua::Result<()> + Send + 'static) -> bool {
        self.0.send(Box::new(f)).is_ok()
    }
}

/// One frame of a Lua stack, as the game lists it under an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub name: String,
    pub source: String,
    pub line: i64,
}

/// A Lua error: its message and the stack where it was raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub stack: Vec<Frame>,
}

impl Failure {
    pub(crate) fn plain(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            stack: Vec::new(),
        }
    }

    /// The stack as the game prints it under `[ERROR]`.
    pub fn traceback(&self) -> String {
        let mut s = String::new();
        for (i, f) in self.stack.iter().enumerate() {
            s += &format!(
                "{}{}. {} - {}:{}\n",
                " ".repeat(i + 2),
                i + 1,
                f.name,
                f.source,
                f.line
            );
        }
        s
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// What the engine functions share.
pub(crate) struct State {
    pub tree: Tree,
    pub output: RefCell<Output>,
    pub color: bool,
    /// The folders of the files being included, innermost last (`None` at the root).
    pub paths: RefCell<Vec<Option<String>>>,
    pub timers: RefCell<Timers>,
    pub start: Instant,
    pub turns: Cell<u64>,
    pub db: RefCell<Option<rusqlite::Connection>>,
    pub modules: RefCell<BTreeMap<String, Opener>>,
    /// Uncaught Lua errors reported so far.
    pub reported: Cell<usize>,
    /// `ErrorNoHalt` and `ErrorNoHaltWithStack` messages printed so far.
    pub messages: Cell<usize>,
    pub inbox: mpsc::Receiver<Posted>,
    pub poster: mpsc::Sender<Posted>,
    /// The protected caller (a Lua function that calls the game's own `xpcall`) and the
    /// stack collector it is given.
    pub caller: RefCell<Option<(Function, Function)>>,
    /// The Rust code of the engine functions, by the number their C function holds.
    pub functions: RefCell<Vec<Engine>>,
    /// The stack depths at which the protected calls in progress began, innermost last.
    pub depths: RefCell<Vec<usize>>,
}

impl State {
    /// Seconds since the runtime started: `SysTime`, `CurTime`, `RealTime`.
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn out(&self, bytes: &[u8]) {
        self.output.borrow().out(bytes);
    }

    pub fn err(&self, bytes: &[u8]) {
        self.output.borrow().err(bytes);
    }

    /// Prints a Lua error as the game does and counts it.
    pub fn report(&self, f: &Failure) {
        self.reported.set(self.reported.get() + 1);
        let mut s = format!("[ERROR] {}\n", f.message);
        s += &f.traceback();
        s.push('\n');
        self.err(s.as_bytes());
    }
}

/// The state of a runtime as the engine functions find it.
pub(crate) fn state(lua: &Lua) -> Rc<State> {
    lua.app_data_ref::<Rc<State>>()
        .map(|s| s.clone())
        .expect("fpkg-glua state")
}

thread_local! {
    /// The calls from Rust into Lua in progress on this thread.
    static NESTING: Cell<usize> = const { Cell::new(0) };
}

/// One level of calls from Rust into Lua, given back when it is dropped.
pub(crate) struct CLevel;

impl CLevel {
    /// One more level; `None` (and nothing counted) at the limit.
    pub(crate) fn enter() -> Option<CLevel> {
        NESTING.with(|c| {
            if c.get() >= MAX_NESTING {
                return None;
            }
            c.set(c.get() + 1);
            Some(CLevel)
        })
    }

    /// `enter`, or Lua's error for a call that nests too deep.
    pub(crate) fn enter_or_error() -> mlua::Result<CLevel> {
        CLevel::enter().ok_or_else(|| mlua::Error::RuntimeError("stack overflow".into()))
    }
}

impl Drop for CLevel {
    fn drop(&mut self) {
        NESTING.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// The Rust code of an engine function.
pub(crate) type Engine = Rc<dyn Fn(&Lua, MultiValue) -> mlua::Result<MultiValue>>;

/// An engine function: `f` as one C function whose Rust errors reach Lua as strings.
pub(crate) fn function<F, A, R>(lua: &Lua, f: F) -> mlua::Result<Function>
where
    F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
    A: FromLuaMulti,
    R: IntoLuaMulti,
{
    let engine: Engine = Rc::new(move |lua, args: MultiValue| {
        f(lua, A::from_lua_multi(args, lua)?)?.into_lua_multi(lua)
    });
    let st = state(lua);
    let id = {
        let mut functions = st.functions.borrow_mut();
        functions.push(engine);
        functions.len() - 1
    };
    // SAFETY: the C function is pushed with its one upvalue, the number of its Rust code.
    unsafe {
        lua.exec_raw(id as i64, |s| {
            ffi::lua_pushcclosure(s, engine_c, 1);
        })
    }
}

/// Runs the Rust code `id` of an engine function and leaves its results on the Lua stack
/// with their number on top, or the message of its error with -1 on top.
fn run_engine(lua: &Lua, id: usize, args: MultiValue) -> mlua::Result<i64> {
    let f = state(lua).functions.borrow()[id].clone();
    let (values, count) = match f(lua, args) {
        Ok(results) => {
            let n = results.len() as i64;
            (results, n)
        }
        Err(e) => {
            let message = Value::String(lua.create_string(clean(&e))?);
            (MultiValue::from_vec(vec![message]), -1)
        }
    };
    // SAFETY: the values are pushed onto the stack of the running engine function, with
    // room checked first; nothing else is on the stack above its arguments.
    lua.exec_raw_lua(|raw| unsafe {
        if ffi::lua_checkstack(raw.state(), values.len() as c_int + 2) == 0 {
            return Err(mlua::Error::RuntimeError("stack overflow".into()));
        }
        for v in &values {
            raw.push_value(v)?;
        }
        Ok(count)
    })
}

/// The C function of every engine function: mlua runs the Rust code (upvalue 1 numbers
/// it), which leaves the results and their count, or a message and -1; a message is raised
/// as the string it is.
unsafe extern "C-unwind" fn engine_c(s: *mut ffi::lua_State) -> c_int {
    // SAFETY: only plain values live in this frame while Lua may raise an error through it.
    unsafe {
        let id = ffi::lua_tointeger(s, ffi::lua_upvalueindex(1)) as usize;
        Lua::entrypoint(s, move |lua, args: MultiValue| run_engine(lua, id, args));
        let count = ffi::lua_tointeger(s, -1);
        ffi::lua_settop(s, -2);
        if count < 0 {
            ffi::lua_error(s);
        }
        count as c_int
    }
}

/// An error raised at the caller's line, as `luaL_error` raises it.
pub(crate) fn lua_error(lua: &Lua, message: impl AsRef<str>) -> mlua::Error {
    let at = lua
        .inspect_stack(1, |d| {
            let src = d.source().short_src.map(|s| s.into_owned());
            match (src, d.current_line()) {
                (Some(s), Some(l)) => format!("{s}:{l}: "),
                _ => String::new(),
            }
        })
        .unwrap_or_default();
    mlua::Error::RuntimeError(format!("{at}{}", message.as_ref()))
}

/// `luaL_argerror` for the engine function that is running: `bad argument #n to 'name'
/// (extra)`, with the name its caller used ('?' when it has none). `n` is the position on
/// the Lua stack; a method call does not count its object, and a bad object is "calling
/// 'name' on bad self".
pub(crate) fn arg_error(lua: &Lua, n: usize, extra: &str) -> mlua::Error {
    let (name, method) = lua
        .inspect_stack(0, |d| {
            let names = d.names();
            (
                names.name.map(|s| s.into_owned()),
                names.name_what == Some("method"),
            )
        })
        .unwrap_or((None, false));
    let name = name.unwrap_or_else(|| "?".into());
    let n = if method { n.saturating_sub(1) } else { n };
    if method && n == 0 {
        return lua_error(lua, format!("calling '{name}' on bad self ({extra})"));
    }
    lua_error(lua, format!("bad argument #{n} to '{name}' ({extra})"))
}

/// The game's "bad argument" error for the value at stack position `n` (`None`: there is
/// none).
pub(crate) fn bad_value(lua: &Lua, n: usize, expected: &str, got: Option<&Value>) -> mlua::Error {
    let got = got.map_or("no value", type_name);
    arg_error(lua, n, &format!("{expected} expected, got {got}"))
}

/// The game's "bad argument" error for argument `n` of `args`.
pub(crate) fn bad_argument(lua: &Lua, args: &MultiValue, n: usize, expected: &str) -> mlua::Error {
    bad_value(lua, n, expected, args.get(n - 1))
}

pub(crate) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) | Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::Thread(_) => "thread",
        _ => "userdata",
    }
}

/// A string argument (a number counts, written as Lua writes it); `None` for anything else.
pub(crate) fn as_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::String(s) => Some(s.as_bytes().to_vec()),
        Value::Integer(i) => Some(crate::json::lua_number(*i as f64).into_bytes()),
        Value::Number(n) => Some(crate::json::lua_number(*n).into_bytes()),
        _ => None,
    }
}

pub(crate) fn as_string(v: &Value) -> Option<String> {
    as_bytes(v).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub(crate) fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        Value::String(s) => s.to_str().ok()?.trim().parse().ok(),
        _ => None,
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Boolean(false))
}

/// A string argument or the game's error.
pub(crate) fn check_string(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<String> {
    args.get(n - 1)
        .and_then(as_string)
        .ok_or_else(|| bad_argument(lua, args, n, "string"))
}

/// A string argument as its bytes (a number counts) or the game's error.
pub(crate) fn check_bytes(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Vec<u8>> {
    args.get(n - 1)
        .and_then(as_bytes)
        .ok_or_else(|| bad_argument(lua, args, n, "string"))
}

/// A number argument or the game's error.
pub(crate) fn check_number(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<f64> {
    args.get(n - 1)
        .and_then(as_number)
        .ok_or_else(|| bad_argument(lua, args, n, "number"))
}

pub(crate) fn arg(args: &MultiValue, n: usize) -> Value {
    args.get(n - 1).cloned().unwrap_or(Value::Nil)
}

/// The bytes before the first NUL, as C reads a string.
pub(crate) fn until_nul(b: &[u8]) -> &[u8] {
    match b.iter().position(|x| *x == 0) {
        Some(n) => &b[..n],
        None => b,
    }
}

/// Compiles GLua source without running it: `Err` holds the syntax error.
pub fn check_syntax(source: &[u8], chunk_name: &str) -> Result<(), String> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::new()).map_err(|e| e.to_string())?;
    lua.set_option(NlOption::Glua(true))
        .map_err(|e| e.to_string())?;
    lua.load(source)
        .set_name(format!("@{chunk_name}"))
        .set_mode(mlua::chunk::ChunkMode::Text)
        .into_function()
        .map(|_| ())
        .map_err(|e| clean(&e))
}

/// The text of an mlua error without its prefixes and tracebacks.
pub(crate) fn clean(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::CallbackError { cause, .. } => return clean(cause),
        mlua::Error::WithContext { cause, .. } => return clean(cause),
        other => other.to_string(),
    };
    text.split("\nstack traceback:")
        .next()
        .unwrap_or(&text)
        .trim_start_matches("runtime error: ")
        .trim_start_matches("syntax error: ")
        .trim_end()
        .to_string()
}

/// `fpkg.wait_until`: the predicate is called from Lua, so that whatever it raises goes on
/// as it is.
const WAIT_UNTIL: &str = r#"
local deadline_of, step, type, error = ...

return function(fn, seconds)
  if type(fn) ~= 'function' then
    error("bad argument #1 to 'wait_until' (function expected, got "..type(fn)..')', 2)
  end

  if seconds ~= nil and type(seconds) ~= 'number' then
    error("bad argument #2 to 'wait_until' (number expected, got "..type(seconds)..')', 2)
  end

  local deadline = deadline_of(seconds)

  while true do
    if fn() then return true end
    if not step(deadline) then return false end
  end
end
"#;

/// A Garry's Mod server-side Lua state without the game.
pub struct Runtime {
    lua: Lua,
    state: Rc<State>,
}

impl Runtime {
    pub fn new(config: Config) -> Result<Runtime, Error> {
        let start = |e: mlua::Error| Error::Start(clean(&e));
        // The game's libraries: Lua's own with debug and jit, without ffi.
        // SAFETY: the debug library is what the game's server has; nothing untrusted runs.
        let lua =
            unsafe { Lua::unsafe_new_with(StdLib::ALL_SAFE | StdLib::DEBUG, LuaOptions::new()) };
        lua.set_option(NlOption::Glua(true)).map_err(start)?;
        let (poster, inbox) = mpsc::channel();
        let state = Rc::new(State {
            tree: config.tree,
            output: RefCell::new(config.output),
            color: config.color,
            paths: RefCell::new(Vec::new()),
            timers: RefCell::new(Timers::default()),
            start: Instant::now(),
            turns: Cell::new(0),
            db: RefCell::new(None),
            modules: RefCell::new(BTreeMap::new()),
            reported: Cell::new(0),
            messages: Cell::new(0),
            inbox,
            poster,
            caller: RefCell::new(None),
            functions: RefCell::new(Vec::new()),
            depths: RefCell::new(Vec::new()),
        });
        lua.set_app_data(state.clone());
        let rt = Runtime { lua, state };
        rt.boot().map_err(start)?;
        Ok(rt)
    }

    fn boot(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        install_caller(lua)?;
        crate::output::install(lua)?;
        crate::include::install(lua)?;
        crate::files::install(lua)?;
        crate::util::install(lua)?;
        crate::timer::install(lua)?;
        crate::sql::install(lua)?;
        crate::modules::install(lua)?;
        crate::stock::load(lua, &lua.create_table()?)
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    pub fn tree(&self) -> &Tree {
        &self.state.tree
    }

    /// Sends the console elsewhere from now on.
    pub fn set_output(&self, output: Output) {
        *self.state.output.borrow_mut() = output;
    }

    /// Uncaught Lua errors reported so far: errors in timers, hooks and callbacks the loop
    /// runs, files `include` could not run, and the like; not errors a call returned.
    pub fn reported_errors(&self) -> usize {
        self.state.reported.get()
    }

    /// `ErrorNoHalt` and `ErrorNoHaltWithStack` messages printed so far.
    pub fn error_messages(&self) -> usize {
        self.state.messages.get()
    }

    /// A function written in Rust as an engine function: its errors reach Lua as strings.
    pub fn create_function<F, A, R>(&self, f: F) -> mlua::Result<Function>
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        function(&self.lua, f)
    }

    /// Registers a built-in binary module that `require(name)` opens.
    pub fn register_module(&self, name: &str, opener: Opener) {
        self.state
            .modules
            .borrow_mut()
            .insert(name.to_string(), opener);
    }

    /// Posts closures to the loop from other threads.
    pub fn poster(&self) -> Poster {
        Poster(self.state.poster.clone())
    }

    /// Compiles GLua source; `chunk_name` is shown in its errors (`gamemodes/x/taskspec.lua`).
    pub fn load(&self, source: &[u8], chunk_name: &str) -> Result<Function, Failure> {
        load_chunk(&self.lua, source, chunk_name)
    }

    /// Calls `f`; an error comes back with the stack where it was raised.
    pub fn call(&self, f: &Function, args: impl IntoLuaMulti) -> Result<MultiValue, Failure> {
        let args = args
            .into_lua_multi(&self.lua)
            .map_err(|e| Failure::plain(clean(&e)))?;
        protected_call(&self.lua, f, args)
    }

    /// Runs `f` with the include path of the file at `lua_path` (relative to `LUA`), so that
    /// its `include` calls resolve next to it.
    pub fn call_in(
        &self,
        lua_path: &str,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> Result<MultiValue, Failure> {
        self.state
            .paths
            .borrow_mut()
            .push(crate::include::folder_of(lua_path));
        let r = self.call(f, args);
        self.state.paths.borrow_mut().pop();
        r
    }

    /// Prints a failure as the game prints a Lua error.
    pub fn report(&self, f: &Failure) {
        self.state.report(f);
    }

    /// Flushes the console.
    pub fn flush(&self) {
        self.state.output.borrow().flush();
    }

    /// Runs a console command: a Lua `concommand` (the console is the caller, `nil`) or a
    /// convar assignment. Returns whether anything handled it.
    pub fn run_command(&self, line: &str) -> Result<bool, Failure> {
        let f: Function = self
            .lua
            .globals()
            .get::<Table>("concommand")
            .and_then(|t| t.get("__fpkg_run_line"))
            .map_err(|e| Failure::plain(clean(&e)))?;
        let r = self.call(&f, line)?;
        Ok(r.iter().next().is_some_and(truthy))
    }

    /// One turn of the loop: the closures posted from other threads, the timers that are due
    /// (not those created during the turn), then `hook.Run('Think')`. Errors are reported.
    /// Returns whether anything but `Think` ran.
    pub fn pump(&self) -> bool {
        turn(&self.lua)
    }

    /// Delivers what is due now: turns while posted closures or due timers wait, at most 100
    /// of them. Never waits for timers that are due later.
    pub fn settle(&self) {
        for _ in 0..100 {
            let ran = self.pump();
            if !ran && !self.state.timers.borrow().due_now(self.state.now()) {
                break;
            }
        }
    }

    /// Turns the loop until `f()` returns a true value or `seconds` pass (false then).
    pub fn wait_until(&self, f: &Function, seconds: f64) -> Result<bool, Failure> {
        let deadline = deadline_of(&self.state, Some(seconds));
        loop {
            let r = protected_call(&self.lua, f, MultiValue::new())?;
            if r.iter().next().is_some_and(truthy) {
                return Ok(true);
            }
            if !step(&self.lua, deadline) {
                return Ok(false);
            }
        }
    }

    /// `wait_until` as a Lua function `(fn, seconds)`: without seconds it waits for as long
    /// as it takes; what `fn` raises goes on as it is.
    pub fn wait_until_function(&self) -> mlua::Result<Function> {
        let lua = &self.lua;
        let deadline = crate::runtime::function(lua, |lua, seconds: Option<f64>| {
            Ok(deadline_of(&state(lua), seconds))
        })?;
        let step = crate::runtime::function(lua, |lua, deadline: f64| Ok(step(lua, deadline)))?;
        let g = lua.globals();
        lua.load(WAIT_UNTIL).set_name("=fpkg/wait_until").call((
            deadline,
            step,
            g.raw_get::<Value>("type")?,
            g.raw_get::<Value>("error")?,
        ))
    }
}

/// When a wait of `seconds` ends: never without seconds; a negative or NaN wait ends now.
fn deadline_of(st: &State, seconds: Option<f64>) -> f64 {
    match seconds {
        None => f64::INFINITY,
        Some(s) => st.now() + s.max(0.0),
    }
}

/// A turn of a wait that ends at `deadline`, then a sleep until the next timer is due (a
/// tick at most); false, without a turn, once the deadline has passed.
fn step(lua: &Lua, deadline: f64) -> bool {
    let st = state(lua);
    if st.now() >= deadline {
        return false;
    }
    turn(lua);
    let next = st.timers.borrow().next_due().unwrap_or(f64::INFINITY);
    let now = st.now();
    let wait = (next - now).min(deadline - now).clamp(0.0, TICK);
    if wait > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(wait));
    }
    true
}

/// One loop turn (see `Runtime::pump`).
pub(crate) fn turn(lua: &Lua) -> bool {
    let st = state(lua);
    st.turns.set(st.turns.get() + 1);
    let mut ran = false;
    while let Ok(f) = st.inbox.try_recv() {
        ran = true;
        if let Err(e) = f(lua) {
            st.report(&Failure::plain(clean(&e)));
        }
    }
    if crate::timer::run_due(lua, &st) {
        ran = true;
    }
    let think = lua
        .globals()
        .get::<Table>("hook")
        .and_then(|h| h.get::<Function>("Run"));
    if let Ok(run) = think
        && let Err(f) = protected_call(lua, &run, "Think".into_lua_multi(lua).unwrap_or_default())
    {
        st.report(&f);
    }
    ran
}

/// Loads GLua text with the game's chunk name `@<name>`.
pub(crate) fn load_chunk(lua: &Lua, source: &[u8], name: &str) -> Result<Function, Failure> {
    if source.first() == Some(&0x1b) {
        return Err(Failure::plain("Cannot run byte code!"));
    }
    lua.load(source)
        .set_name(format!("@{name}"))
        .set_mode(mlua::chunk::ChunkMode::Text)
        .into_function()
        .map_err(|e| Failure::plain(clean(&e)))
}

/// `xpcall(f, collector, ...)`: the results, or the error with the stack where it was
/// raised. Counts as a call through C.
pub(crate) fn protected_call(
    lua: &Lua,
    f: &Function,
    args: MultiValue,
) -> Result<MultiValue, Failure> {
    let Some(_level) = CLevel::enter() else {
        return Err(Failure {
            message: "stack overflow".into(),
            stack: caller_stack(lua),
        });
    };
    let st = state(lua);
    let (xpcall, collector) = st.caller.borrow().clone().ok_or_else(|| {
        Failure::plain("fpkg's runtime has not finished starting: there is no xpcall yet")
    })?;
    let mut all = MultiValue::new();
    all.push_back(Value::Function(f.clone()));
    all.push_back(Value::Function(collector));
    all.extend(args);
    st.depths.borrow_mut().push(depth(lua));
    let r = xpcall.call::<MultiValue>(all);
    st.depths.borrow_mut().pop();
    let mut r = r.map_err(|e| Failure::plain(clean(&e)))?;
    match r.pop_front() {
        Some(Value::Boolean(true)) => Ok(r),
        _ => Err(failure_of(lua, r.pop_front().unwrap_or(Value::Nil))),
    }
}

/// The error value `collect_stack` made, as a `Failure`.
fn failure_of(lua: &Lua, v: Value) -> Failure {
    let Value::Table(t) = &v else {
        return Failure::plain(
            output::tostring(lua, &v)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|e| clean(&e)),
        );
    };
    let message: String = t.raw_get("message").unwrap_or_default();
    let mut stack = Vec::new();
    if let Ok(frames) = t.raw_get::<Table>("stack") {
        for fr in frames.sequence_values::<Table>().flatten() {
            stack.push(Frame {
                name: fr.raw_get("name").unwrap_or_default(),
                source: fr.raw_get("source").unwrap_or_default(),
                line: fr.raw_get("line").unwrap_or(-1),
            });
        }
    }
    Failure { message, stack }
}

/// The number of active frames, found by doubling and halving (each probe walks the stack).
fn depth(lua: &Lua) -> usize {
    let active = |n: usize| lua.inspect_stack(n, |_| ()).is_some();
    if !active(0) {
        return 0;
    }
    let (mut low, mut high) = (0, 1);
    while active(high) {
        low = high;
        high *= 2;
    }
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if active(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low + 1
}

/// The Lua frames above an engine function, for a report it makes itself.
pub(crate) fn caller_stack(lua: &Lua) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut level = 1;
    while let Some(f) = lua.inspect_stack(level, |d| Frame {
        name: d
            .names()
            .name
            .map_or_else(|| "unknown".into(), |s| s.into_owned()),
        source: d
            .source()
            .short_src
            .map_or_else(|| "?".into(), |s| s.into_owned()),
        line: d.current_line().map_or(-1, |l| l as i64),
    }) {
        out.push(f);
        level += 1;
        if level > 64 {
            break;
        }
    }
    out
}

/// The message handler of `protected_call`: the error's text and the frames between it and
/// the `xpcall` (the handler, `xpcall`, the caller and the frames below are left out).
fn collect_stack(lua: &Lua, err: Value) -> mlua::Result<Table> {
    let message = match &err {
        Value::Error(e) => clean(e),
        v => String::from_utf8_lossy(&output::tostring(lua, v).unwrap_or_default()).into_owned(),
    };
    let frames = lua.create_table()?;
    let below = state(lua).depths.borrow().last().copied().unwrap_or(0);
    let last = depth(lua).saturating_sub(below + 3).min(64);
    for level in 1..=last {
        let Some((name, source, line)) = lua.inspect_stack(level, |d| {
            let src = d.source();
            (
                d.names().name.map(|s| s.into_owned()),
                src.short_src
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|| "?".into()),
                d.current_line().map_or(-1, |l| l as i64),
            )
        }) else {
            break;
        };
        let fr = lua.create_table()?;
        fr.raw_set("name", name.unwrap_or_else(|| "unknown".into()))?;
        fr.raw_set("source", source)?;
        fr.raw_set("line", line)?;
        frames.raw_set(level, fr)?;
    }
    let t = lua.create_table()?;
    t.raw_set("message", message)?;
    t.raw_set("stack", frames)?;
    Ok(t)
}

/// The caller of `protected_call`: a Lua function that calls the game's own `xpcall`, so
/// that the frame of `xpcall` has a name in tracebacks, with the stack collector.
fn install_caller(lua: &Lua) -> mlua::Result<()> {
    let xpcall: Function = lua.globals().raw_get("xpcall")?;
    let caller: Function = lua
        .load(
            "local xpcall = ...\nlocal function results(...) return ... end\nreturn function(f, handler, ...) return results(xpcall(f, handler, ...)) end",
        )
        .set_name("=fpkg/protected call")
        .call(xpcall)?;
    *state(lua).caller.borrow_mut() = Some((caller, lua.create_function(collect_stack)?));
    Ok(())
}
