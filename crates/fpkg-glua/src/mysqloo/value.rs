//! What the fields of a result become in Lua, and escaping.
//!
//! MySQLOO keeps every field as text: the text of the text protocol (`db:query`), and for a
//! prepared statement (`db:prepare`) the text libmariadb writes for a value of the binary
//! protocol when asked for a string. Integers are written in decimal (unsigned when the column
//! is), `FLOAT` with at most 6 significant digits and `DOUBLE` with as many as it takes to read
//! back the same, both with the fixed decimals of the column when it has them; dates and times
//! as `2024-01-31`, `2024-01-31 12:00:00`, `-838:59:59`, with as many digits of the fraction of
//! a second as the column has; everything else as the bytes the server sent.
//!
//! The text becomes a Lua value by the type of the column (`Query::getData`): `TINYINT`,
//! `SMALLINT`, `MEDIUMINT`, `INT`, `BIGINT`, `FLOAT` and `DOUBLE` a number, as `atof` reads the
//! text (so a `BIGINT` beyond 2^53 loses its last digits), `BIT` the first byte as a signed
//! number, the `NULL` type nil, and everything else (`DECIMAL`, `YEAR`, the dates and times,
//! the strings, `JSON`, `ENUM`, `SET`, the spatial types) the string as it is.

/// The column types of the protocol.
pub(crate) mod kind {
    pub const TINY: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const LONG: u8 = 3;
    pub const FLOAT: u8 = 4;
    pub const DOUBLE: u8 = 5;
    pub const NULL: u8 = 6;
    pub const TIMESTAMP: u8 = 7;
    pub const LONGLONG: u8 = 8;
    pub const INT24: u8 = 9;
    pub const DATE: u8 = 10;
    pub const TIME: u8 = 11;
    pub const DATETIME: u8 = 12;
    pub const YEAR: u8 = 13;
    pub const NEWDATE: u8 = 14;
    pub const BIT: u8 = 16;
    pub const TIMESTAMP2: u8 = 17;
    pub const DATETIME2: u8 = 18;
    pub const TIME2: u8 = 19;
    pub const STRING: u8 = 254;
}

/// `UNSIGNED_FLAG` and `ZEROFILL_FLAG` of a column.
pub(crate) const UNSIGNED: u16 = 32;
pub(crate) const ZEROFILL: u16 = 64;

/// The decimals of a `FLOAT` or `DOUBLE` column without fixed decimals.
const NOT_FIXED_DEC: u8 = 31;

/// What a column says about the text of its values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Meta {
    pub kind: u8,
    pub flags: u16,
    pub decimals: u8,
    pub length: u32,
}

/// A value of the binary protocol, as it was read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Binary {
    Signed(i64),
    Unsigned(u64),
    Float(f32),
    Double(f64),
    /// Year, month, day, hour, minute, second, microsecond.
    DateTime(u16, u8, u8, u8, u8, u8, u32),
    /// Negative, days, hours, minutes, seconds, microseconds.
    Time(bool, u32, u8, u8, u8, u32),
    Bytes(Vec<u8>),
}

/// The text libmariadb writes for a value of the binary protocol.
pub(crate) fn binary_text(meta: &Meta, value: Binary) -> Vec<u8> {
    match value {
        Binary::Signed(n) => zero_filled(meta, n.to_string().into_bytes()),
        Binary::Unsigned(n) => zero_filled(meta, n.to_string().into_bytes()),
        Binary::Float(f) => {
            if meta.decimals < NOT_FIXED_DEC {
                fixed(f as f64, meta.decimals)
            } else {
                significant(f as f64, 6)
            }
        }
        Binary::Double(d) => {
            if meta.decimals < NOT_FIXED_DEC {
                fixed(d, meta.decimals)
            } else {
                shortest(d)
            }
        }
        Binary::DateTime(y, mo, d, h, mi, s, us) => {
            let mut out = match meta.kind {
                kind::DATE | kind::NEWDATE => format!("{y:04}-{mo:02}-{d:02}"),
                kind::TIME | kind::TIME2 => format!("{h:02}:{mi:02}:{s:02}"),
                _ => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"),
            };
            if !matches!(meta.kind, kind::DATE | kind::NEWDATE) {
                fraction(&mut out, meta.decimals, us);
            }
            out.into_bytes()
        }
        Binary::Time(negative, days, h, mi, s, us) => {
            let hours = days as u64 * 24 + h as u64;
            let mut out = format!(
                "{}{hours:02}:{mi:02}:{s:02}",
                if negative { "-" } else { "" }
            );
            fraction(&mut out, meta.decimals, us);
            out.into_bytes()
        }
        Binary::Bytes(b) => b,
    }
}

fn fraction(out: &mut String, decimals: u8, micros: u32) {
    if decimals > 0 && decimals <= 6 {
        let digits = format!("{micros:06}");
        out.push('.');
        out.push_str(&digits[..decimals as usize]);
    }
}

/// An integer padded with zeros to the width of a `ZEROFILL` column.
fn zero_filled(meta: &Meta, digits: Vec<u8>) -> Vec<u8> {
    let width = meta.length as usize;
    if meta.flags & ZEROFILL == 0 || digits.len() >= width || width > 64 {
        return digits;
    }
    let mut out = vec![b'0'; width - digits.len()];
    out.extend(digits);
    out
}

/// A number with `decimals` digits after the point, rounded to the nearest.
fn fixed(n: f64, decimals: u8) -> Vec<u8> {
    if !n.is_finite() {
        return b"0".to_vec();
    }
    format!("{n:.*}", decimals as usize).into_bytes()
}

/// A number rounded to `digits` significant digits.
fn significant(n: f64, digits: usize) -> Vec<u8> {
    if !n.is_finite() {
        return b"0".to_vec();
    }
    let rounded: f64 = format!("{n:.*e}", digits - 1).parse().unwrap_or(n);
    shortest(rounded)
}

/// The shortest text that reads back as the same number; libmariadb writes `0` for an
/// infinity and for NaN.
fn shortest(n: f64) -> Vec<u8> {
    if !n.is_finite() {
        return b"0".to_vec();
    }
    format!("{n:?}").into_bytes()
}

/// A field as Lua gets it.
#[derive(Debug, PartialEq)]
pub(crate) enum Field<'a> {
    Nil,
    Number(f64),
    Text(&'a [u8]),
}

/// What a field that is not NULL becomes in Lua, by the type of its column. The whole-number
/// types (`TINYINT` to `BIGINT`, signed or not, `BOOLEAN` and `ZEROFILL` columns included),
/// `FLOAT` and `DOUBLE` are a number read from the text by [`atof`]; `BIT` is a number, its
/// first byte taken as signed (0 when there is none); a column of the NULL type gives nothing;
/// any other type is the text itself, every byte of it.
pub(crate) fn field(kind: u8, text: &[u8]) -> Field<'_> {
    const NUMBERS: [u8; 7] = [
        kind::TINY,
        kind::SHORT,
        kind::INT24,
        kind::LONG,
        kind::LONGLONG,
        kind::FLOAT,
        kind::DOUBLE,
    ];
    if NUMBERS.contains(&kind) {
        Field::Number(atof(text))
    } else if kind == kind::BIT {
        let first = text.first().copied().unwrap_or(0);
        Field::Number(f64::from(i8::from_ne_bytes([first])))
    } else if kind == kind::NULL {
        Field::Nil
    } else {
        Field::Text(text)
    }
}

/// C's `atof`: the longest prefix that is a decimal number, after white space; 0 when there
/// is none.
pub(crate) fn atof(text: &[u8]) -> f64 {
    let mut i = 0;
    while i < text.len() && matches!(text[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let start = i;
    if i < text.len() && matches!(text[i], b'+' | b'-') {
        i += 1;
    }
    let rest = &text[i..];
    let word = |w: &[u8]| rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w);
    let negative = start < i && text[start] == b'-';
    if word(b"infinity") || word(b"inf") {
        return if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if word(b"nan") {
        return if negative { -f64::NAN } else { f64::NAN };
    }
    let digits_from = i;
    while i < text.len() && text[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - digits_from;
    if i < text.len() && text[i] == b'.' {
        let point = i;
        i += 1;
        let frac_from = i;
        while i < text.len() && text[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - frac_from;
        if digits == 0 {
            i = point;
        }
    }
    if digits == 0 {
        return 0.0;
    }
    if i < text.len() && matches!(text[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < text.len() && matches!(text[j], b'+' | b'-') {
            j += 1;
        }
        let exp_from = j;
        while j < text.len() && text[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_from {
            i = j;
        }
    }
    std::str::from_utf8(&text[start..i])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

/// How a character set of the client takes escaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Charset {
    /// One byte a character.
    Single,
    /// UTF-8 of up to three bytes a character (`utf8`, `utf8mb3`).
    Utf8mb3,
    /// UTF-8 of up to four bytes a character.
    Utf8mb4,
    /// Another character set of several bytes a character (`big5`, `gbk`, `sjis`, ...).
    Multibyte,
}

/// The character sets libmariadb knows, by the name `SET NAMES` gets, and how they escape.
const CHARSETS: &[(&str, Charset)] = &[
    ("armscii8", Charset::Single),
    ("ascii", Charset::Single),
    ("big5", Charset::Multibyte),
    ("binary", Charset::Single),
    ("cp1250", Charset::Single),
    ("cp1251", Charset::Single),
    ("cp1256", Charset::Single),
    ("cp1257", Charset::Single),
    ("cp850", Charset::Single),
    ("cp852", Charset::Single),
    ("cp866", Charset::Single),
    ("cp932", Charset::Multibyte),
    ("dec8", Charset::Single),
    ("eucjpms", Charset::Multibyte),
    ("euckr", Charset::Multibyte),
    ("gb2312", Charset::Multibyte),
    ("gbk", Charset::Multibyte),
    ("geostd8", Charset::Single),
    ("greek", Charset::Single),
    ("hebrew", Charset::Single),
    ("hp8", Charset::Single),
    ("keybcs2", Charset::Single),
    ("koi8r", Charset::Single),
    ("koi8u", Charset::Single),
    ("latin1", Charset::Single),
    ("latin2", Charset::Single),
    ("latin5", Charset::Single),
    ("latin7", Charset::Single),
    ("macce", Charset::Single),
    ("macroman", Charset::Single),
    ("sjis", Charset::Multibyte),
    ("swe7", Charset::Single),
    ("tis620", Charset::Single),
    ("ucs2", Charset::Multibyte),
    ("ujis", Charset::Multibyte),
    ("utf16", Charset::Multibyte),
    ("utf16le", Charset::Multibyte),
    ("utf32", Charset::Multibyte),
    ("utf8", Charset::Utf8mb3),
    ("utf8mb3", Charset::Utf8mb3),
    ("utf8mb4", Charset::Utf8mb4),
];

/// A character set by its name, whatever its case: the name `SET NAMES` gets and how it
/// escapes; `None` for one libmariadb does not know.
pub(crate) fn charset(name: &[u8]) -> Option<(&'static str, Charset)> {
    CHARSETS
        .iter()
        .find(|(n, _)| n.as_bytes().eq_ignore_ascii_case(name))
        .copied()
}

/// The length of the UTF-8 character at the start of `s`, or 0 when it is not a whole valid
/// one (MySQL's check: no overlong forms, nothing beyond U+10FFFF).
fn utf8_length(s: &[u8], max: usize) -> usize {
    let cont = |i: usize| s.get(i).is_some_and(|b| b & 0xc0 == 0x80);
    let c = s[0];
    if c < 0x80 {
        1
    } else if c < 0xc2 {
        0
    } else if c < 0xe0 {
        if cont(1) { 2 } else { 0 }
    } else if c < 0xf0 {
        if cont(1) && cont(2) && (c >= 0xe1 || s[1] >= 0xa0) {
            3
        } else {
            0
        }
    } else if c < 0xf5 && max == 4 {
        if cont(1)
            && cont(2)
            && cont(3)
            && (c >= 0xf1 || s[1] >= 0x90)
            && (c <= 0xf3 || s[1] <= 0x8f)
        {
            4
        } else {
            0
        }
    } else {
        0
    }
}

/// `mysql_real_escape_string`: with backslashes (`\0`, `\n`, `\r`, `\\`, `\'`, `\"`, `\Z`),
/// or, when the server is in `NO_BACKSLASH_ESCAPES` mode, with `'` doubled. A whole
/// multibyte character is kept as it is, and so are the bytes of a broken one.
pub(crate) fn escape(text: &[u8], charset: Charset, no_backslash: bool) -> Result<Vec<u8>, String> {
    let max = match charset {
        Charset::Utf8mb3 => 3,
        Charset::Utf8mb4 => 4,
        Charset::Single => 1,
        Charset::Multibyte => {
            if !text.is_ascii() {
                return Err(
                    "fpkg's mysqloo module does not implement escaping text that is not ASCII in a multibyte character set other than UTF-8"
                        .into(),
                );
            }
            1
        }
    };
    let mut out = Vec::with_capacity(text.len() * 2);
    let mut i = 0;
    while i < text.len() {
        if max > 1 {
            let len = utf8_length(&text[i..], max);
            if len > 1 {
                out.extend_from_slice(&text[i..i + len]);
                i += len;
                continue;
            }
        }
        let c = text[i];
        if no_backslash {
            if c == b'\'' {
                out.extend_from_slice(b"''");
            } else {
                out.push(c);
            }
        } else {
            let escaped = match c {
                0 => Some(b'0'),
                b'\n' => Some(b'n'),
                b'\r' => Some(b'r'),
                b'\\' => Some(b'\\'),
                b'\'' => Some(b'\''),
                b'"' => Some(b'"'),
                0x1a => Some(b'Z'),
                _ => None,
            };
            match escaped {
                Some(e) => {
                    out.push(b'\\');
                    out.push(e);
                }
                None => out.push(c),
            }
        }
        i += 1;
    }
    Ok(out)
}
