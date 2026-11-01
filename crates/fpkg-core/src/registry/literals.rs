//! The string literals a published spec assigns to `version` and `name`, which
//! the server and `FileRegistry` both check against the metadata.

/// Every string literal assigned to `<expr>.<field>` in `source`, in order.
/// Comments (Lua and GLua) are skipped; strings elsewhere are skipped whole.
pub fn assigned_literals(source: &str, field: &str) -> Vec<String> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // Comments.
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            if let Some((_, end)) = long_bracket(b, i + 2) {
                i = end;
            } else {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // Strings that are not assignments of interest.
        if c == b'\'' || c == b'"' {
            i = quoted(b, i).map(|(_, e)| e).unwrap_or(b.len());
            continue;
        }
        if c == b'[' && let Some((_, end)) = long_bracket(b, i) {
            i = end;
            continue;
        }
        // `.field` followed by `=` (not `==`) and a literal.
        if c == b'.' && b[i + 1..].starts_with(field.as_bytes()) {
            let after = i + 1 + field.len();
            let ident_end = b.get(after).is_none_or(|x| !(x.is_ascii_alphanumeric() || *x == b'_'));
            let prev_ok = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b']' || b[i - 1] == b')');
            if ident_end && prev_ok {
                let mut j = skip_ws(b, after);
                if b.get(j) == Some(&b'=') && b.get(j + 1) != Some(&b'=') {
                    j = skip_ws(b, j + 1);
                    let lit = match b.get(j) {
                        Some(b'\'') | Some(b'"') => quoted(b, j),
                        Some(b'[') => long_bracket(b, j),
                        _ => None,
                    };
                    if let Some((value, end)) = lit {
                        out.push(value);
                        i = end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\r' || b[i] == b'\n') {
        i += 1;
    }
    i
}

/// A quoted string at `i`: its decoded value and the index after it.
fn quoted(b: &[u8], i: usize) -> Option<(String, usize)> {
    let q = b[i];
    let mut j = i + 1;
    let mut out = Vec::new();
    while j < b.len() {
        match b[j] {
            x if x == q => return Some((String::from_utf8_lossy(&out).into_owned(), j + 1)),
            b'\n' => return None,
            b'\\' => {
                j += 1;
                let e = *b.get(j)?;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    b'0'..=b'9' => {
                        let mut n = 0u32;
                        let mut k = 0;
                        while k < 3 && j < b.len() && b[j].is_ascii_digit() {
                            n = n * 10 + (b[j] - b'0') as u32;
                            j += 1;
                            k += 1;
                        }
                        out.push(n.min(255) as u8);
                        continue;
                    }
                    other => out.push(other),
                }
                j += 1;
            }
            x => {
                out.push(x);
                j += 1;
            }
        }
    }
    None
}

/// A long bracket `[[...]]` / `[==[...]==]` at `i`: content and end index.
fn long_bracket(b: &[u8], i: usize) -> Option<(String, usize)> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    let mut level = 0;
    while b.get(j) == Some(&b'=') {
        level += 1;
        j += 1;
    }
    if b.get(j) != Some(&b'[') {
        return None;
    }
    let start = j + 1;
    let mut close = vec![b']'];
    close.extend(std::iter::repeat_n(b'=', level));
    close.push(b']');
    let rest = &b[start..];
    let pos = rest.windows(close.len()).position(|w| w == close.as_slice())?;
    let mut content = &rest[..pos];
    // A newline right after the opening bracket is not part of the string.
    if content.first() == Some(&b'\n') {
        content = &content[1..];
    }
    Some((String::from_utf8_lossy(content).into_owned(), start + pos + close.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_literals() {
        let src = "-- s.version = '9.9.9'\n/* s.version = '8' */\nPackage:describe(function(s)\n  s.name        = \"Srlion's Fast Serializer\"\n  s.version     = '7.0.9'\n  s.summary = 'x.version = 1'\n  if s.version == '1' then end\n  t.name=[[Long\nName]]\nend)";
        assert_eq!(assigned_literals(src, "version"), vec!["7.0.9"]);
        assert_eq!(assigned_literals(src, "name"), vec!["Srlion's Fast Serializer", "Long\nName"]);
        assert_eq!(assigned_literals("s.version = get()", "version"), Vec::<String>::new());
        assert_eq!(assigned_literals("s.versions = '1'", "version"), Vec::<String>::new());
        assert_eq!(assigned_literals("s.name = 'a\\'b'", "name"), vec!["a'b"]);
        assert_eq!(assigned_literals("s.name = [==[x]]y]==]", "name"), vec!["x]]y"]);
    }
}
