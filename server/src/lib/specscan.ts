// Finds the string literals assigned to `<var>.version` and `<var>.name` in a packagespec without
// evaluating it, using a Lua/GLua tokenizer; any non-literal assignment is reported, not guessed.

export interface FieldScan {
  /** The literal values assigned, in source order. */
  literals: string[];
  /** Lines where the field is assigned something other than a literal. */
  computed: number[];
}

export interface SpecScan {
  version: FieldScan;
  name: FieldScan;
  /** Set when the source could not be tokenized (unterminated string...). */
  error: string | null;
}

type Tok =
  | { t: "name"; v: string; line: number }
  | { t: "string"; v: string; line: number }
  | { t: "sym"; v: string; line: number }
  | { t: "other"; line: number };

const SYMBOLS = ["...", "..", "==", "~=", "!=", "<=", ">=", "&&", "||", "::", "<<", ">>", "//"];

function longBracketLevel(src: string, i: number): number {
  // At `[`: returns the level of `[==[`, or -1 if it is not a long bracket.
  let j = i + 1;
  let level = 0;
  while (src[j] === "=") {
    level++;
    j++;
  }
  return src[j] === "[" ? level : -1;
}

function readLongBracket(src: string, i: number, level: number): { text: string; end: number } | null {
  const open = i + level + 2;
  const close = "]" + "=".repeat(level) + "]";
  const end = src.indexOf(close, open);
  if (end < 0) return null;
  let text = src.slice(open, end);
  // Lua skips a newline right after the opening bracket.
  if (text.startsWith("\r\n")) text = text.slice(2);
  else if (text.startsWith("\n") || text.startsWith("\r")) text = text.slice(1);
  return { text, end: end + close.length };
}

/**
 * A quoted string with its escapes. Lua strings are bytes: `\\xNN` and
 * `\\ddd` give one byte each, so the value is assembled as bytes and decoded
 * as UTF-8 at the end (`'Caf\\xc3\\xa9'` is `Café`, as the game sees it).
 */
function readQuoted(src: string, i: number): { text: string; end: number } | null {
  const q = src[i];
  const enc = new TextEncoder();
  const bytes: number[] = [];
  const text = (s: string) => bytes.push(...enc.encode(s));
  let j = i + 1;
  while (j < src.length) {
    const c = src[j]!;
    if (c === q) return { text: new TextDecoder().decode(Uint8Array.from(bytes)), end: j + 1 };
    if (c === "\n") return null;
    if (c !== "\\") {
      // A whole code point (a surrogate pair stays together).
      const cp = src.codePointAt(j)!;
      const ch = String.fromCodePoint(cp);
      text(ch);
      j += ch.length;
      continue;
    }
    const e = src[j + 1];
    j += 2;
    switch (e) {
      case "n": bytes.push(10); break;
      case "t": bytes.push(9); break;
      case "r": bytes.push(13); break;
      case "a": bytes.push(7); break;
      case "b": bytes.push(8); break;
      case "f": bytes.push(12); break;
      case "v": bytes.push(11); break;
      case "\\": bytes.push(92); break;
      case '"': bytes.push(34); break;
      case "'": bytes.push(39); break;
      case "\n": bytes.push(10); break;
      case "x": {
        const hex = src.slice(j, j + 2);
        if (!/^[0-9A-Fa-f]{2}$/.test(hex)) return null;
        bytes.push(parseInt(hex, 16));
        j += 2;
        break;
      }
      case "z":
        while (j < src.length && /\s/.test(src[j]!)) j++;
        break;
      case "u": {
        const m = /^\{([0-9A-Fa-f]{1,8})\}/.exec(src.slice(j));
        if (!m) return null;
        const cp = parseInt(m[1]!, 16);
        if (cp > 0x10ffff) return null;
        text(String.fromCodePoint(cp));
        j += m[0].length;
        break;
      }
      default: {
        const m = /^\d{1,3}/.exec(src.slice(j - 1));
        if (!m || parseInt(m[0], 10) > 255) return null;
        bytes.push(parseInt(m[0], 10));
        j += m[0].length - 1;
      }
    }
  }
  return null;
}

/** Tokenizes Lua or GLua source; comments and whitespace are dropped. */
export function tokenize(src: string): { toks: Tok[]; error: string | null } {
  const toks: Tok[] = [];
  let i = 0;
  let line = 1;
  const advance = (to: number) => {
    for (let k = i; k < to; k++) if (src[k] === "\n") line++;
    i = to;
  };
  while (i < src.length) {
    const c = src[i]!;
    if (c === "\n" || c === " " || c === "\t" || c === "\r" || c === "\f" || c === "\v") {
      advance(i + 1);
      continue;
    }
    // Comments: `--[[ ]]`, `-- ...`, GLua `/* */` and `// ...`.
    if (c === "-" && src[i + 1] === "-") {
      if (src[i + 2] === "[") {
        const level = longBracketLevel(src, i + 2);
        if (level >= 0) {
          const r = readLongBracket(src, i + 2, level);
          if (!r) return { toks, error: `unterminated long comment on line ${line}` };
          advance(r.end);
          continue;
        }
      }
      const nl = src.indexOf("\n", i);
      advance(nl < 0 ? src.length : nl);
      continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      const end = src.indexOf("*/", i + 2);
      if (end < 0) return { toks, error: `unterminated comment on line ${line}` };
      advance(end + 2);
      continue;
    }
    if (c === "/" && src[i + 1] === "/") {
      const nl = src.indexOf("\n", i);
      advance(nl < 0 ? src.length : nl);
      continue;
    }
    if (c === "'" || c === '"') {
      const r = readQuoted(src, i);
      if (!r) return { toks, error: `unterminated or malformed string on line ${line}` };
      toks.push({ t: "string", v: r.text, line });
      advance(r.end);
      continue;
    }
    if (c === "[") {
      const level = longBracketLevel(src, i);
      if (level >= 0) {
        const r = readLongBracket(src, i, level);
        if (!r) return { toks, error: `unterminated long string on line ${line}` };
        toks.push({ t: "string", v: r.text, line });
        advance(r.end);
        continue;
      }
    }
    if (/[A-Za-z_]/.test(c)) {
      const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i, i + 256))!;
      toks.push({ t: "name", v: m[0], line });
      advance(i + m[0].length);
      continue;
    }
    if (/[0-9]/.test(c) || (c === "." && /[0-9]/.test(src[i + 1] ?? ""))) {
      const m = /^(0[xX][0-9A-Fa-f_.]*([pP][+-]?\d+)?|[0-9_.]+([eE][+-]?\d+)?)[A-Za-z]*/.exec(src.slice(i, i + 256))!;
      toks.push({ t: "other", line });
      advance(i + m[0].length);
      continue;
    }
    const sym = SYMBOLS.find((s) => src.startsWith(s, i)) ?? c;
    toks.push({ t: "sym", v: sym, line });
    advance(i + sym.length);
  }
  return { toks, error: null };
}

/** True when the expression starting at `toks[k]` is a single string literal. */
function literalAt(toks: Tok[], k: number): string | null {
  const v = toks[k];
  if (!v || v.t !== "string") return null;
  // The literal must be the whole right-hand side, not `'1.' .. x` or `'a' or b`.
  const next = toks[k + 1];
  if (!next) return v.v;
  if (next.t === "sym") return [";", ")", "}", ","].includes(next.v) ? v.v : null;
  if (next.t === "name" && ["and", "or"].includes(next.v)) return null;
  return v.v;
}

/** Scans a packagespec for the `version` and `name` assignments. */
export function scanSpec(src: string): SpecScan {
  const { toks, error } = tokenize(src);
  const out: SpecScan = { version: { literals: [], computed: [] }, name: { literals: [], computed: [] }, error };
  for (let k = 0; k + 3 < toks.length + 1; k++) {
    const obj = toks[k];
    const dot = toks[k + 1];
    const field = toks[k + 2];
    const eq = toks[k + 3];
    if (!obj || obj.t !== "name" || !dot || dot.t !== "sym" || dot.v !== "." || !field || field.t !== "name" || !eq || eq.t !== "sym" || eq.v !== "=") continue;
    if (field.v !== "version" && field.v !== "name") continue;
    // `a.b.version = ...` and `x:y.version` are other tables' fields.
    const before = toks[k - 1];
    if (before && before.t === "sym" && (before.v === "." || before.v === ":")) continue;
    const target = field.v === "version" ? out.version : out.name;
    const value = literalAt(toks, k + 4);
    if (value === null) target.computed.push(field.line);
    else target.literals.push(value);
  }
  return out;
}
