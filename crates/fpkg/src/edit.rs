//! Editing the project manifest for `fpkg add`/`remove`: the source is tokenised and
//! its blocks followed; anything ambiguous is refused so the caller prints the line.

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    NoDescribe,
    SeveralDescribe(usize),
    NoEnd,
    /// A `depends` call that is not a top-level statement of the block.
    Conditional(usize),
    /// A `depends` call fpkg cannot rewrite safely, with the line and why.
    Ambiguous(usize, &'static str),
    /// The file does not tokenise (an unfinished string or comment).
    Unparsable(usize),
    AlreadyPresent(String),
    NotPresent(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NoDescribe => write!(f, "there is no Package:describe(function(s) ... end) block"),
            EditError::SeveralDescribe(n) => write!(f, "there are {n} Package:describe blocks"),
            EditError::NoEnd => write!(f, "the end) that closes Package:describe was not found"),
            EditError::Conditional(line) => write!(f, "line {line} calls depends inside a conditional, loop or function"),
            EditError::Ambiguous(line, why) => write!(f, "line {line}: {why}"),
            EditError::Unparsable(line) => write!(f, "line {line}: an unfinished string or comment"),
            EditError::AlreadyPresent(id) => write!(f, "the project already depends on '{id}'"),
            EditError::NotPresent(id) => write!(f, "the project manifest has no s.depends '{id}'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    /// A string literal's value and its opening quote (`[` for long strings).
    Str(String, char),
    Num,
    Punct(char),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    /// 0-based lines of the first and last character.
    line: usize,
    end_line: usize,
    start: usize,
    end: usize,
}

/// The level of a long bracket at `i` (`[[` is 0, `[==[` is 2), if one opens there.
fn long_open(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while b.get(j) == Some(&b'=') {
        j += 1;
    }
    (b.get(j) == Some(&b'[')).then_some(j - i - 1)
}

/// The index after the long bracket opened at `i` with `level`.
fn long_close(b: &[u8], i: usize, level: usize) -> Option<usize> {
    let mut close = vec![b']'];
    close.extend(std::iter::repeat_n(b'=', level));
    close.push(b']');
    let from = i + level + 2;
    b.get(from..)?.windows(close.len()).position(|w| w == close.as_slice()).map(|p| from + p + close.len())
}

fn lex(src: &str) -> Result<Vec<Token>, EditError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 0;
    let lines_in = |from: usize, to: usize| b[from..to].iter().filter(|c| **c == b'\n').count();
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Comments: --[[ ]], --, //, /* */.
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            if let Some(level) = long_open(b, i + 2) {
                let end = long_close(b, i + 2, level).ok_or(EditError::Unparsable(line + 1))?;
                line += lines_in(i, end);
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
            let end = b[i + 2..].windows(2).position(|w| w == b"*/").map(|p| i + 2 + p + 2).ok_or(EditError::Unparsable(line + 1))?;
            line += lines_in(i, end);
            i = end;
            continue;
        }
        let start = i;
        let start_line = line;
        let tok = if c == b'\'' || c == b'"' {
            let mut j = i + 1;
            let mut value = Vec::new();
            loop {
                match b.get(j) {
                    None | Some(b'\n') => return Err(EditError::Unparsable(line + 1)),
                    Some(&q) if q == c => break,
                    Some(b'\\') => {
                        let e = *b.get(j + 1).ok_or(EditError::Unparsable(line + 1))?;
                        if e == b'\n' {
                            line += 1;
                        }
                        value.push(match e {
                            b'n' => b'\n',
                            b't' => b'\t',
                            other => other,
                        });
                        j += 2;
                    }
                    Some(&x) => {
                        value.push(x);
                        j += 1;
                    }
                }
            }
            i = j + 1;
            Tok::Str(String::from_utf8_lossy(&value).into_owned(), c as char)
        } else if let Some(level) = long_open(b, i) {
            let end = long_close(b, i, level).ok_or(EditError::Unparsable(line + 1))?;
            let value = String::from_utf8_lossy(&b[i + level + 2..end - level - 2]).into_owned();
            line += lines_in(i, end);
            i = end;
            Tok::Str(value, '[')
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            i = j;
            Tok::Name(src[start..j].to_string())
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let mut j = i;
            while j < b.len() {
                let x = b[j];
                let exponent_sign = (x == b'+' || x == b'-') && matches!(b[j - 1], b'e' | b'E' | b'p' | b'P');
                if x.is_ascii_alphanumeric() || x == b'.' || x == b'_' || exponent_sign {
                    j += 1;
                } else {
                    break;
                }
            }
            i = j;
            Tok::Num
        } else {
            // Punctuation, one byte at a time (a UTF-8 sequence becomes several).
            i += 1;
            Tok::Punct(c as char)
        };
        out.push(Token { tok, line: start_line, end_line: line, start, end: i });
    }
    Ok(out)
}

fn is_name(t: &Token, n: &str) -> bool {
    matches!(&t.tok, Tok::Name(x) if x == n)
}

fn is_punct(t: &Token, p: char) -> bool {
    t.tok == Tok::Punct(p)
}

/// One top-level `s.depends` call.
struct Call {
    /// The package id or path it names.
    id: String,
    /// Lines (0-based) of its first and last token.
    first_line: usize,
    last_line: usize,
    /// The whitespace between `depends` and its argument, and its quote.
    gap: String,
    quote: char,
}

struct Block {
    param: String,
    describe_line: usize,
    end_tok: Token,
    first_body_line: Option<usize>,
    calls: Vec<Call>,
}

fn find_block(src: &str) -> Result<Block, EditError> {
    let toks = lex(src)?;
    let n = toks.len();
    let at = |i: usize| toks.get(i);
    let mut starts = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        if is_name(tok, "Package")
            && at(i + 1).is_some_and(|t| is_punct(t, ':') || is_punct(t, '.'))
            && at(i + 2).is_some_and(|t| is_name(t, "describe"))
            && at(i + 3).is_some_and(|t| is_punct(t, '('))
            && at(i + 4).is_some_and(|t| is_name(t, "function"))
            && at(i + 5).is_some_and(|t| is_punct(t, '('))
            && at(i + 7).is_some_and(|t| is_punct(t, ')'))
            && let Some(Tok::Name(p)) = at(i + 6).map(|t| &t.tok)
        {
            starts.push((i, p.clone()));
        }
    }
    let (start, param) = match starts.len() {
        0 => return Err(EditError::NoDescribe),
        1 => starts.remove(0),
        k => return Err(EditError::SeveralDescribe(k)),
    };
    let describe_line = toks[start].line;
    let body = start + 8;
    let mut depth = 1usize;
    let mut calls = Vec::new();
    let mut i = body;
    let mut end_tok = None;
    while i < n {
        let t = &toks[i];
        match &t.tok {
            Tok::Name(k) if matches!(k.as_str(), "function" | "do" | "if" | "repeat") => depth += 1,
            Tok::Name(k) if matches!(k.as_str(), "end" | "until") => {
                depth -= 1;
                if depth == 0 {
                    if !at(i + 1).is_some_and(|t| is_punct(t, ')')) {
                        return Err(EditError::NoEnd);
                    }
                    end_tok = Some(t.clone());
                    break;
                }
            }
            Tok::Name(p) if *p == param
                && at(i + 1).is_some_and(|t| is_punct(t, '.') || is_punct(t, ':'))
                && at(i + 2).is_some_and(|t| is_name(t, "depends")) =>
            {
                let line = t.line + 1;
                if depth > 1 {
                    return Err(EditError::Conditional(line));
                }
                let dep = &toks[i + 2];
                let (id, quote, last) = match at(i + 3).map(|t| &t.tok) {
                    Some(Tok::Str(v, q)) => (v.clone(), *q, i + 3),
                    Some(Tok::Punct('(')) => {
                        let Some(Tok::Str(v, q)) = at(i + 4).map(|t| &t.tok) else {
                            return Err(EditError::Ambiguous(line, "the first argument of depends is not a string literal"));
                        };
                        let mut level = 0;
                        let mut j = i + 3;
                        while j < n {
                            if is_punct(&toks[j], '(') {
                                level += 1;
                            } else if is_punct(&toks[j], ')') {
                                level -= 1;
                                if level == 0 {
                                    break;
                                }
                            }
                            j += 1;
                        }
                        if j >= n {
                            return Err(EditError::Unparsable(line));
                        }
                        (v.clone(), *q, j)
                    }
                    _ => return Err(EditError::Ambiguous(line, "depends is used but not called with a string")),
                };
                // The call must have its lines to itself (a trailing `;` aside).
                let mut after = last + 1;
                if at(after).is_some_and(|x| is_punct(x, ';') && x.line == toks[last].end_line) {
                    after += 1;
                }
                let shares_before = i > 0 && toks[i - 1].end_line == t.line;
                let shares_after = at(after).is_some_and(|x| x.line == toks[last].end_line);
                if shares_before || shares_after {
                    return Err(EditError::Ambiguous(line, "the depends call shares its line with another statement"));
                }
                let next = &toks[i + 3];
                let gap = if next.line == dep.line { src[dep.end..next.start].to_string() } else { String::new() };
                calls.push(Call {
                    id,
                    first_line: t.line,
                    last_line: toks[last].end_line,
                    gap,
                    quote: if quote == '[' { '\'' } else { quote },
                });
                i = after;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let end_tok = end_tok.ok_or(EditError::NoEnd)?;
    let first_body_line = toks[body..].iter().take_while(|t| t.start < end_tok.start).map(|t| t.line).find(|l| *l != describe_line);
    Ok(Block { param, describe_line, end_tok, first_body_line, calls })
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

fn join(lines: Vec<String>, crlf: bool, trailing: bool) -> String {
    let sep = if crlf { "\r\n" } else { "\n" };
    let mut s = lines.join(sep);
    if trailing {
        s.push_str(sep);
    }
    s
}

/// The line `fpkg add` writes, given the call's spacing and quote style.
pub fn depends_line(param: &str, gap: &str, quote: char, id: &str, constraint: Option<&str>) -> String {
    match constraint {
        // `s.depends 'id', '~> 1.0'` would not parse, so constraints need
        // parentheses. A gap wider than one space is alignment and is kept.
        Some(c) => {
            let gap = if gap.len() > 1 && !gap.contains('\t') { gap } else { "" };
            format!("{param}.depends{gap}({quote}{id}{quote}, {quote}{c}{quote})")
        }
        None => format!("{param}.depends{}{quote}{id}{quote}", if gap.is_empty() { " " } else { gap }),
    }
}

/// Adds a depends line after the last one (or before the closing `end)`).
pub fn add(src: &str, id: &str, constraint: Option<&str>) -> Result<String, EditError> {
    let crlf = src.contains("\r\n");
    let trailing = src.ends_with('\n');
    let lines: Vec<&str> = src.lines().collect();
    let block = find_block(src)?;
    if block.calls.iter().any(|c| c.id == id) {
        return Err(EditError::AlreadyPresent(id.to_string()));
    }
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    match block.calls.last() {
        Some(last) => {
            let indent = indent_of(lines[last.first_line]);
            let new = format!("{indent}{}", depends_line(&block.param, &last.gap, last.quote, id, constraint));
            out.insert(last.last_line + 1, new);
        }
        None => {
            let end_line = block.end_tok.line;
            let end_text = lines.get(end_line).copied().unwrap_or("");
            if end_line == block.describe_line || !end_text[..block.end_tok.start - line_start(src, end_line)].trim().is_empty() {
                return Err(EditError::Ambiguous(end_line + 1, "the closing end) does not start its line"));
            }
            let indent = match block.first_body_line {
                Some(l) if l < end_line => indent_of(lines[l]).to_string(),
                _ => format!("{}  ", indent_of(lines[block.describe_line])),
            };
            let new = format!("{indent}{}", depends_line(&block.param, "", '\'', id, constraint));
            let mut at = end_line;
            if end_line > 0 && !lines[end_line - 1].trim().is_empty() && end_line - 1 != block.describe_line {
                out.insert(at, String::new());
                at += 1;
            }
            out.insert(at, new);
        }
    }
    Ok(join(out, crlf, trailing))
}

fn line_start(src: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    src.match_indices('\n').nth(line - 1).map(|(i, _)| i + 1).unwrap_or(src.len())
}

/// Removes every top-level depends call naming `id`.
pub fn remove(src: &str, id: &str) -> Result<String, EditError> {
    let crlf = src.contains("\r\n");
    let trailing = src.ends_with('\n');
    let lines: Vec<&str> = src.lines().collect();
    let block = find_block(src)?;
    let hits: Vec<&Call> = block.calls.iter().filter(|c| c.id == id).collect();
    if hits.is_empty() {
        return Err(EditError::NotPresent(id.to_string()));
    }
    let drop = |i: usize| hits.iter().any(|c| (c.first_line..=c.last_line).contains(&i));
    let out: Vec<String> = lines.iter().enumerate().filter(|(i, _)| !drop(*i)).map(|(_, s)| s.to_string()).collect();
    Ok(join(out, crlf, trailing))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLUX: &str = "Package:describe(function(s)\n  s.name        = 'Flux'\n  s.version     = '0.9.0'\n\n  s.depends     'sfs'\n  s.depends     'cable'\nend)\n";

    #[test]
    fn adds_after_the_last_depends() {
        let out = add(FLUX, "yaml", Some("~> 1.2")).unwrap();
        assert_eq!(
            out,
            "Package:describe(function(s)\n  s.name        = 'Flux'\n  s.version     = '0.9.0'\n\n  s.depends     'sfs'\n  s.depends     'cable'\n  s.depends     ('yaml', '~> 1.2')\nend)\n"
        );
        assert_eq!(add(FLUX, "sfs", None), Err(EditError::AlreadyPresent("sfs".into())));
    }

    #[test]
    fn adds_before_end_without_depends() {
        let src = "Package:describe(function(spec)\n    spec.name = \"X\"\nend)";
        assert_eq!(add(src, "sfs", Some("~> 7.0")).unwrap(), "Package:describe(function(spec)\n    spec.name = \"X\"\n\n    spec.depends('sfs', '~> 7.0')\nend)");
    }

    #[test]
    fn single_space_is_not_alignment() {
        let src = "Package:describe(function(s)\n  s.depends 'cable'\nend)\n";
        assert_eq!(add(src, "yaml", Some("~> 1.2")).unwrap(), "Package:describe(function(s)\n  s.depends 'cable'\n  s.depends('yaml', '~> 1.2')\nend)\n");
    }

    #[test]
    fn removes() {
        let out = remove(FLUX, "sfs").unwrap();
        assert!(!out.contains("'sfs'"));
        assert!(out.contains("'cable'"));
        assert_eq!(remove(FLUX, "nope"), Err(EditError::NotPresent("nope".into())));
        let with = add(FLUX, "yaml", Some("~> 1.2")).unwrap();
        assert_eq!(remove(&with, "yaml").unwrap(), FLUX);
        // Multi-line calls go whole.
        let multi = "Package:describe(function(s)\n  s.depends(\n    'a',\n    '~> 1.0'\n  )\n  s.depends 'b'\nend)\n";
        assert_eq!(remove(multi, "a").unwrap(), "Package:describe(function(s)\n  s.depends 'b'\nend)\n");
    }

    #[test]
    fn comments_and_strings_are_not_code() {
        let src = "Package:describe(function(s)\n  --[[ s.depends 'b'\n  end) ]]\n  s.summary = \"s.depends 'c'\"\n  // s.depends 'd'\n  /* s.depends 'e' */\n  s.depends 'a'\nend)\n";
        assert_eq!(remove(src, "b"), Err(EditError::NotPresent("b".into())));
        let out = add(src, "b", None).unwrap();
        assert!(out.ends_with("  s.depends 'a'\n  s.depends 'b'\nend)\n"), "{out}");
        assert!(remove(&out, "b").unwrap() == src);
    }

    #[test]
    fn refuses_ambiguous_files() {
        assert_eq!(add("return 1\n", "a", None), Err(EditError::NoDescribe));
        let two = format!("{FLUX}{FLUX}");
        assert_eq!(add(&two, "a", None), Err(EditError::SeveralDescribe(2)));
        let cond = "Package:describe(function(s)\n  s.version = '1.0.0'\n  if IS_DEVELOPMENT then\n    s.depends 'dev'\n  end\nend)\n";
        assert_eq!(add(cond, "a", None), Err(EditError::Conditional(4)));
        // Badly indented, still inside the if: the structure decides, not the indentation.
        let flat = "Package:describe(function(s)\n  if IS_DEVELOPMENT then\n  s.depends 'dev'\n  end\nend)\n";
        assert_eq!(add(flat, "a", None), Err(EditError::Conditional(3)));
        let inline = "Package:describe(function(s)\n  s.version = '1.0.0'\n  if X then s.depends 'dev' end\nend)\n";
        assert_eq!(remove(inline, "dev"), Err(EditError::Conditional(3)));
        let one_line = "Package:describe(function(s)\n  s.depends 'c'; s.depends 'b'\nend)\n";
        assert!(matches!(remove(one_line, "b"), Err(EditError::Ambiguous(2, _))));
        let computed = "Package:describe(function(s)\n  s.depends(NAME)\nend)\n";
        assert!(matches!(add(computed, "a", None), Err(EditError::Ambiguous(2, _))));
        assert_eq!(add("Package:describe(function(s)\n  s.name = 'x'\n", "a", None), Err(EditError::NoEnd));
        assert!(matches!(add("Package:describe(function(s)\n  s.name = 'x\nend)", "a", None), Err(EditError::Unparsable(_))));
    }

    #[test]
    fn body_indentation_differs_from_depends() {
        // The first body line is indented differently from the depends lines.
        let src = "Package:describe(function(s)\n      s.name = 'x'\n  s.depends 'a'\nend)\n";
        let out = add(src, "b", None).unwrap();
        assert_eq!(out, "Package:describe(function(s)\n      s.name = 'x'\n  s.depends 'a'\n  s.depends 'b'\nend)\n");
        assert_eq!(remove(src, "a").unwrap(), "Package:describe(function(s)\n      s.name = 'x'\nend)\n");
    }

    #[test]
    fn nested_functions_are_followed() {
        let src = "Package:describe(function(s)\n  local f = function() return 1 end\n  for i = 1, 2 do end\n  s.depends 'a'\nend)\nprint('after')\n";
        let out = add(src, "b", None).unwrap();
        assert!(out.contains("  s.depends 'a'\n  s.depends 'b'\nend)\nprint('after')\n"), "{out}");
    }

    #[test]
    fn keeps_crlf() {
        let src = FLUX.replace('\n', "\r\n");
        let out = add(&src, "yaml", None).unwrap();
        assert!(out.contains("  s.depends     'yaml'\r\nend)\r\n"));
    }
}
