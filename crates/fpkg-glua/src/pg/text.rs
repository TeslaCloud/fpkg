//! The text that goes to the server and comes back, as gmsv_pg and libpq make it: parameters,
//! the Lua values of result fields, escaping and quoting, bytea in its hex form, and the error
//! and notice messages in libpq's layout (`pqBuildErrorMessage3` with the default verbosity,
//! its `LINE n:` cursor display included).

/// How the client encoding cuts text into characters, for escaping and the cursor display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    /// One byte is one character: `SQL_ASCII`, `LATIN1` and the other single-byte encodings.
    Single,
    /// A multibyte encoding other than UTF-8 (`EUC_JP`, `SJIS`, ...), whose characters are not
    /// told apart here.
    Other,
}

impl Encoding {
    /// The encoding of the server's name for it (`client_encoding`).
    pub(crate) fn of(name: &[u8]) -> Encoding {
        const MULTIBYTE: &[&str] = &[
            "EUC_JP",
            "EUC_CN",
            "EUC_KR",
            "EUC_TW",
            "EUC_JIS_2004",
            "MULE_INTERNAL",
            "SJIS",
            "SHIFT_JIS_2004",
            "BIG5",
            "GBK",
            "UHC",
            "GB18030",
            "JOHAB",
        ];
        let upper = String::from_utf8_lossy(name).to_ascii_uppercase();
        if upper == "UTF8" || upper == "UNICODE" {
            Encoding::Utf8
        } else if MULTIBYTE.contains(&upper.as_str()) {
            Encoding::Other
        } else {
            Encoding::Single
        }
    }
}

/// A number as a parameter (`number_param`): a whole number within the 64-bit range as an
/// integer, anything else in the shortest form that reads back as the same number, and the
/// server's names for NaN and the infinities.
pub(crate) fn number_param(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if (-LIMIT..LIMIT).contains(&value) && value == (value as i64) as f64 {
        return (value as i64).to_string();
    }
    shortest(value)
}

/// `std::to_chars` of a double without a format: the shortest digits that read back as the
/// number, written as `%f` or `%e` would write them, whichever is shorter (`%f` on a tie). A
/// whole number written as `%f` has all of its digits, as libstdc++ writes it.
pub(crate) fn shortest(value: f64) -> String {
    let sci = format!("{:e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if value.is_sign_negative() { "-" } else { "" };

    let mut e_form = String::from(sign);
    e_form.push_str(&digits[..1]);
    if digits.len() > 1 {
        e_form.push('.');
        e_form.push_str(&digits[1..]);
    }
    e_form.push('e');
    e_form.push(if exp < 0 { '-' } else { '+' });
    e_form.push_str(&format!("{:02}", exp.unsigned_abs()));

    let mut f_form = String::from(sign);
    if exp >= 0 {
        let whole = exp as usize + 1;
        if digits.len() <= whole {
            f_form = format!("{value:.0}");
        } else {
            f_form.push_str(&digits[..whole]);
            f_form.push('.');
            f_form.push_str(&digits[whole..]);
        }
    } else {
        f_form.push_str("0.");
        f_form.push_str(&"0".repeat(exp.unsigned_abs() as usize - 1));
        f_form.push_str(&digits);
    }

    if f_form.len() <= e_form.len() {
        f_form
    } else {
        e_form
    }
}

/// The largest whole number a Lua number holds exactly, either way.
const MAX_EXACT: i128 = 1 << 53;

/// The number a field of the numeric category stands for (`number`): `None` keeps it text,
/// as for a whole number beyond 2^53, one beyond the 64-bit range, or text that does not read
/// as a number as a whole (`money`, `regclass`).
pub(crate) fn number(text: &[u8]) -> Option<f64> {
    let digits = text.strip_prefix(b"-").unwrap_or(text);
    if text.is_empty() || (!digits.is_empty() && digits.iter().all(u8::is_ascii_digit)) {
        let whole = std::str::from_utf8(text).ok()?.parse::<i128>().ok()?;
        if !(-MAX_EXACT..=MAX_EXACT).contains(&whole) || whole.unsigned_abs() > i64::MAX as u128 {
            return None;
        }
    }
    float(text)
}

/// `std::from_chars` of a double in the general format: an optional minus, digits with an
/// optional point and exponent, or `inf`, `infinity` and `nan` in any case. A number that is
/// out of the range of a double is none.
fn float(text: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(text).ok()?;
    let body = s.strip_prefix('-').unwrap_or(s);
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(if s.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    let mut chars = body.bytes().peekable();
    let mut mantissa = 0;
    let mut nonzero = false;
    while let Some(c) = chars.peek().copied().filter(u8::is_ascii_digit) {
        nonzero |= c != b'0';
        mantissa += 1;
        chars.next();
    }
    if chars.peek() == Some(&b'.') {
        chars.next();
        while let Some(c) = chars.peek().copied().filter(u8::is_ascii_digit) {
            nonzero |= c != b'0';
            mantissa += 1;
            chars.next();
        }
    }
    if mantissa == 0 {
        return None;
    }
    if matches!(chars.peek(), Some(b'e' | b'E')) {
        chars.next();
        if matches!(chars.peek(), Some(b'+' | b'-')) {
            chars.next();
        }
        let mut exponent = 0;
        while chars.peek().is_some_and(u8::is_ascii_digit) {
            exponent += 1;
            chars.next();
        }
        if exponent == 0 {
            return None;
        }
    }
    if chars.next().is_some() {
        return None;
    }
    let value: f64 = s.parse().ok()?;
    if value.is_infinite() || (value == 0.0 && nonzero) {
        return None;
    }
    Some(value)
}

/// What a field becomes in Lua, by the category of its type (`convert`).
#[derive(Debug, PartialEq)]
pub(crate) enum Field<'a> {
    Bool(bool),
    Number(f64),
    Text(&'a [u8]),
}

/// The Lua value of a field of a type of the category (`typcategory`, 0 when not known).
pub(crate) fn convert(category: u8, text: &[u8]) -> Field<'_> {
    match category {
        b'B' => Field::Bool(text == b"t"),
        b'N' => number(text).map_or(Field::Text(text), Field::Number),
        _ => Field::Text(text),
    }
}

/// The number of rows a command tag says the command affected (`PQcmdTuples`), 0 for a tag
/// that says none.
pub(crate) fn affected(tag: &[u8]) -> f64 {
    let rest = if let Some(rest) = tag.strip_prefix(b"INSERT ") {
        match rest.iter().position(|c| *c == b' ') {
            Some(i) => &rest[i + 1..],
            None => return 0.0,
        }
    } else if let Some(rest) = [&b"SELECT "[..], b"DELETE ", b"UPDATE "]
        .iter()
        .find_map(|p| tag.strip_prefix(*p))
    {
        rest
    } else if let Some(rest) = [&b"FETCH "[..], b"MERGE "]
        .iter()
        .find_map(|p| tag.strip_prefix(*p))
    {
        rest
    } else if let Some(rest) = [&b"MOVE "[..], b"COPY "]
        .iter()
        .find_map(|p| tag.strip_prefix(*p))
    {
        rest
    } else {
        return 0.0;
    };
    if rest.is_empty() || !rest.iter().all(u8::is_ascii_digit) {
        return 0.0;
    }
    std::str::from_utf8(rest)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

/// The bytes of a character that starts with `lead` in UTF-8 (`pg_utf_mblen`): 1 for a byte
/// that starts none.
fn utf8_len(lead: u8) -> usize {
    if lead & 0x80 == 0 {
        1
    } else if lead & 0xe0 == 0xc0 {
        2
    } else if lead & 0xf0 == 0xe0 {
        3
    } else if lead & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

/// Whether the character at the start of `s`, of `len` bytes, is well formed UTF-8
/// (`pg_utf8_islegal`): no overlong forms, no surrogates, nothing past U+10FFFF.
fn utf8_legal(s: &[u8], len: usize) -> bool {
    if len > 4 || s.len() < len {
        return false;
    }
    let tail = |i: usize| (0x80..=0xbf).contains(&s[i]);
    if len >= 4 && !tail(3) {
        return false;
    }
    if len >= 3 && !tail(2) {
        return false;
    }
    if len >= 2 {
        let a = s[1];
        let ok = match s[0] {
            0xe0 => (0xa0..=0xbf).contains(&a),
            0xed => (0x80..=0x9f).contains(&a),
            0xf0 => (0x90..=0xbf).contains(&a),
            0xf4 => (0x80..=0x8f).contains(&a),
            _ => (0x80..=0xbf).contains(&a),
        };
        if !ok {
            return false;
        }
    }
    let a = s[0];
    !((0x80..0xc2).contains(&a) || a > 0xf4)
}

/// Whether text is well formed for the encoding as the module checks it before it escapes
/// without libpq: UTF-8 that libpq takes, or plain ASCII in any other encoding.
pub(crate) fn well_formed(text: &[u8], encoding: Encoding) -> bool {
    if encoding != Encoding::Utf8 {
        return text.is_ascii();
    }
    let mut at = 0;
    while at < text.len() {
        let len = utf8_len(text[at]);
        if text[at] >= 0x80 && !utf8_legal(&text[at..], len) {
            return false;
        }
        at += len;
    }
    true
}

/// Why libpq turns text down: the message it writes into the connection.
pub(crate) const INCOMPLETE: &str = "incomplete multibyte character";
pub(crate) const INVALID: &str = "invalid multibyte character";

/// The error for text in a multibyte encoding other than UTF-8, whose characters this module
/// does not know.
fn unknown_encoding() -> String {
    "fpkg's pg module does not implement escaping text that is not ASCII in a multibyte encoding other than UTF8".into()
}

/// `PQescapeStringConn`: quotes doubled (and backslashes, without standard conforming
/// strings), text that is not well formed in the encoding turned down.
pub(crate) fn escape_string(
    text: &[u8],
    encoding: Encoding,
    std_strings: bool,
) -> Result<Vec<u8>, String> {
    let text = crate::runtime::until_nul(text);
    let mut out = Vec::with_capacity(text.len() + 2);
    let mut at = 0;
    while at < text.len() {
        let c = text[at];
        if c < 0x80 {
            if c == b'\'' || (c == b'\\' && !std_strings) {
                out.push(c);
            }
            out.push(c);
            at += 1;
            continue;
        }
        let len = match encoding {
            Encoding::Utf8 => utf8_len(c),
            Encoding::Single => 1,
            Encoding::Other => return Err(unknown_encoding()),
        };
        if text.len() - at < len {
            return Err(INCOMPLETE.into());
        }
        if encoding == Encoding::Utf8 && !utf8_legal(&text[at..], len) {
            return Err(INVALID.into());
        }
        out.extend_from_slice(&text[at..at + len]);
        at += len;
    }
    Ok(out)
}

/// The module's own quoting of a name that is well formed: double quotes around it, those in
/// it doubled.
pub(crate) fn quote_plain_name(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 2);
    out.push(b'"');
    for &c in crate::runtime::until_nul(text) {
        if c == b'"' {
            out.push(c);
        }
        out.push(c);
    }
    out.push(b'"');
    out
}

/// `PQescapeIdentifier`: the name in double quotes, with text that is not well formed in the
/// encoding turned down the way libpq turns it down.
pub(crate) fn escape_identifier(text: &[u8], encoding: Encoding) -> Result<Vec<u8>, String> {
    let text = crate::runtime::until_nul(text);
    let mut at = 0;
    let mut validated = false;
    while at < text.len() {
        if text[at] < 0x80 {
            at += 1;
            continue;
        }
        let len = match encoding {
            Encoding::Utf8 => utf8_len(text[at]),
            Encoding::Single => 1,
            Encoding::Other => return Err(unknown_encoding()),
        };
        if len > text.len() - at {
            return Err(INCOMPLETE.into());
        }
        if !validated {
            if encoding == Encoding::Utf8 && !well_formed(&text[at..], encoding) {
                return Err(INVALID.into());
            }
            validated = true;
        }
        at += len;
    }
    Ok(quote_plain_name(text))
}

/// Binary data as the hex text a bytea takes (`\x00ff`).
pub(crate) fn escape_bytea(raw: &[u8]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(2 + raw.len() * 2);
    out.extend_from_slice(b"\\x");
    for b in raw {
        out.push(DIGITS[(b >> 4) as usize]);
        out.push(DIGITS[(b & 0x0f) as usize]);
    }
    out
}

/// The binary data of a bytea in its hex form, or the module's error.
pub(crate) fn unescape_bytea(text: &[u8]) -> Result<Vec<u8>, &'static str> {
    if text.len() < 2 || text[0] != b'\\' || text[1] != b'x' {
        return Err("pg - binary data has to start with \\x");
    }
    if !text.len().is_multiple_of(2) {
        return Err("pg - binary data is cut off");
    }
    let hex = |d: u8| (d as char).to_digit(16);
    let mut out = Vec::with_capacity(text.len() / 2 - 1);
    for pair in text[2..].chunks(2) {
        match (hex(pair[0]), hex(pair[1])) {
            (Some(h), Some(l)) => out.push((h << 4 | l) as u8),
            _ => return Err("pg - binary data has something other than hex digits in it"),
        }
    }
    Ok(out)
}

/// Text without the white space at its end, as the module trims libpq's messages.
pub(crate) fn trimmed(text: &[u8]) -> &[u8] {
    let mut end = text.len();
    while end > 0 && matches!(text[end - 1], b'\n' | b'\r' | b' ' | b'\t') {
        end -= 1;
    }
    &text[..end]
}

/// The fields of an error or a notice from the server, by their one-byte codes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Fields(pub(crate) Vec<(u8, Vec<u8>)>);

impl Fields {
    /// The value of a field; of one that came twice, the last, as libpq keeps them.
    pub(crate) fn get(&self, code: u8) -> Option<&[u8]> {
        self.0
            .iter()
            .rev()
            .find(|(c, _)| *c == code)
            .map(|(_, v)| v.as_slice())
    }
}

/// The names that the module gives the fields in the details of an error.
pub(crate) const DETAILS: &[(u8, &str)] = &[
    (b'C', "sqlstate"),
    (b'V', "severity"),
    (b'M', "message"),
    (b'D', "detail"),
    (b'H', "hint"),
    (b'W', "context"),
    (b'P', "position"),
    (b's', "schema"),
    (b't', "table"),
    (b'c', "column"),
    (b'd', "datatype"),
    (b'n', "constraint"),
];

/// `atoi`: the whole number at the start of the text, 0 when there is none.
fn atoi(text: &[u8]) -> i64 {
    let s = std::str::from_utf8(text).unwrap_or("");
    let s = s.trim_start();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let n: i64 = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0i64, |n, d| {
            n.saturating_mul(10).saturating_add((d - b'0') as i64)
        });
    if neg { -n } else { n }
}

/// The message of an error or a notice, as libpq builds it: `ERROR:  message`, the cursor
/// display or ` at character n`, then `DETAIL:`, `HINT:`, `QUERY:` and, for an error,
/// `CONTEXT:` lines, each ending with a line break. `query` is the text of the statement that
/// was sent, when libpq knows it (not for one that was prepared).
pub(crate) fn message(
    fields: &Fields,
    query: Option<&[u8]>,
    encoding: Encoding,
    error: bool,
) -> Vec<u8> {
    let mut msg = Vec::new();
    if let Some(severity) = fields.get(b'S') {
        msg.extend_from_slice(severity);
        msg.extend_from_slice(b":  ");
    }
    if let Some(primary) = fields.get(b'M') {
        msg.extend_from_slice(primary);
    }
    let mut cursor = None;
    if let Some(position) = fields.get(b'P') {
        match query {
            Some(q) => cursor = Some((q, atoi(position))),
            None => {
                msg.extend_from_slice(b" at character ");
                msg.extend_from_slice(position);
            }
        }
    } else if let Some(position) = fields.get(b'p') {
        match fields.get(b'q') {
            Some(q) => cursor = Some((q, atoi(position))),
            None => {
                msg.extend_from_slice(b" at character ");
                msg.extend_from_slice(position);
            }
        }
    }
    msg.push(b'\n');
    if let Some((q, position)) = cursor
        && position > 0
    {
        report_position(&mut msg, q, position, encoding);
    }
    for (code, label) in [(b'D', "DETAIL"), (b'H', "HINT"), (b'q', "QUERY")] {
        if let Some(v) = fields.get(code) {
            msg.extend_from_slice(label.as_bytes());
            msg.extend_from_slice(b":  ");
            msg.extend_from_slice(v);
            msg.push(b'\n');
        }
    }
    if error && let Some(context) = fields.get(b'W') {
        msg.extend_from_slice(b"CONTEXT:  ");
        msg.extend_from_slice(context);
        msg.push(b'\n');
    }
    msg
}

/// The screen width of the character at the start of `s` in UTF-8 (`pg_utf_dsplen`), where
/// what has none counts as one.
fn utf8_width(s: &[u8]) -> i64 {
    let at = |i: usize| s.get(i).copied().unwrap_or(0) as u32;
    let c = at(0);
    let code = if c & 0x80 == 0 {
        c
    } else if c & 0xe0 == 0xc0 {
        (c & 0x1f) << 6 | (at(1) & 0x3f)
    } else if c & 0xf0 == 0xe0 {
        (c & 0x0f) << 12 | (at(1) & 0x3f) << 6 | (at(2) & 0x3f)
    } else if c & 0xf8 == 0xf0 {
        (c & 0x07) << 18 | (at(1) & 0x3f) << 12 | (at(2) & 0x3f) << 6 | (at(3) & 0x3f)
    } else {
        return 1;
    };
    if wide(code) { 2 } else { 1 }
}

/// Whether a code point takes two columns: the East Asian Wide and Fullwidth ranges.
fn wide(c: u32) -> bool {
    const RANGES: &[(u32, u32)] = &[
        (0x1100, 0x115f),
        (0x231a, 0x231b),
        (0x2329, 0x232a),
        (0x23e9, 0x23ec),
        (0x23f0, 0x23f0),
        (0x23f3, 0x23f3),
        (0x25fd, 0x25fe),
        (0x2614, 0x2615),
        (0x2648, 0x2653),
        (0x267f, 0x267f),
        (0x2693, 0x2693),
        (0x26a1, 0x26a1),
        (0x26aa, 0x26ab),
        (0x26bd, 0x26be),
        (0x26c4, 0x26c5),
        (0x26ce, 0x26ce),
        (0x26d4, 0x26d4),
        (0x26ea, 0x26ea),
        (0x26f2, 0x26f3),
        (0x26f5, 0x26f5),
        (0x26fa, 0x26fa),
        (0x26fd, 0x26fd),
        (0x2705, 0x2705),
        (0x270a, 0x270b),
        (0x2728, 0x2728),
        (0x274c, 0x274c),
        (0x274e, 0x274e),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27b0, 0x27b0),
        (0x27bf, 0x27bf),
        (0x2b1b, 0x2b1c),
        (0x2b50, 0x2b50),
        (0x2b55, 0x2b55),
        (0x2e80, 0x303e),
        (0x3041, 0x33ff),
        (0x3400, 0x4dbf),
        (0x4e00, 0x9fff),
        (0xa000, 0xa4cf),
        (0xa960, 0xa97f),
        (0xac00, 0xd7a3),
        (0xf900, 0xfaff),
        (0xfe10, 0xfe19),
        (0xfe30, 0xfe6f),
        (0xff00, 0xff60),
        (0xffe0, 0xffe6),
        (0x16fe0, 0x16fe4),
        (0x17000, 0x18cff),
        (0x1b000, 0x1b2ff),
        (0x1f004, 0x1f004),
        (0x1f0cf, 0x1f0cf),
        (0x1f18e, 0x1f18e),
        (0x1f191, 0x1f19a),
        (0x1f200, 0x1f251),
        (0x1f300, 0x1f64f),
        (0x1f680, 0x1f6ff),
        (0x1f900, 0x1f9ff),
        (0x1fa70, 0x1faff),
        (0x20000, 0x2fffd),
        (0x30000, 0x3fffd),
    ];
    RANGES.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c))
}

/// libpq's `reportErrorPosition`: the line of the query that holds the error, cut to 60
/// columns around it, and a `^` under the character at `position` (counted from 1).
fn report_position(msg: &mut Vec<u8>, query: &[u8], position: i64, encoding: Encoding) {
    const DISPLAY_SIZE: i64 = 60;
    const MIN_RIGHT_CUT: i64 = 10;

    let loc = position - 1;
    if loc < 0 {
        return;
    }
    let mut w = crate::runtime::until_nul(query).to_vec();
    let slen = w.len() + 1;
    let mut qidx = vec![0usize; slen];
    let mut scridx = vec![0i64; slen];
    let multibyte = encoding == Encoding::Utf8;

    let mut qoffset = 0usize;
    let mut scroffset = 0i64;
    let mut loc_line = 1;
    let mut ibeg = 0usize;
    let mut iend = None;
    let mut cno = 0usize;
    while qoffset < w.len() {
        let ch = w[qoffset];
        qidx[cno] = qoffset;
        scridx[cno] = scroffset;
        if ch == b'\t' {
            w[qoffset] = b' ';
        } else if ch == b'\r' || ch == b'\n' {
            if (cno as i64) < loc {
                if ch == b'\r' || cno == 0 || w[qidx[cno - 1]] != b'\r' {
                    loc_line += 1;
                }
                ibeg = cno + 1;
            } else {
                iend = Some(cno);
                break;
            }
        }
        if multibyte {
            scroffset += utf8_width(&w[qoffset..]);
            qoffset += utf8_len(ch).min(w.len() - qoffset);
        } else {
            scroffset += 1;
            qoffset += 1;
        }
        cno += 1;
    }
    let mut iend = match iend {
        Some(i) => i,
        None => {
            qidx[cno] = qoffset;
            scridx[cno] = scroffset;
            cno
        }
    };
    if loc > cno as i64 {
        return;
    }
    let loc = loc as usize;
    let mut beg_trunc = false;
    let mut end_trunc = false;
    if scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
        if scridx[ibeg] + DISPLAY_SIZE >= scridx[loc] + MIN_RIGHT_CUT {
            while scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
                iend -= 1;
            }
            end_trunc = true;
        } else {
            while scridx[loc] + MIN_RIGHT_CUT < scridx[iend] {
                iend -= 1;
                end_trunc = true;
            }
            while scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
                ibeg += 1;
                beg_trunc = true;
            }
        }
    }
    w.truncate(qidx[iend]);
    let prefix = format!("LINE {loc_line}: {}", if beg_trunc { "..." } else { "" });
    let mut scroffset = prefix.len() as i64;
    msg.extend_from_slice(prefix.as_bytes());
    msg.extend_from_slice(&w[qidx[ibeg].min(w.len())..]);
    if end_trunc {
        msg.extend_from_slice(b"...");
    }
    msg.push(b'\n');
    scroffset += scridx[loc] - scridx[ibeg];
    msg.extend(std::iter::repeat_n(b' ', scroffset.max(0) as usize));
    msg.extend_from_slice(b"^\n");
}
