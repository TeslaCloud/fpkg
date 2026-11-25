//! The engine's `timer` library on the runtime's loop.
//!
//! A timer fires at the first loop turn at or after its time; a turn runs only the timers
//! that were due when it began, so one created during a turn (even with a delay of 0) fires
//! at the next. A named timer created again is replaced and starts over; repetitions of 0 or
//! less repeat forever. A timer is removed after its last repetition has run: during that
//! one it still exists, with 0 repetitions left, and `Adjust` with repetitions (which sets
//! the repetitions left) keeps it. A timer whose function raises an error is removed, and the
//! error reported once. `Stop` rewinds a timer (time and repetitions) until
//! `Start`; `Pause` keeps the time left, which `TimeLeft` gives as a negative number while
//! paused.

use std::rc::Rc;

use nl::mlua::{self, Function, Lua, MultiValue, RegistryKey, Value};

use crate::runtime::{
    State, arg, as_number, bad_argument, check_number, check_string, protected_call, state,
};

/// Which timer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Id {
    Named(String),
    Simple(u64),
}

struct Timer {
    id: Id,
    delay: f64,
    reps: u64,
    done: u64,
    next: f64,
    func: Rc<RegistryKey>,
    /// Time left while paused.
    paused: Option<f64>,
    stopped: bool,
    order: u64,
}

/// Every timer of a runtime.
#[derive(Default)]
pub(crate) struct Timers {
    list: Vec<Timer>,
    seq: u64,
}

impl Timers {
    fn find(&mut self, name: &str) -> Option<&mut Timer> {
        self.list
            .iter_mut()
            .find(|t| matches!(&t.id, Id::Named(n) if n == name))
    }

    fn add(&mut self, id: Id, delay: f64, reps: u64, func: RegistryKey, now: f64) {
        if let Id::Named(n) = &id {
            self.list
                .retain(|t| !matches!(&t.id, Id::Named(x) if x == n));
        }
        self.seq += 1;
        self.list.push(Timer {
            id,
            delay,
            reps,
            done: 0,
            next: now + delay.max(0.0),
            func: Rc::new(func),
            paused: None,
            stopped: false,
            order: self.seq,
        });
    }

    /// A function to call at the next turn.
    pub fn defer(&mut self, func: RegistryKey) {
        self.seq += 1;
        let id = Id::Simple(self.seq);
        self.list.push(Timer {
            id,
            delay: 0.0,
            reps: 1,
            done: 0,
            next: f64::NEG_INFINITY,
            func: Rc::new(func),
            paused: None,
            stopped: false,
            order: self.seq,
        });
    }

    /// Whether a timer runs when it is due: not paused, stopped or on its last repetition.
    fn active(t: &Timer) -> bool {
        t.paused.is_none() && !t.stopped && !(t.reps > 0 && t.done >= t.reps)
    }

    /// Whether a timer is due at `now`.
    pub fn due_now(&self, now: f64) -> bool {
        self.list.iter().any(|t| Self::active(t) && t.next <= now)
    }

    /// When the next timer is due.
    pub fn next_due(&self) -> Option<f64> {
        self.list
            .iter()
            .filter(|t| Self::active(t))
            .map(|t| t.next)
            .fold(None, |a, b| Some(a.map_or(b, |a: f64| a.min(b))))
    }
}

/// Runs the timers due at the start of the turn. Returns whether any ran.
pub(crate) fn run_due(lua: &Lua, st: &Rc<State>) -> bool {
    let now = st.now();
    let mut due: Vec<(f64, u64)> = st
        .timers
        .borrow()
        .list
        .iter()
        .filter(|t| Timers::active(t) && t.next <= now)
        .map(|t| (t.next, t.order))
        .collect();
    due.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut ran = false;
    for (_, order) in due {
        let func = {
            let mut timers = st.timers.borrow_mut();
            let Some(i) = timers.list.iter().position(|t| t.order == order) else {
                continue;
            };
            let t = &mut timers.list[i];
            if !Timers::active(t) {
                continue;
            }
            t.done += 1;
            t.next = now + t.delay.max(0.0);
            t.func.clone()
        };
        ran = true;
        let failed = match lua.registry_value::<Function>(&func) {
            Ok(f) => match protected_call(lua, &f, MultiValue::new()) {
                Ok(_) => false,
                Err(e) => {
                    st.report(&e);
                    true
                }
            },
            Err(_) => true,
        };
        st.timers
            .borrow_mut()
            .list
            .retain(|t| t.order != order || !(failed || t.reps > 0 && t.done >= t.reps));
    }
    ran
}

fn name_arg(lua: &Lua, args: &MultiValue) -> mlua::Result<String> {
    check_string(lua, args, 1)
}

fn func_arg(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Function> {
    match arg(args, n) {
        Value::Function(f) => Ok(f),
        _ => Err(bad_argument(lua, args, n, "function")),
    }
}

fn reps_of(v: &Value) -> u64 {
    as_number(v).map_or(0, |n| if n <= 0.0 { 0 } else { n as u64 })
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let timer = lua.create_table()?;
    timer.set(
        "Create",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let delay = check_number(lua, &args, 2)?;
            let reps = reps_of(&arg(&args, 3));
            let f = func_arg(lua, &args, 4)?;
            let st = state(lua);
            let key = lua.create_registry_value(f)?;
            st.timers
                .borrow_mut()
                .add(Id::Named(name), delay, reps, key, st.now());
            Ok(())
        })?,
    )?;
    timer.set(
        "Simple",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let delay = check_number(lua, &args, 1)?;
            let f = func_arg(lua, &args, 2)?;
            let st = state(lua);
            let key = lua.create_registry_value(f)?;
            let mut timers = st.timers.borrow_mut();
            let id = Id::Simple(timers.seq + 1);
            timers.add(id, delay, 1, key, st.now());
            Ok(())
        })?,
    )?;
    timer.set(
        "Adjust",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let delay = check_number(lua, &args, 2)?;
            let reps = arg(&args, 3);
            let f = match arg(&args, 4) {
                Value::Function(f) => Some(lua.create_registry_value(f)?),
                _ => None,
            };
            let st = state(lua);
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            t.next = t.next - t.delay.max(0.0) + delay.max(0.0);
            t.delay = delay;
            if !reps.is_nil() {
                t.reps = reps_of(&reps);
                t.done = 0;
            }
            if let Some(f) = f {
                t.func = Rc::new(f);
            }
            Ok(true)
        })?,
    )?;
    let remove = crate::runtime::function(lua, |lua, args: MultiValue| {
        let name = name_arg(lua, &args)?;
        state(lua)
            .timers
            .borrow_mut()
            .list
            .retain(|t| !matches!(&t.id, Id::Named(n) if *n == name));
        Ok(())
    })?;
    timer.set("Remove", remove.clone())?;
    timer.set("Destroy", remove)?;
    timer.set(
        "Exists",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            Ok(state(lua).timers.borrow_mut().find(&name).is_some())
        })?,
    )?;
    timer.set(
        "Start",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let now = st.now();
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            t.stopped = false;
            t.paused = None;
            t.done = 0;
            t.next = now + t.delay.max(0.0);
            Ok(true)
        })?,
    )?;
    timer.set(
        "Stop",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            if t.stopped {
                return Ok(false);
            }
            t.stopped = true;
            t.paused = None;
            t.done = 0;
            Ok(true)
        })?,
    )?;
    timer.set(
        "Pause",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let now = st.now();
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            if t.paused.is_some() || t.stopped {
                return Ok(false);
            }
            t.paused = Some((t.next - now).max(0.0));
            Ok(true)
        })?,
    )?;
    timer.set(
        "UnPause",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let now = st.now();
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            match t.paused.take() {
                Some(left) => {
                    t.next = now + left;
                    Ok(true)
                }
                None => Ok(false),
            }
        })?,
    )?;
    timer.set(
        "Toggle",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let now = st.now();
            let mut timers = st.timers.borrow_mut();
            let Some(t) = timers.find(&name) else {
                return Ok(false);
            };
            match t.paused.take() {
                Some(left) => t.next = now + left,
                None => t.paused = Some((t.next - now).max(0.0)),
            }
            Ok(true)
        })?,
    )?;
    timer.set(
        "IsPaused",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            Ok(state(lua)
                .timers
                .borrow_mut()
                .find(&name)
                .is_some_and(|t| t.paused.is_some()))
        })?,
    )?;
    timer.set(
        "TimeLeft",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            let st = state(lua);
            let now = st.now();
            let mut timers = st.timers.borrow_mut();
            Ok(timers.find(&name).map(|t| match t.paused {
                Some(left) => -left,
                None if t.stopped => t.delay,
                None => (t.next - now).max(0.0),
            }))
        })?,
    )?;
    timer.set(
        "RepsLeft",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = name_arg(lua, &args)?;
            Ok(state(lua)
                .timers
                .borrow_mut()
                .find(&name)
                .map(|t| t.reps.saturating_sub(t.done)))
        })?,
    )?;
    timer.set(
        "Check",
        crate::runtime::function(lua, |_, _: MultiValue| Ok(()))?,
    )?;
    lua.globals().set("timer", timer)
}
