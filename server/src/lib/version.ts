// Versions and constraints; must agree with fpkg-core's version.rs. Components are digit
// strings so any length compares exactly; a suffix sorts before its release, as in semver.

export interface Version {
  major: string;
  minor: string;
  patch: string;
  /** The pre-release suffix without its `-`, or null. */
  suffix: string | null;
  /** How many numeric components were written (1, 2 or 3). */
  written: 1 | 2 | 3;
}

/** Longest numeric component, in digits; keeps sort keys fixed width. */
export const MAX_DIGITS = 20;

const VERSION_RE = /^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

function stripZeros(n: string): string {
  return n.replace(/^0+(?=\d)/, "");
}

/** Parses a version as written in a spec or a URL; null if it is not one. */
export function parseVersion(text: string): Version | null {
  const m = VERSION_RE.exec(text);
  if (!m) return null;
  const parts = [m[1], m[2], m[3]];
  if (parts.some((p) => p !== undefined && stripZeros(p).length > MAX_DIGITS)) return null;
  const suffix = m[4] ?? null;
  if (suffix !== null) {
    for (const id of suffix.split(".")) {
      // semver: numeric identifiers have no leading zeros.
      if (/^\d+$/.test(id) && (id.length > 1 && id.startsWith("0"))) return null;
      if (/^\d+$/.test(id) && id.length > MAX_DIGITS) return null;
    }
  }
  const written = (m[3] !== undefined ? 3 : m[2] !== undefined ? 2 : 1) as 1 | 2 | 3;
  return {
    major: stripZeros(m[1]!),
    minor: stripZeros(m[2] ?? "0"),
    patch: stripZeros(m[3] ?? "0"),
    suffix,
    written,
  };
}

/** `X.Y.Z[-suffix]`. */
export function formatVersion(v: Version): string {
  return `${v.major}.${v.minor}.${v.patch}${v.suffix !== null ? `-${v.suffix}` : ""}`;
}

/** The canonical form of a version string, or null if it does not parse. */
export function canonicalize(text: string): string | null {
  const v = parseVersion(text);
  return v ? formatVersion(v) : null;
}

/** True when `text` is a version written canonically, as the registry stores them. */
export function isCanonical(text: string): boolean {
  return canonicalize(text) === text;
}

function compareNumeric(a: string, b: string): number {
  if (a.length !== b.length) return a.length < b.length ? -1 : 1;
  return a < b ? -1 : a > b ? 1 : 0;
}

/** semver 2.0 pre-release precedence. */
function compareSuffix(a: string, b: string): number {
  const xs = a.split(".");
  const ys = b.split(".");
  for (let i = 0; i < Math.min(xs.length, ys.length); i++) {
    const x = xs[i]!;
    const y = ys[i]!;
    const xn = /^\d+$/.test(x);
    const yn = /^\d+$/.test(y);
    let c: number;
    if (xn && yn) c = compareNumeric(x, y);
    else if (xn) c = -1;
    else if (yn) c = 1;
    else c = x < y ? -1 : x > y ? 1 : 0;
    if (c !== 0) return c;
  }
  return xs.length === ys.length ? 0 : xs.length < ys.length ? -1 : 1;
}

export function compareVersions(a: Version, b: Version): number {
  const c = compareNumeric(a.major, b.major) || compareNumeric(a.minor, b.minor) || compareNumeric(a.patch, b.patch);
  if (c !== 0) return c;
  if (a.suffix === null || b.suffix === null) {
    if (a.suffix === b.suffix) return 0;
    return a.suffix === null ? 1 : -1;
  }
  return compareSuffix(a.suffix, b.suffix);
}

/**
 * A key that sorts bytewise as `compareVersions` orders: components zero-padded to 20 digits,
 * then `~` (no suffix) or `!`-joined identifiers (numeric as `0` + 20 digits, else `1` + text).
 * `!` sorts below every identifier character and `~` above.
 */
export function sortKey(v: Version): string {
  const pad = (n: string) => n.padStart(MAX_DIGITS, "0");
  const base = `${pad(v.major)}.${pad(v.minor)}.${pad(v.patch)}`;
  if (v.suffix === null) return `${base}~`;
  const ids = v.suffix.split(".").map((id) => (/^\d+$/.test(id) ? `0${pad(id)}` : `1${id}`));
  return `${base}!${ids.join("!")}`;
}

export type Op = "==" | ">=" | ">" | "<=" | "<" | "~>";

export interface Requirement {
  op: Op;
  version: Version;
}

/** A conjunction of requirements; empty matches every version. */
export interface Constraint {
  requirements: Requirement[];
}

const REQUIREMENT_RE = /^(==|>=|<=|~>|>|<)?\s*(\S+)$/;

/**
 * Parses a constraint: `*` or empty for any version, else a comma list of
 * `[op] version` with op one of `==` (the default), `>=`, `>`, `<=`, `<`,
 * `~>`. Null if it does not parse.
 */
export function parseConstraint(text: string): Constraint | null {
  const t = text.trim();
  if (t === "" || t === "*") return { requirements: [] };
  const requirements: Requirement[] = [];
  for (const part of t.split(",")) {
    const m = REQUIREMENT_RE.exec(part.trim());
    if (!m) return null;
    const version = parseVersion(m[2]!);
    if (!version) return null;
    requirements.push({ op: (m[1] ?? "==") as Op, version });
  }
  return { requirements };
}

function increment(n: string): string {
  return (BigInt(n) + 1n).toString();
}

/** Exclusive upper bound of `~> v`: `~> 1` and `~> 1.2` stay below `2.0.0`, `~> 1.2.3` below `1.3.0`. */
export function pessimisticUpperBound(v: Version): Version {
  if (v.written === 3) return { major: v.major, minor: increment(v.minor), patch: "0", suffix: null, written: 3 };
  return { major: increment(v.major), minor: "0", patch: "0", suffix: null, written: 3 };
}

export function satisfiesRequirement(v: Version, r: Requirement): boolean {
  const c = compareVersions(v, r.version);
  switch (r.op) {
    case "==":
      return c === 0;
    case ">=":
      return c >= 0;
    case ">":
      return c > 0;
    case "<=":
      return c <= 0;
    case "<":
      return c < 0;
    case "~>":
      return c >= 0 && compareVersions(v, pessimisticUpperBound(r.version)) < 0;
  }
}

/**
 * True when `v` meets every requirement of `c`. A pre-release satisfies only when some
 * requirement names a pre-release of the same X.Y.Z (`>= 2.0.0-beta` admits `2.0.0-rc1`).
 */
export function satisfies(v: Version, c: Constraint): boolean {
  if (v.suffix !== null) {
    const opted = c.requirements.some(
      (r) => r.version.suffix !== null && r.version.major === v.major && r.version.minor === v.minor && r.version.patch === v.patch,
    );
    if (!opted) return false;
  }
  return c.requirements.every((r) => satisfiesRequirement(v, r));
}

/** `satisfies` on strings; false when either does not parse. */
export function matches(version: string, constraint: string): boolean {
  const v = parseVersion(version);
  const c = parseConstraint(constraint);
  return v !== null && c !== null && satisfies(v, c);
}

/** Package ids. */
export const ID_RE = /^[a-z][a-z0-9_]{0,63}$/;

export function isValidId(id: string): boolean {
  return ID_RE.test(id);
}
