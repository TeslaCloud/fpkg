//! `util.TableToJSON` and `util.JSONToTable`, as the game writes and reads JSON.
//!
//! Writing: a table whose keys are exactly 1..n is an array, any other table an object (an
//! empty one is `[]`); keys become strings (numbers as Lua's `tostring` writes them, `1e+15`);
//! a whole number in the 32-bit range is written as an integer (`3`, `0` for `-0`), any other
//! number in its shortest exact form, with `.0` after a whole one (`0.1`, `1e-7`,
//! `123456789012.0`, `1e21`); NaN is written as `-2147483648` and an infinity as nothing at
//! all, as the game writes them; strings are written byte for byte, escaping only `"`, `\`
//! and control characters; values that JSON cannot hold (functions, userdata, keys that are
//! tables) are left out. Pretty output indents by one tab per level.
//!
//! Reading: `//` and `/* */` comments and trailing commas are accepted; arrays become
//! sequences, `null` a missing value; keys that are numbers in decimal form become numbers
//! unless conversions are off; more than 15,000 keys or values in all, or nesting deeper than
//! 512 levels, fails unless the limits are off. Anything invalid gives `nil`.

use std::collections::HashSet;

use nl::mlua::{self, Lua, Table, Value};

/// The keys and values `JSONToTable` reads unless its limits are off.
pub const MAX_ENTRIES: usize = 15_000;
/// The nesting `JSONToTable` reads unless its limits are off.
pub const MAX_DEPTH: usize = 512;
/// The nesting it reads with the limits off: what keeps the reader on its stack.
const HARD_DEPTH: usize = 4096;

/// The options of `util.JSONToTable`.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonOptions {
    pub ignore_limits: bool,
    pub ignore_conversions: bool,
}

/// A number as Lua's `tostring` writes it (`%.14g`).
pub fn lua_number(n: f64) -> String {
    if n.is_nan() {
        return if n.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if n.is_infinite() {
        return if n < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if n == 0.0 {
        return if n.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let e = format!("{n:.13e}");
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..14).contains(&exp) {
        let m = trim_fraction(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{m}e{sign}{:02}", exp.abs());
    }
    let decimals = (13 - exp).max(0) as usize;
    trim_fraction(&format!("{n:.decimals$}")).to_string()
}

fn trim_fraction(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// A number as the game's JSON writer writes it.
pub fn json_number(n: f64) -> String {
    if n.is_nan() {
        return i32::MIN.to_string();
    }
    if n.is_infinite() {
        return String::new();
    }
    if n == n.trunc() && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&n) {
        return (n as i32).to_string();
    }
    // The shortest digits that read back as n, laid out as RapidJSON's Prettify does.
    let e = format!("{n:e}");
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let length = digits.len() as i32;
    let kk = exp + 1;
    let k = kk - length;
    let body = if (0..).contains(&k) && kk <= 21 {
        format!("{digits}{}.0", "0".repeat(k as usize))
    } else if 0 < kk && kk <= 21 {
        format!("{}.{}", &digits[..kk as usize], &digits[kk as usize..])
    } else if -6 < kk && kk <= 0 {
        format!("0.{}{digits}", "0".repeat((-kk) as usize))
    } else if length == 1 {
        format!("{digits}e{}", kk - 1)
    } else {
        format!("{}.{}e{}", &digits[..1], &digits[1..], kk - 1)
    };
    if negative { format!("-{body}") } else { body }
}

fn key_bytes(k: &Value) -> Option<Vec<u8>> {
    match k {
        Value::String(s) => Some(s.as_bytes().to_vec()),
        Value::Integer(i) => Some(lua_number(*i as f64).into_bytes()),
        Value::Number(n) => Some(lua_number(*n).into_bytes()),
        Value::Boolean(b) => Some(b.to_string().into_bytes()),
        _ => None,
    }
}

fn escape(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &b in s {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b if b < 0x20 => out.extend(format!("\\u{b:04X}").into_bytes()),
            b => out.push(b),
        }
    }
    out.push(b'"');
}

/// Whether a table's keys are exactly 1..n, n > 0.
fn sequence_len(t: &Table) -> mlua::Result<Option<usize>> {
    let mut count = 0usize;
    let mut max = 0i64;
    for pair in t.pairs::<Value, Value>() {
        let (k, _) = pair?;
        count += 1;
        match k {
            Value::Integer(i) if i >= 1 => max = max.max(i),
            Value::Number(n) if n >= 1.0 && n == n.trunc() => max = max.max(n as i64),
            _ => return Ok(None),
        }
    }
    Ok((count > 0 && max as usize == count).then_some(count))
}

/// `util.TableToJSON`: `None` for a table that contains itself.
pub fn to_json(t: &Table, pretty: bool) -> mlua::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if !write_table(&mut out, t, pretty, 0, &mut seen)? {
        return Ok(None);
    }
    Ok(Some(out))
}

fn indent(out: &mut Vec<u8>, pretty: bool, depth: usize) {
    if pretty {
        out.push(b'\n');
        out.extend(std::iter::repeat_n(b'\t', depth));
    }
}

fn write_value(
    out: &mut Vec<u8>,
    v: &Value,
    pretty: bool,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> mlua::Result<bool> {
    match v {
        Value::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Integer(i) => out.extend(json_number(*i as f64).into_bytes()),
        Value::Number(n) => out.extend(json_number(*n).into_bytes()),
        Value::String(s) => escape(out, &s.as_bytes()),
        Value::Table(t) => return write_table(out, t, pretty, depth, seen),
        _ => out.extend_from_slice(b"null"),
    }
    Ok(true)
}

fn json_value(v: &Value) -> bool {
    matches!(
        v,
        Value::Boolean(_)
            | Value::Integer(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Table(_)
    )
}

fn write_table(
    out: &mut Vec<u8>,
    t: &Table,
    pretty: bool,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> mlua::Result<bool> {
    let id = t.to_pointer() as usize;
    if !seen.insert(id) {
        return Ok(false);
    }
    if let Some(n) = sequence_len(t)? {
        out.push(b'[');
        let mut first = true;
        for i in 1..=n {
            let v: Value = t.raw_get(i)?;
            if !json_value(&v) {
                continue;
            }
            if !first {
                out.push(b',');
            }
            first = false;
            indent(out, pretty, depth + 1);
            if !write_value(out, &v, pretty, depth + 1, seen)? {
                return Ok(false);
            }
        }
        if !first {
            indent(out, pretty, depth);
        }
        out.push(b']');
    } else {
        let mut entries = Vec::new();
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair?;
            if let Some(k) = key_bytes(&k)
                && json_value(&v)
            {
                entries.push((k, v));
            }
        }
        if entries.is_empty() {
            out.extend_from_slice(b"[]");
        } else {
            out.push(b'{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                indent(out, pretty, depth + 1);
                escape(out, k);
                out.extend_from_slice(if pretty { b": " } else { b":" });
                if !write_value(out, v, pretty, depth + 1, seen)? {
                    return Ok(false);
                }
            }
            indent(out, pretty, depth);
            out.push(b'}');
        }
    }
    seen.remove(&id);
    Ok(true)
}

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(Vec<u8>),
    Array(Vec<Json>),
    Object(Vec<(Vec<u8>, Json)>),
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    entries: usize,
    opts: JsonOptions,
}

impl Parser<'_> {
    fn space(&mut self) -> Option<()> {
        loop {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if self.s[self.i..].starts_with(b"/*") {
                let end = self.s[self.i + 2..].windows(2).position(|w| w == b"*/")?;
                self.i += end + 4;
            } else {
                return Some(());
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn count(&mut self) -> Option<()> {
        self.entries += 1;
        (self.opts.ignore_limits || self.entries <= MAX_ENTRIES).then_some(())
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        let limit = if self.opts.ignore_limits {
            HARD_DEPTH
        } else {
            MAX_DEPTH
        };
        if depth > limit {
            return None;
        }
        self.space()?;
        match self.peek()? {
            b'{' => {
                self.i += 1;
                let mut members = Vec::new();
                loop {
                    self.space()?;
                    if self.peek()? == b'}' {
                        self.i += 1;
                        return Some(Json::Object(members));
                    }
                    let Json::String(k) = self.string()? else {
                        return None;
                    };
                    self.space()?;
                    if self.peek()? != b':' {
                        return None;
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    self.count()?;
                    members.push((k, v));
                    self.space()?;
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {}
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.space()?;
                    if self.peek()? == b']' {
                        self.i += 1;
                        return Some(Json::Array(items));
                    }
                    items.push(self.value(depth + 1)?);
                    self.count()?;
                    self.space()?;
                    match self.peek()? {
                        b',' => self.i += 1,
                        b']' => {}
                        _ => return None,
                    }
                }
            }
            b'"' => self.string(),
            b't' => self.word(b"true", Json::Bool(true)),
            b'f' => self.word(b"false", Json::Bool(false)),
            b'n' => self.word(b"null", Json::Null),
            _ => self.number(),
        }
    }

    fn word(&mut self, w: &[u8], v: Json) -> Option<Json> {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            Some(v)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            p.i > s
        };
        if !digits(self) {
            return None;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !digits(self) {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return None;
            }
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()
            .map(Json::Number)
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<Json> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return Some(Json::String(out)),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.s[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return None;
                                }
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            let ch = char::from_u32(cp)?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return None,
                    }
                }
                c if c < 0x20 => return None,
                c => out.push(c),
            }
        }
    }
}

/// A key in decimal number form, which the game turns into a number.
fn numeric_key(k: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(k).ok()?;
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, rest) = body.split_at(
        body.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(body.len()),
    );
    if int.is_empty() {
        return None;
    }
    let rest = match rest.strip_prefix('.') {
        Some(r) => {
            let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            if n == 0 {
                return None;
            }
            &r[n..]
        }
        None => rest,
    };
    let rest = match rest.strip_prefix(['e', 'E']) {
        Some(r) => {
            let r = r.strip_prefix(['+', '-']).unwrap_or(r);
            if r.is_empty() || !r.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            ""
        }
        None => rest,
    };
    if !rest.is_empty() {
        return None;
    }
    s.parse().ok()
}

fn to_lua(lua: &Lua, v: &Json, opts: JsonOptions) -> mlua::Result<Value> {
    Ok(match v {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(*n),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, item, opts)?)?;
            }
            Value::Table(t)
        }
        Json::Object(members) => {
            let t = lua.create_table_with_capacity(0, members.len())?;
            for (k, item) in members {
                let value = to_lua(lua, item, opts)?;
                match numeric_key(k).filter(|_| !opts.ignore_conversions) {
                    Some(n) => t.raw_set(n, value)?,
                    None => t.raw_set(lua.create_string(k)?, value)?,
                }
            }
            Value::Table(t)
        }
    })
}

/// `util.JSONToTable`: `None` when the text is not JSON holding an object or array, or
/// passes the limits.
pub fn from_json(lua: &Lua, text: &[u8], opts: JsonOptions) -> mlua::Result<Option<Table>> {
    let mut p = Parser {
        s: text,
        i: 0,
        entries: 0,
        opts,
    };
    let Some(v) = p.value(0) else {
        return Ok(None);
    };
    if p.space().is_none() || p.i != text.len() {
        return Ok(None);
    }
    match (&v, to_lua(lua, &v, opts)?) {
        (Json::Array(_) | Json::Object(_), Value::Table(t)) => Ok(Some(t)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua() -> Lua {
        Lua::new()
    }

    fn eval(lua: &Lua, code: &str) -> Table {
        lua.load(code).eval().unwrap()
    }

    fn json(lua: &Lua, code: &str) -> String {
        String::from_utf8(to_json(&eval(lua, code), false).unwrap().unwrap()).unwrap()
    }

    /// The members of a JSON text at its top level, split at the commas outside strings
    /// and brackets.
    fn members(inner: &str) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        let (mut depth, mut in_string, mut escaped) = (0, false, false);
        let mut cur = String::new();
        for c in inner.chars() {
            if in_string {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
            } else {
                match c {
                    '"' => in_string = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => depth -= 1,
                    ',' if depth == 0 => {
                        parts.push(std::mem::take(&mut cur));
                        continue;
                    }
                    _ => {}
                }
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
        parts
    }

    /// A compact JSON text with the members of every object in a fixed order, so that a
    /// result can be compared with what the game wrote in another key order; numbers and
    /// strings keep their text.
    fn canonical(s: &str) -> String {
        if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            let mut entries: Vec<String> = members(inner)
                .iter()
                .map(|m| {
                    let colon = m.find("\":").map_or(0, |i| i + 1);
                    format!("{}:{}", &m[..colon], canonical(&m[colon + 1..]))
                })
                .collect();
            entries.sort();
            return format!("{{{}}}", entries.join(","));
        }
        if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            let items: Vec<String> = members(inner).iter().map(|m| canonical(m)).collect();
            return format!("[{}]", items.join(","));
        }
        s.to_string()
    }

    #[test]
    fn lua_numbers_print_as_lua_prints_them() {
        for (n, s) in [
            (1.0, "1"),
            (-2.0, "-2"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.33333333333333"),
            (1e15, "1e+15"),
            (123456789012345.0, "1.2345678901234e+14"),
            (12345678901234.0, "12345678901234"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (2.5e300, "2.5e+300"),
        ] {
            assert_eq!(lua_number(n), s, "{n}");
        }
    }

    #[test]
    fn json_numbers_as_the_game_writes_them() {
        for (n, s) in [
            (3.0, "3"),
            (-7.0, "-7"),
            (-0.0, "0"),
            (2147483647.0, "2147483647"),
            (-2147483648.0, "-2147483648"),
            (2147483648.0, "2147483648.0"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (123.456, "123.456"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (1.25e-9, "1.25e-9"),
            (9007199254740994.0, "9007199254740994.0"),
            (1e15, "1000000000000000.0"),
            (123456789012.0, "123456789012.0"),
            (76561198000000000.0, "76561198000000000.0"),
            (1e20, "100000000000000000000.0"),
            (1e21, "1e21"),
            (1.5e21, "1.5e21"),
            (1.5e300, "1.5e300"),
            (f64::NAN, "-2147483648"),
            (f64::INFINITY, ""),
            (f64::NEG_INFINITY, ""),
        ] {
            assert_eq!(json_number(n), s, "{n}");
        }
    }

    #[test]
    fn what_the_game_wrote() {
        let lua = lua();
        assert_eq!(json(&lua, "return {1, 2, 3}"), "[1,2,3]");
        assert_eq!(json(&lua, "return {}"), "[]");
        assert_eq!(json(&lua, "return {a = 1}"), r#"{"a":1}"#);
        assert_eq!(json(&lua, "return { [1e15] = 1 }"), r#"{"1e+15":1}"#);
        assert_eq!(json(&lua, "return { [1.5] = 2 }"), r#"{"1.5":2}"#);
        assert_eq!(json(&lua, "return { [1e21] = 3 }"), r#"{"1e+21":3}"#);
        assert_eq!(json(&lua, "return { [5] = 'x' }"), r#"{"5":"x"}"#);
        assert_eq!(
            json(
                &lua,
                "return { 1e21, 0.1, -0, 2 ^ 53 + 2, 1e15, 123456789012, 1.0e-7 }"
            ),
            "[1e21,0.1,0,9007199254740994.0,1000000000000000.0,123456789012.0,1e-7]"
        );
        assert_eq!(
            json(&lua, "return { e = 76561198000000000 }"),
            r#"{"e":76561198000000000.0}"#
        );
        assert_eq!(
            json(&lua, "return { 0 / 0, 1 / 0, -1 / 0 }"),
            "[-2147483648,,]"
        );
        let bytes = to_json(&eval(&lua, "return { '\\255\\254' }"), false)
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"[\"\xff\xfe\"]");
        let back = from_json(&lua, &bytes, JsonOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            back.raw_get::<nl::mlua::LuaString>(1)
                .unwrap()
                .as_bytes()
                .to_vec(),
            b"\xff\xfe"
        );
        let keyed = to_json(&eval(&lua, "return { ['\\255'] = 1 }"), false)
            .unwrap()
            .unwrap();
        assert_eq!(keyed, b"{\"\xff\":1}");
        assert_eq!(
            canonical(&json(
                &lua,
                r#"return { a = {}, b = { [1] = 1, [3] = 3 }, c = { ['1'] = 'x', [2] = 'y' }, d = 'é\n"\\/', e = 76561198000000000, f = '76561198000000000' }"#
            )),
            canonical(
                r#"{"b":{"3":3,"1":1},"f":"76561198000000000","d":"é\n\"\\/","c":{"1":"x","2":"y"},"a":[],"e":76561198000000000.0}"#
            )
        );
        let b = json(&lua, "return { b = { [1] = 1, [3] = 3 } }");
        assert!(
            b == r#"{"b":{"1":1,"3":3}}"# || b == r#"{"b":{"3":3,"1":1}}"#,
            "{b}"
        );
        assert_eq!(
            json(&lua, "return {{1}, {b = true}}"),
            r#"[[1],{"b":true}]"#
        );
        assert_eq!(
            json(&lua, "return {f = print, s = 'q\"\\n'}"),
            r#"{"s":"q\"\n"}"#
        );
        assert_eq!(json(&lua, "return {'a\\1b'}"), "[\"a\\u0001b\"]");
        assert_eq!(json(&lua, "return {'é'}"), "[\"é\"]");
        let t = eval(&lua, "local t = {} t.self = t return t");
        assert_eq!(to_json(&t, false).unwrap(), None);
    }

    #[test]
    fn pretty_output_indents_by_tabs() {
        let lua = lua();
        let pretty = to_json(&eval(&lua, "return { a = 1, b = { 1, 2 }, c = {} }"), true)
            .unwrap()
            .unwrap();
        let pretty = String::from_utf8(pretty).unwrap();
        let lines: Vec<&str> = pretty.lines().collect();
        assert_eq!((lines[0], lines[lines.len() - 1]), ("{", "}"));
        let body = lines[1..lines.len() - 1].join("\n");
        let mut entries: Vec<String> = body
            .split(",\n\t\"")
            .map(|e| {
                e.trim_start_matches('\t')
                    .trim_start_matches('"')
                    .to_string()
            })
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            ["a\": 1", "b\": [\n\t\t1,\n\t\t2\n\t]", "c\": []"],
            "{pretty}"
        );
        assert_eq!(
            String::from_utf8(
                to_json(&eval(&lua, "return { b = { 1, 2 } }"), true)
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            "{\n\t\"b\": [\n\t\t1,\n\t\t2\n\t]\n}"
        );
    }

    #[test]
    fn reading_converts_numeric_keys_and_accepts_comments() {
        let lua = lua();
        let t = from_json(
            &lua,
            br#"{"1":"a","b":[1,2.5,"x"],"-3":true,"1.5":4}"#,
            JsonOptions::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(t.raw_get::<String>(1).unwrap(), "a");
        assert!(t.raw_get::<bool>(-3).unwrap());
        assert_eq!(t.raw_get::<f64>(1.5).unwrap(), 4.0);
        let b: Table = t.raw_get("b").unwrap();
        assert_eq!(b.raw_len(), 3);
        assert_eq!(b.raw_get::<f64>(2).unwrap(), 2.5);
        let kept = from_json(
            &lua,
            br#"{"76561198000000000":1}"#,
            JsonOptions {
                ignore_conversions: true,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(kept.raw_get::<f64>("76561198000000000").unwrap(), 1.0);
        let c = from_json(
            &lua,
            b"// x\n{ /* y */ \"a\": [1, 2,], }",
            JsonOptions::default(),
        )
        .unwrap()
        .unwrap();
        let a: Table = c.raw_get("a").unwrap();
        assert_eq!(a.raw_len(), 2);
        for bad in [
            &b"{"[..],
            b"[1,]x",
            b"{a:1}",
            b"5",
            b"\"s\"",
            b"",
            b"[\"\\q\"]",
        ] {
            assert!(
                from_json(&lua, bad, JsonOptions::default())
                    .unwrap()
                    .is_none(),
                "{bad:?}"
            );
        }
        let s = from_json(&lua, br#"["\u00e9\ud83d\ude00"]"#, JsonOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(s.raw_get::<String>(1).unwrap(), "é😀");
        let n = from_json(&lua, b"[1,null,3]", JsonOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(n.raw_get::<f64>(3).unwrap(), 3.0);
        assert!(n.raw_get::<Value>(2).unwrap().is_nil());
    }

    #[test]
    fn limits() {
        let lua = lua();
        let many = format!("[{}]", vec!["1"; MAX_ENTRIES + 1].join(","));
        assert!(
            from_json(&lua, many.as_bytes(), JsonOptions::default())
                .unwrap()
                .is_none()
        );
        let off = JsonOptions {
            ignore_limits: true,
            ..Default::default()
        };
        assert!(from_json(&lua, many.as_bytes(), off).unwrap().is_some());
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert!(
            from_json(&lua, deep.as_bytes(), JsonOptions::default())
                .unwrap()
                .is_none()
        );
        assert!(from_json(&lua, deep.as_bytes(), off).unwrap().is_some());
    }

    #[test]
    fn round_trips_and_what_the_game_read() {
        let lua = lua();
        for code in [
            "return {1, 2, 3}",
            "return {a = {b = {c = 'd'}}, n = -0.5, big = 2^53}",
            "return {[1] = 'x', [5] = 'y', z = false}",
            "return {{1, {2, {3}}}, {}}",
        ] {
            let t = eval(&lua, code);
            let s = to_json(&t, false).unwrap().unwrap();
            let back = from_json(&lua, &s, JsonOptions::default())
                .unwrap()
                .unwrap();
            let again = to_json(&back, false).unwrap().unwrap();
            let (s, again) = (
                String::from_utf8(s).unwrap(),
                String::from_utf8(again).unwrap(),
            );
            if s.starts_with('{') {
                assert_eq!(canonical(&again), canonical(&s), "{code}");
            } else {
                assert_eq!(again, s, "{code}");
            }
        }
        let read = |text: &str, opts| {
            let t = from_json(&lua, text.as_bytes(), opts).unwrap().unwrap();
            String::from_utf8(to_json(&t, false).unwrap().unwrap()).unwrap()
        };
        assert_eq!(
            canonical(&read(
                r#"{"1":"a","2":"b","x":{"5":1},"n":76561198000000000,"s":"76561198000000000","f":1.0,"g":[1,null,3]}"#,
                JsonOptions::default()
            )),
            canonical(
                r#"{"1":"a","2":"b","f":1,"s":"76561198000000000","n":76561198000000000.0,"x":{"5":1},"g":{"1":1,"3":3}}"#
            )
        );
        assert_eq!(
            canonical(&read(
                r#"{"1":"a","2.5":"b","76561198000000000":"c"}"#,
                JsonOptions {
                    ignore_conversions: true,
                    ..Default::default()
                }
            )),
            canonical(r#"{"76561198000000000":"c","1":"a","2.5":"b"}"#)
        );
    }
}
