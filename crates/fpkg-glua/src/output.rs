//! Where the console goes: `print`, `Msg`, `MsgN`, `MsgC` to standard output, `ErrorNoHalt`
//! and reported Lua errors to standard error, both to standard error, or both into a buffer.
//!
//! A stream that does not take escape sequences (one that is not a terminal, or any stream
//! while `NO_COLOR` is set, as the host decides) gets everything with its ANSI escape
//! sequences stripped, so that colored text reads cleanly in logs and CI.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use nl::mlua::{self, Lua, MultiValue, Value};

/// The console of a runtime.
#[derive(Clone, Debug)]
pub enum Output {
    /// The process's standard output and standard error; each flag says whether that stream
    /// takes escape sequences.
    Stdio { color_out: bool, color_err: bool },
    /// Both into standard error, for a host whose standard output carries a result of its
    /// own (a listing, JSON).
    Stderr { color: bool },
    /// Both kept in memory, in order, for tests and hosts that show them later; `color` says
    /// whether escape sequences are kept.
    Capture {
        console: Rc<RefCell<Captured>>,
        color: bool,
    },
}

impl Default for Output {
    fn default() -> Output {
        Output::Stdio {
            color_out: false,
            color_err: false,
        }
    }
}

/// Which stream a piece of the console went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// What a captured console received, in order.
#[derive(Clone, Debug, Default)]
pub struct Captured {
    chunks: Vec<(Stream, Vec<u8>)>,
}

impl Captured {
    fn joined(&self, stream: Stream) -> Vec<u8> {
        self.chunks
            .iter()
            .filter(|(s, _)| *s == stream)
            .flat_map(|(_, b)| b.iter().copied())
            .collect()
    }

    /// Everything written to standard output.
    pub fn out(&self) -> Vec<u8> {
        self.joined(Stream::Out)
    }

    /// Everything written to standard error.
    pub fn err(&self) -> Vec<u8> {
        self.joined(Stream::Err)
    }

    pub fn out_text(&self) -> String {
        String::from_utf8_lossy(&self.out()).into_owned()
    }

    pub fn err_text(&self) -> String {
        String::from_utf8_lossy(&self.err()).into_owned()
    }

    /// Writes what was captured to another console, in the order it came.
    pub fn replay(&self, to: &Output) {
        for (stream, bytes) in &self.chunks {
            to.write(*stream, bytes);
        }
    }
}

impl Output {
    /// A new captured console that keeps or strips escape sequences, and its buffer.
    pub fn capture_with(color: bool) -> (Output, Rc<RefCell<Captured>>) {
        let console = Rc::new(RefCell::new(Captured::default()));
        (
            Output::Capture {
                console: console.clone(),
                color,
            },
            console,
        )
    }

    /// A new captured console without escape sequences, and its buffer.
    pub fn capture() -> (Output, Rc<RefCell<Captured>>) {
        Output::capture_with(false)
    }

    /// Whether what goes to `stream` keeps its escape sequences.
    fn color(&self, stream: Stream) -> bool {
        match (self, stream) {
            (Output::Stdio { color_out, .. }, Stream::Out) => *color_out,
            (Output::Stdio { color_err, .. }, Stream::Err) => *color_err,
            (Output::Stderr { color }, _) | (Output::Capture { color, .. }, _) => *color,
        }
    }

    pub(crate) fn write(&self, stream: Stream, bytes: &[u8]) {
        let stripped;
        let bytes = if self.color(stream) {
            bytes
        } else {
            stripped = strip_escapes(bytes);
            &stripped
        };
        match (self, stream) {
            (Output::Stdio { .. }, Stream::Out) => {
                let _ = std::io::stdout().lock().write_all(bytes);
            }
            (Output::Stdio { .. }, Stream::Err) | (Output::Stderr { .. }, _) => {
                let _ = std::io::stdout().flush();
                let mut e = std::io::stderr().lock();
                let _ = e.write_all(bytes);
                let _ = e.flush();
            }
            (Output::Capture { console, .. }, _) => {
                let mut c = console.borrow_mut();
                match c.chunks.last_mut() {
                    Some((s, b)) if *s == stream => b.extend_from_slice(bytes),
                    _ => c.chunks.push((stream, bytes.to_vec())),
                }
            }
        }
    }

    pub(crate) fn out(&self, bytes: &[u8]) {
        self.write(Stream::Out, bytes);
    }

    pub(crate) fn err(&self, bytes: &[u8]) {
        self.write(Stream::Err, bytes);
    }

    pub(crate) fn flush(&self) {
        if let Output::Stdio { .. } | Output::Stderr { .. } = self {
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
        }
    }
}

/// The bytes without their well-formed ANSI escape sequences: control sequences (`ESC [`,
/// parameter bytes 0x30-0x3F, intermediate bytes 0x20-0x2F, one final byte 0x40-0x7E),
/// operating system commands (`ESC ]` up to `BEL` or `ESC \\`) and two-byte escapes (`ESC`
/// and a byte 0x40-0x5F). Anything else stays as it is, an `ESC` that starts no such
/// sequence included. Each write is stripped on its own: a sequence split across two writes
/// passes through.
pub fn strip_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b
            && let Some(n) = escape_len(&bytes[i..])
        {
            i += n;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The length of the well-formed escape sequence `b` starts with (`b[0]` is `ESC`).
fn escape_len(b: &[u8]) -> Option<usize> {
    match *b.get(1)? {
        b'[' => {
            let mut j = 2;
            while matches!(b.get(j), Some(0x30..=0x3f)) {
                j += 1;
            }
            while matches!(b.get(j), Some(0x20..=0x2f)) {
                j += 1;
            }
            matches!(b.get(j), Some(0x40..=0x7e)).then_some(j + 1)
        }
        b']' => (2..b.len()).find_map(|j| match b[j] {
            0x07 => Some(j + 1),
            0x1b if b.get(j + 1) == Some(&b'\\') => Some(j + 2),
            _ => None,
        }),
        0x40..=0x5f => Some(2),
        _ => None,
    }
}

/// `tostring(v)` through the state's own `tostring`, so that `__tostring` applies.
pub(crate) fn tostring(lua: &Lua, v: &Value) -> mlua::Result<Vec<u8>> {
    match v {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        _ => {
            let _level = crate::runtime::CLevel::enter_or_error()?;
            let f: mlua::Function = lua.globals().get("tostring")?;
            let s: Value = f.call(v.clone())?;
            match s {
                Value::String(s) => Ok(s.as_bytes().to_vec()),
                other => Ok(format!("{other:?}").into_bytes()),
            }
        }
    }
}

/// The arguments converted with `tostring` and joined by `sep`.
pub(crate) fn join(lua: &Lua, args: &MultiValue, sep: &[u8]) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::new();
    for (i, v) in args.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        out.extend(tostring(lua, v)?);
    }
    Ok(out)
}

/// The color a `MsgC` argument sets: a table with numeric `r`, `g` and `b`.
pub(crate) fn color_of(v: &Value) -> Option<(u8, u8, u8)> {
    let Value::Table(t) = v else {
        return None;
    };
    let c = |k: &str| -> Option<u8> {
        match t.raw_get::<Value>(k).ok()? {
            Value::Integer(i) => Some(i.clamp(0, 255) as u8),
            Value::Number(n) => Some(n.clamp(0.0, 255.0) as u8),
            _ => None,
        }
    };
    Some((c("r")?, c("g")?, c("b")?))
}

/// `print`, `Msg`, `MsgN`, `MsgC`, `MsgAll`, `ServerLog`, `ErrorNoHalt` and
/// `ErrorNoHaltWithStack`. The last two count as messages, not as reported errors.
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set(
        "print",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let mut s = join(lua, &args, b"\t")?;
            if let Some(nul) = s.iter().position(|b| *b == 0) {
                s.truncate(nul);
            }
            s.push(b'\n');
            crate::runtime::state(lua).out(&s);
            Ok(())
        })?,
    )?;
    let msg = crate::runtime::function(lua, |lua, args: MultiValue| {
        let s = join(lua, &args, b"")?;
        crate::runtime::state(lua).out(&s);
        Ok(())
    })?;
    g.set("Msg", msg.clone())?;
    g.set("MsgAll", msg.clone())?;
    g.set("ServerLog", msg)?;
    g.set(
        "MsgN",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let mut s = join(lua, &args, b"")?;
            s.push(b'\n');
            crate::runtime::state(lua).out(&s);
            Ok(())
        })?,
    )?;
    g.set(
        "MsgC",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let st = crate::runtime::state(lua);
            let mut s = Vec::new();
            let mut colored = false;
            for v in args.iter() {
                if let Some((r, g, b)) = color_of(v) {
                    if st.color {
                        s.extend(format!("\x1b[38;2;{r};{g};{b}m").into_bytes());
                        colored = true;
                    }
                } else {
                    s.extend(tostring(lua, v)?);
                }
            }
            if colored {
                s.extend_from_slice(b"\x1b[0m");
            }
            st.out(&s);
            Ok(())
        })?,
    )?;
    g.set(
        "ErrorNoHalt",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let s = join(lua, &args, b"")?;
            let st = crate::runtime::state(lua);
            st.messages.set(st.messages.get() + 1);
            st.err(&s);
            Ok(())
        })?,
    )?;
    g.set(
        "ErrorNoHaltWithStack",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let mut s = join(lua, &args, b"")?;
            let tb = lua.traceback(None, 1)?;
            s.push(b'\n');
            s.extend_from_slice(&tb.as_bytes());
            s.push(b'\n');
            let st = crate::runtime::state(lua);
            st.messages.set(st.messages.get() + 1);
            st.err(&s);
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_stripped() {
        assert_eq!(
            strip_escapes(b"\x1b[31mred\x1b[0m plain \x1b[38;2;1;2;3mrgb\x1b[0m"),
            b"red plain rgb"
        );
        assert_eq!(strip_escapes(b"\x1b[?25l\x1b[2 qx"), b"x");
        assert_eq!(strip_escapes(b"\x1b]0;title\x07x\x1b]8;;u\x1b\\y"), b"xy");
        assert_eq!(strip_escapes(b"a\x1bMb\x1b7c"), b"ab\x1b7c");
        assert_eq!(strip_escapes("é\tok\n".as_bytes()), "é\tok\n".as_bytes());
    }

    #[test]
    fn what_starts_no_sequence_stays() {
        for kept in [
            &b"lone esc\x1b"[..],
            b"before a newline\x1b\nnext",
            b"\x1b\xc3\xa9",
            b"\x1b[12;\nnext",
            b"\x1b[12",
            b"\x1b]0;no end\n",
            b"\x1bc",
            b"[31m without esc",
        ] {
            assert_eq!(strip_escapes(kept), kept, "{kept:?}");
        }
    }

    #[test]
    fn a_sequence_split_across_writes_passes_through() {
        let (out, console) = Output::capture();
        out.out(b"\x1b[3");
        out.out(b"1mred");
        assert_eq!(console.borrow().out(), b"\x1b[31mred");
    }

    #[test]
    fn a_capture_keeps_the_order_and_strips_unless_it_keeps_colors() {
        let (plain, console) = Output::capture();
        plain.out(b"\x1b[1ma");
        plain.err(b"b");
        plain.out(b"c");
        assert_eq!(console.borrow().out(), b"ac");
        assert_eq!(console.borrow().err(), b"b");
        let (colored, kept) = Output::capture_with(true);
        console.borrow().replay(&colored);
        colored.out(b"\x1b[0m");
        let kept = kept.borrow();
        assert_eq!(
            kept.chunks,
            [
                (Stream::Out, b"a".to_vec()),
                (Stream::Err, b"b".to_vec()),
                (Stream::Out, b"c\x1b[0m".to_vec()),
            ]
        );
    }
}
