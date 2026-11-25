//! Valve's KeyValues text: `util.KeyValuesToTable` and `util.TableToKeyValues`.
//!
//! Reading follows the engine's parser: quoted or bare tokens, `{` and `}`, `//` comments,
//! conditions such as `[$WIN32]` skipped, `#include` and `#base` lines ignored. A value that
//! is a whole number in the 32-bit range (as `strtol` reads it) becomes a number (`007` is 7;
//! `4294967296` stays a string), one that reads further as a decimal number (`strtod`,
//! without hexadecimal) becomes a single precision number, anything else stays a string.
//! Keys are lowercased unless their case is kept; of two equal keys the later wins. Blocks
//! nested deeper than `MAX_DEPTH` make the text unreadable.
//!
//! Writing: one key and value per line, a block's entries indented by a tab; numbers as
//! Lua's `tostring` writes them (`1e+20`); booleans and values of other types are left out.

use nl::mlua::{self, Lua, Table, Value};

/// How deep blocks may nest in a text `KeyValuesToTable` reads.
pub const MAX_DEPTH: usize = 512;

/// The entries of a block: key and value, in order.
pub type Entries = Vec<(Vec<u8>, KeyValue)>;

/// A parsed entry.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyValue {
    String(Vec<u8>),
    Int(i32),
    Float(f32),
    Block(Entries),
}

#[derive(Debug, PartialEq)]
enum Token {
    Open,
    Close,
    Text(Vec<u8>, bool),
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    escapes: bool,
}

impl Lexer<'_> {
    fn next(&mut self) -> Option<Token> {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            if self.s[self.i..].starts_with(b"#include") || self.s[self.i..].starts_with(b"#base") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            break;
        }
        let c = *self.s.get(self.i)?;
        match c {
            b'{' => {
                self.i += 1;
                Some(Token::Open)
            }
            b'}' => {
                self.i += 1;
                Some(Token::Close)
            }
            b'"' => {
                self.i += 1;
                let mut out = Vec::new();
                while let Some(&c) = self.s.get(self.i) {
                    self.i += 1;
                    if c == b'"' {
                        break;
                    }
                    if c == b'\\'
                        && self.escapes
                        && let Some(&e) = self.s.get(self.i)
                    {
                        self.i += 1;
                        out.push(match e {
                            b'n' => b'\n',
                            b't' => b'\t',
                            b'v' => 0x0b,
                            b'b' => 0x08,
                            b'r' => b'\r',
                            b'f' => 0x0c,
                            b'a' => 0x07,
                            other => other,
                        });
                        continue;
                    }
                    out.push(c);
                }
                Some(Token::Text(out, false))
            }
            b'[' => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != b']' {
                    self.i += 1;
                }
                self.i = (self.i + 1).min(self.s.len());
                Some(Token::Text(self.s[start..self.i].to_vec(), true))
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len()
                    && !self.s[self.i].is_ascii_whitespace()
                    && !matches!(self.s[self.i], b'{' | b'}' | b'"')
                {
                    self.i += 1;
                }
                Some(Token::Text(self.s[start..self.i].to_vec(), false))
            }
        }
    }

    /// The next token that is not a condition.
    fn token(&mut self) -> Option<Token> {
        loop {
            match self.next()? {
                Token::Text(_, true) => continue,
                t => return Some(t),
            }
        }
    }
}

/// `strtol` in base 10 over the whole text: `Some` when it reads all of it without overflow.
fn strtol_len(s: &[u8]) -> (usize, Option<i64>) {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let sign_at = i;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let digits = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits {
        return (0, None);
    }
    let text = std::str::from_utf8(&s[sign_at..i]).unwrap_or("");
    (i, text.parse::<i64>().ok())
}

/// How much of the text `strtod` reads, and the number.
fn strtod_len(s: &[u8]) -> (usize, f64) {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let rest = &s[i..];
    let lower: Vec<u8> = rest.iter().take(8).map(u8::to_ascii_lowercase).collect();
    for word in [&b"infinity"[..], b"inf", b"nan"] {
        if lower.starts_with(word) {
            let neg = s.get(start) == Some(&b'-');
            let v = if word == b"nan" {
                f64::NAN
            } else {
                f64::INFINITY
            };
            return (i + word.len(), if neg { -v } else { v });
        }
    }
    let mantissa = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - mantissa;
    let mut frac_digits = 0;
    if i < s.len() && s[i] == b'.' {
        let j = i + 1;
        let mut k = j;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        frac_digits = k - j;
        if int_digits + frac_digits > 0 {
            i = k;
        }
    }
    if int_digits + frac_digits == 0 {
        return (0, 0.0);
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut k = i + 1;
        if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
            k += 1;
        }
        let e = k;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        if k > e {
            i = k;
        }
    }
    let text = std::str::from_utf8(&s[start..i]).unwrap_or("0");
    let text = text.strip_suffix('.').unwrap_or(text);
    (i, text.parse().unwrap_or(0.0))
}

/// The type the engine gives a value.
fn classify(v: Vec<u8>) -> KeyValue {
    if v.is_empty() {
        return KeyValue::String(v);
    }
    let (iend, ival) = strtol_len(&v);
    let (fend, fval) = if v.len() > 1 && v[1].eq_ignore_ascii_case(&b'x') {
        (0, 0.0)
    } else {
        strtod_len(&v)
    };
    if v.len() == 18 && v.starts_with(b"0x") {
        return KeyValue::String(v);
    }
    if fend > iend && fend == v.len() {
        return KeyValue::Float(fval as f32);
    }
    if iend == v.len()
        && let Some(i) = ival.and_then(|i| i32::try_from(i).ok())
    {
        return KeyValue::Int(i);
    }
    KeyValue::String(v)
}

fn block(lx: &mut Lexer, lower: bool, depth: usize) -> Option<Entries> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut out = Vec::new();
    loop {
        let key = match lx.token() {
            Some(Token::Close) | None => return Some(out),
            Some(Token::Text(k, _)) => k,
            Some(Token::Open) => return None,
        };
        let key = if lower { key.to_ascii_lowercase() } else { key };
        match lx.token()? {
            Token::Open => out.push((key, KeyValue::Block(block(lx, lower, depth + 1)?))),
            Token::Text(v, _) => out.push((key, classify(v))),
            Token::Close => return None,
        }
    }
}

/// The first root of a KeyValues text: its name and its entries.
pub fn parse_keyvalues(text: &[u8], escapes: bool, keep_case: bool) -> Option<(Vec<u8>, Entries)> {
    let mut lx = Lexer {
        s: text,
        i: 0,
        escapes,
    };
    let Token::Text(name, _) = lx.token()? else {
        return None;
    };
    match lx.token()? {
        Token::Open => Some((name, block(&mut lx, !keep_case, 1)?)),
        _ => None,
    }
}

fn to_lua(lua: &Lua, entries: &Entries) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (k, v) in entries {
        let v = match v {
            KeyValue::String(s) => Value::String(lua.create_string(s)?),
            KeyValue::Int(i) => Value::Integer(i64::from(*i)),
            KeyValue::Float(f) => Value::Number(f64::from(*f)),
            KeyValue::Block(b) => Value::Table(to_lua(lua, b)?),
        };
        t.raw_set(lua.create_string(k)?, v)?;
    }
    Ok(t)
}

/// `util.KeyValuesToTable`: the entries of the first root; `None` for text that is not
/// KeyValues.
pub fn keyvalues_to_table(
    lua: &Lua,
    text: &[u8],
    escapes: bool,
    keep_case: bool,
) -> mlua::Result<Option<Table>> {
    match parse_keyvalues(text, escapes, keep_case) {
        Some((_, entries)) => to_lua(lua, &entries).map(Some),
        None => Ok(None),
    }
}

fn quote(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    out.extend_from_slice(s);
    out.push(b'"');
}

fn write_block(
    out: &mut Vec<u8>,
    t: &Table,
    depth: usize,
    seen: &mut Vec<usize>,
) -> mlua::Result<()> {
    if seen.contains(&(t.to_pointer() as usize)) {
        return Ok(());
    }
    seen.push(t.to_pointer() as usize);
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair?;
        let Some(key) = crate::runtime::as_bytes(&k) else {
            continue;
        };
        let tabs = vec![b'\t'; depth];
        match &v {
            Value::Table(sub) => {
                out.extend_from_slice(&tabs);
                quote(out, &key);
                out.push(b'\n');
                out.extend_from_slice(&tabs);
                out.extend_from_slice(b"{\n");
                write_block(out, sub, depth + 1, seen)?;
                out.extend_from_slice(&tabs);
                out.extend_from_slice(b"}\n");
            }
            Value::String(_) | Value::Integer(_) | Value::Number(_) => {
                out.extend_from_slice(&tabs);
                quote(out, &key);
                out.extend_from_slice(b"\t\t");
                quote(out, &crate::runtime::as_bytes(&v).unwrap_or_default());
                out.push(b'\n');
            }
            _ => {}
        }
    }
    seen.pop();
    Ok(())
}

/// `util.TableToKeyValues`.
pub fn table_to_keyvalues(t: &Table, root: &[u8]) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::new();
    quote(&mut out, root);
    out.extend_from_slice(b"\n{\n");
    write_block(&mut out, t, 1, &mut Vec::new())?;
    out.extend_from_slice(b"}\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gamemode_file() {
        let text = br#""hl2rp"
{
	// A comment
	"base"		"flux"
	"title"		"HL2 RP"
	"maps"		"^rp_"
	"version"	"1.0.0"
	"MaxPlayers"	"128"
	"ratio"		"0.1"
	"settings"
	{
		1 { "name" "x" "type" "Numeric" }
	}
	"cond"		"a" [$WIN32]
}
"#;
        let (name, entries) = parse_keyvalues(text, false, false).unwrap();
        assert_eq!(name, b"hl2rp");
        let get = |k: &str| {
            entries
                .iter()
                .rev()
                .find(|(x, _)| x == k.as_bytes())
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("title"), Some(KeyValue::String(b"HL2 RP".to_vec())));
        assert_eq!(get("version"), Some(KeyValue::String(b"1.0.0".to_vec())));
        assert_eq!(get("maxplayers"), Some(KeyValue::Int(128)));
        assert_eq!(get("ratio"), Some(KeyValue::Float(0.1)));
        assert_eq!(get("cond"), Some(KeyValue::String(b"a".to_vec())));
        let Some(KeyValue::Block(settings)) = get("settings") else {
            panic!()
        };
        let KeyValue::Block(one) = &settings[0].1 else {
            panic!()
        };
        assert_eq!(
            one[1],
            (b"type".to_vec(), KeyValue::String(b"Numeric".to_vec()))
        );
        let (_, kept) = parse_keyvalues(text, false, true).unwrap();
        assert!(kept.iter().any(|(k, _)| k == b"MaxPlayers"));
    }

    #[test]
    fn values_as_the_engine_types_them() {
        assert_eq!(classify(b"12".to_vec()), KeyValue::Int(12));
        assert_eq!(classify(b"-3".to_vec()), KeyValue::Int(-3));
        assert_eq!(classify(b"1.5".to_vec()), KeyValue::Float(1.5));
        assert_eq!(classify(b"1.".to_vec()), KeyValue::Float(1.0));
        assert_eq!(classify(b"1e3".to_vec()), KeyValue::Float(1000.0));
        assert_eq!(
            classify(b"0x10".to_vec()),
            KeyValue::String(b"0x10".to_vec())
        );
        assert_eq!(
            classify(b"12abc".to_vec()),
            KeyValue::String(b"12abc".to_vec())
        );
        assert_eq!(classify(b"".to_vec()), KeyValue::String(Vec::new()));
        assert_eq!(
            classify(b"3000000000".to_vec()),
            KeyValue::String(b"3000000000".to_vec())
        );
        assert_eq!(
            classify(b"4294967296".to_vec()),
            KeyValue::String(b"4294967296".to_vec())
        );
        assert_eq!(classify(b"007".to_vec()), KeyValue::Int(7));
        assert_eq!(classify(b"-2147483648".to_vec()), KeyValue::Int(i32::MIN));
        assert!(matches!(classify(b"inf".to_vec()), KeyValue::Float(f) if f.is_infinite()));
    }

    #[test]
    fn escapes_only_when_asked() {
        let text = br#""r" { "a" "x\ty\"z" }"#;
        let (_, plain) = parse_keyvalues(text, false, false).unwrap();
        assert_eq!(plain[0].1, KeyValue::String(b"x\\ty\\".to_vec()));
        let (_, esc) = parse_keyvalues(text, true, false).unwrap();
        assert_eq!(esc[0].1, KeyValue::String(b"x\ty\"z".to_vec()));
    }

    #[test]
    fn writing() {
        let lua = Lua::new();
        let t: Table = lua
            .load("return { a = 'b', sub = { n = 2 } }")
            .eval()
            .unwrap();
        let s = String::from_utf8(table_to_keyvalues(&t, b"Root").unwrap()).unwrap();
        assert!(s.starts_with("\"Root\"\n{\n"), "{s}");
        assert!(s.contains("\t\"a\"\t\t\"b\"\n"), "{s}");
        assert!(
            s.contains("\t\"sub\"\n\t{\n\t\t\"n\"\t\t\"2\"\n\t}\n"),
            "{s}"
        );
        let back = keyvalues_to_table(&lua, s.as_bytes(), false, false)
            .unwrap()
            .unwrap();
        let sub: Table = back.get("sub").unwrap();
        assert_eq!(sub.get::<i64>("n").unwrap(), 2);
    }

    #[test]
    fn what_the_game_wrote_and_read() {
        let lua = Lua::new();
        let t: Table = lua
            .load("return { a = 1.5, b = 100000, c = 1e20, d = true, e = { f = 'g' } }")
            .eval()
            .unwrap();
        let s = String::from_utf8(table_to_keyvalues(&t, b"TableToKeyValues").unwrap()).unwrap();
        let body = s
            .strip_prefix("\"TableToKeyValues\"\n{\n")
            .and_then(|b| b.strip_suffix("}\n"))
            .unwrap();
        let mut entries: Vec<&str> = body.split_inclusive('\n').collect();
        let block = entries
            .iter()
            .position(|l| *l == "\t\"e\"\n")
            .expect("the block");
        let e: String = entries.drain(block..block + 4).collect();
        assert_eq!(e, "\t\"e\"\n\t{\n\t\t\"f\"\t\t\"g\"\n\t}\n");
        entries.sort();
        assert_eq!(
            entries,
            [
                "\t\"a\"\t\t\"1.5\"\n",
                "\t\"b\"\t\t\"100000\"\n",
                "\t\"c\"\t\t\"1e+20\"\n"
            ]
        );
        let (_, read) = parse_keyvalues(
            br#""r" { "a" "1.5" "b" "4294967296" "c" "007" "d" "abc" }"#,
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            read,
            [
                (b"a".to_vec(), KeyValue::Float(1.5)),
                (b"b".to_vec(), KeyValue::String(b"4294967296".to_vec())),
                (b"c".to_vec(), KeyValue::Int(7)),
                (b"d".to_vec(), KeyValue::String(b"abc".to_vec())),
            ]
        );
    }

    #[test]
    fn deep_nesting_is_refused() {
        let deep = format!("\"a\"{}", "{\"b\"".repeat(200_000));
        assert!(parse_keyvalues(deep.as_bytes(), false, false).is_none());
        let ok = format!("\"a\"{}{{}}", "{\"b\"".repeat(MAX_DEPTH - 1));
        assert!(parse_keyvalues(ok.as_bytes(), false, false).is_some());
    }
}
