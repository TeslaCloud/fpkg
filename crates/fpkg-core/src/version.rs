//! Versions (Flux's `X[.Y[.Z]][-suffix]`, canonicalised to three components) and
//! constraints. Suffixes order as semver pre-releases, before the release.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    #[error("invalid version '{input}': {reason}")]
    Version { input: String, reason: String },
    #[error("invalid constraint '{input}': {reason}")]
    Constraint { input: String, reason: String },
}

fn verr(input: &str, reason: impl Into<String>) -> VersionError {
    VersionError::Version { input: input.to_string(), reason: reason.into() }
}

/// A canonical version: three numeric components and an optional suffix.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The pre-release suffix after `-`, without the dash.
    pub suffix: Option<String>,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch, suffix: None }
    }

    /// Parses Flux's grammar and also returns how many numeric components were
    /// written (1 to 3). `fpkg check` warns below 3 and `fpkg publish` refuses.
    pub fn parse_with_components(input: &str) -> Result<(Version, u8), VersionError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(verr(input, "it is empty"));
        }
        let (core, suffix) = match s.split_once('-') {
            Some((c, x)) => (c, Some(x)),
            None => (s, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() > 3 {
            return Err(verr(input, "at most three numeric components are allowed"));
        }
        let mut nums = [0u64; 3];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(verr(input, format!("component '{p}' is not a number")));
            }
            // Leading zeros are accepted, as Flux's tonumber accepts them, and
            // dropped by canonicalisation ('1.02' is 1.2.0).
            nums[i] = p.parse().map_err(|_| verr(input, format!("component '{p}' is too large")))?;
        }
        let suffix = match suffix {
            None => None,
            Some(x) => {
                if x.is_empty() {
                    return Err(verr(input, "the suffix after '-' is empty"));
                }
                for ident in x.split('.') {
                    if ident.is_empty() || !ident.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        return Err(verr(
                            input,
                            "the suffix must be dot-separated identifiers of [0-9A-Za-z-]",
                        ));
                    }
                    // Semver: numeric identifiers have no leading zeros, which
                    // keeps ordering and equality in agreement (rc.01 vs rc.1).
                    if ident.len() > 1 && ident.starts_with('0') && ident.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(verr(input, format!("the numeric suffix identifier '{ident}' has a leading zero")));
                    }
                }
                Some(x.to_string())
            }
        };
        let v = Version { major: nums[0], minor: nums[1], patch: nums[2], suffix };
        Ok((v, parts.len() as u8))
    }

    /// Parses and canonicalises (`1.0` -> `1.0.0`).
    pub fn parse(input: &str) -> Result<Version, VersionError> {
        Self::parse_with_components(input).map(|(v, _)| v)
    }

    /// Parses a version that must already be written with three components, as
    /// `fpkg publish` and the registry require.
    pub fn parse_strict(input: &str) -> Result<Version, VersionError> {
        let (v, n) = Self::parse_with_components(input)?;
        if n != 3 {
            return Err(verr(input, "three components (X.Y.Z) are required"));
        }
        Ok(v)
    }

    pub fn is_prerelease(&self) -> bool {
        self.suffix.is_some()
    }

    /// The version written with `n` numeric components, as a constraint shows it.
    fn display_components(&self, n: u8) -> String {
        let mut s = match n {
            1 => format!("{}", self.major),
            2 => format!("{}.{}", self.major, self.minor),
            _ => format!("{}.{}.{}", self.major, self.minor, self.patch),
        };
        if let Some(x) = &self.suffix {
            s.push('-');
            s.push_str(x);
        }
        s
    }
}

/// Semver 2.0 pre-release ordering of two suffixes.
fn cmp_suffix(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('.');
    let mut ib = b.split('.');
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let nx = x.bytes().all(|c| c.is_ascii_digit());
                let ny = y.bytes().all(|c| c.is_ascii_digit());
                let o = match (nx, ny) {
                    // No leading zeros (parse refuses them), so length then text
                    // is numeric order, for numbers of any size.
                    (true, true) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.suffix, &other.suffix) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => cmp_suffix(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_components(3))
    }
}

impl FromStr for Version {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, VersionError> {
        Version::parse(s)
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Version::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A comparison operator of a constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// `== v` or a bare `v`
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
    /// `~> v`, Ruby's pessimistic operator
    Tilde,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "==",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Tilde => "~>",
        }
    }
}

/// One `op version` term. The number of written components matters for `~>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Comparator {
    pub op: Op,
    pub version: Version,
    pub components: u8,
}

impl Comparator {
    /// The exclusive upper bound of a `~>`: the last written component floats
    /// (`~> 1.2` is `< 2.0.0`, `~> 1.2.3` is `< 1.3.0`, `~> 1` is `< 2.0.0`).
    pub fn tilde_upper(&self) -> Version {
        let v = &self.version;
        if self.components >= 3 {
            Version::new(v.major, v.minor.saturating_add(1), 0)
        } else {
            Version::new(v.major.saturating_add(1), 0, 0)
        }
    }

    pub fn matches(&self, v: &Version) -> bool {
        match self.op {
            Op::Eq => v == &self.version,
            Op::Gt => v > &self.version,
            Op::Ge => v >= &self.version,
            Op::Lt => v < &self.version,
            Op::Le => v <= &self.version,
            Op::Tilde => v >= &self.version && v < &self.tilde_upper(),
        }
    }
}

impl fmt::Display for Comparator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.op.as_str(), self.version.display_components(self.components))
    }
}

/// A conjunction of comparators. Empty means any version (`*`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Constraint {
    pub comparators: Vec<Comparator>,
}

impl Constraint {
    pub fn any() -> Self {
        Constraint { comparators: Vec::new() }
    }

    pub fn is_any(&self) -> bool {
        self.comparators.is_empty()
    }

    /// `== v`
    pub fn exact(v: &Version) -> Self {
        Constraint { comparators: vec![Comparator { op: Op::Eq, version: v.clone(), components: 3 }] }
    }

    /// What `fpkg add` writes by default: `~> <major>.<minor>`.
    pub fn default_for(v: &Version) -> Self {
        Constraint {
            comparators: vec![Comparator { op: Op::Tilde, version: Version::new(v.major, v.minor, 0), components: 2 }],
        }
    }

    pub fn parse(input: &str) -> Result<Constraint, VersionError> {
        let cerr = |reason: String| VersionError::Constraint { input: input.to_string(), reason };
        let s = input.trim();
        if s.is_empty() || s == "*" {
            return Ok(Constraint::any());
        }
        let mut comparators = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(cerr("empty term between commas".into()));
            }
            if part == "*" {
                continue;
            }
            let (op, rest) = if let Some(r) = part.strip_prefix("==") {
                (Op::Eq, r)
            } else if let Some(r) = part.strip_prefix(">=") {
                (Op::Ge, r)
            } else if let Some(r) = part.strip_prefix("<=") {
                (Op::Le, r)
            } else if let Some(r) = part.strip_prefix("~>") {
                (Op::Tilde, r)
            } else if let Some(r) = part.strip_prefix('>') {
                (Op::Gt, r)
            } else if let Some(r) = part.strip_prefix('<') {
                (Op::Lt, r)
            } else if part.starts_with(|c: char| c.is_ascii_digit()) {
                (Op::Eq, part)
            } else {
                return Err(cerr(format!("'{part}' does not start with ==, >=, <=, ~>, >, < or a version")));
            };
            let (version, components) = Version::parse_with_components(rest).map_err(|e| match e {
                VersionError::Version { reason, .. } => cerr(reason),
                other => other,
            })?;
            comparators.push(Comparator { op, version, components });
        }
        Ok(Constraint { comparators })
    }

    /// Whether `v` satisfies the constraint. A pre-release matches only when a
    /// comparator names a pre-release of the same X.Y.Z; `*` matches anything.
    pub fn matches(&self, v: &Version) -> bool {
        if self.is_any() {
            return true;
        }
        if v.is_prerelease() && !self.allows_prerelease_of(v) {
            return false;
        }
        self.comparators.iter().all(|c| c.matches(v))
    }

    /// Whether a comparator names a pre-release with `v`'s X.Y.Z.
    pub fn allows_prerelease_of(&self, v: &Version) -> bool {
        self.comparators.iter().any(|c| {
            c.version.is_prerelease() && (c.version.major, c.version.minor, c.version.patch) == (v.major, v.minor, v.patch)
        })
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.comparators.is_empty() {
            return f.write_str("*");
        }
        for (i, c) in self.comparators.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

impl FromStr for Constraint {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, VersionError> {
        Constraint::parse(s)
    }
}

impl Serialize for Constraint {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Constraint {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Constraint::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn c(s: &str) -> Constraint {
        Constraint::parse(s).unwrap()
    }

    #[test]
    fn canonicalisation() {
        // The grammar's forms and the versions found in flux-ce.
        for (input, canonical, n) in [
            ("1.0", "1.0.0", 2),
            ("1.0.0", "1.0.0", 3),
            ("0.9.0", "0.9.0", 3),
            ("7.0.9", "7.0.9", 3),
            ("20211019", "20211019.0.0", 1),
            ("1.2.3-beta", "1.2.3-beta", 3),
            ("1-beta", "1.0.0-beta", 1),
            (" 2.1.1 ", "2.1.1", 3),
            ("1.02", "1.2.0", 2),
            ("1.2.3-rc.1", "1.2.3-rc.1", 3),
        ] {
            let (ver, comps) = Version::parse_with_components(input).unwrap();
            assert_eq!(ver.to_string(), canonical, "{input}");
            assert_eq!(comps, n, "{input}");
        }
        for bad in ["", "a", "1.", ".1", "1..2", "1.2.3.4", "1.2.3-", "1.2.3-be$ta", "v1.0", "1.0 beta", "-1", "1.0.0-rc.01", "1.0.0-00"] {
            assert!(Version::parse(bad).is_err(), "{bad} should not parse");
        }
        assert!(Version::parse_strict("1.0").is_err());
        assert!(Version::parse_strict("1.0.0").is_ok());
    }

    #[test]
    fn ordering() {
        // Flux's compare_version gets this wrong (1100 > 200); fpkg does not.
        assert!(!(v("1.10.0") > v("2.0.0")));
        assert!(v("1.10.0") < v("2.0.0"));
        assert!(v("1.10.0") > v("1.9.0"));
        assert!(v("20211019") > v("7.0.9"));
        assert_eq!(v("1.0"), v("1.0.0"));
        assert!(v("1.2.3-beta") < v("1.2.3"));
        assert!(v("1.2.3-beta") > v("1.2.2"));
        // The semver 2.0 example chain.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        // Ord and Eq agree: equal ordering means equal values.
        let all = ["1.0.0-rc.1", "1.0.0-rc.10", "1.0.0-rc.2", "1.0.0-0", "1.0.0-rc.1a", "1.0.0"];
        for a in all {
            for b in all {
                assert_eq!(v(a).cmp(&v(b)) == std::cmp::Ordering::Equal, v(a) == v(b), "{a} {b}");
            }
        }
        assert!(v("1.0.0-rc.2") < v("1.0.0-rc.10"));
    }

    #[test]
    fn constraint_table() {
        let table: &[(&str, &[&str], &[&str])] = &[
            ("1.2.3", &["1.2.3"], &["1.2.4", "1.2.2", "1.2.3-beta"]),
            ("== 1.2.3", &["1.2.3"], &["1.2.4", "1.3.0"]),
            ("== 1.2", &["1.2.0"], &["1.2.1"]),
            (">= 1.2", &["1.2.0", "1.2.9", "2.0.0", "10.0.0"], &["1.1.9", "1.0.0"]),
            ("~> 1.2", &["1.2.0", "1.2.9", "1.9.9"], &["2.0.0", "1.1.9"]),
            ("~> 1.2.3", &["1.2.3", "1.2.99"], &["1.3.0", "1.2.2", "2.0.0"]),
            // Flux's is_version treats '~> 1.2.0' like '~> 1.2'; fpkg follows Ruby.
            ("~> 1.2.0", &["1.2.0", "1.2.5"], &["1.3.0"]),
            ("~> 7.0", &["7.0.9", "7.5.0"], &["8.0.0", "6.9.9"]),
            ("~> 1", &["1.0.0", "1.9.0"], &["2.0.0", "0.9.0"]),
            ("< 2.0", &["1.9.9", "0.1.0"], &["2.0.0", "2.0.1"]),
            ("<= 2.0", &["2.0.0", "1.0.0"], &["2.0.1"]),
            ("> 1.0", &["1.0.1", "2.0.0"], &["1.0.0", "0.9.0"]),
            ("*", &["0.0.1", "99.0.0"], &[]),
            ("", &["1.0.0"], &[]),
            (">= 1.0, < 1.5", &["1.0.0", "1.4.9"], &["1.5.0", "0.9.0"]),
            (">=1.0,<1.5", &["1.2.0"], &["1.5.0"]),
            ("~> 1.2.3-beta", &["1.2.3-beta", "1.2.3-rc", "1.2.3", "1.2.4"], &["1.2.3-alpha", "1.3.0", "1.2.4-beta"]),
            // Pre-releases match only a constraint that names one of the same X.Y.Z.
            ("~> 1.2", &["1.3.0"], &["1.3.0-beta", "1.2.0-rc.1", "2.0.0-beta"]),
            ("< 2.0", &["1.9.9"], &["2.0.0-beta", "1.5.0-beta"]),
            (">= 1.2.3-beta", &["1.2.3-beta", "1.2.3", "1.3.0"], &["1.3.0-beta", "1.2.3-alpha"]),
            ("== 2.0.0-beta", &["2.0.0-beta"], &["2.0.0", "2.0.0-beta.2"]),
            ("*", &["2.0.0-beta"], &[]),
        ];
        for (cons, yes, no) in table {
            let k = c(cons);
            for y in *yes {
                assert!(k.matches(&v(y)), "{cons} should match {y}");
            }
            for n in *no {
                assert!(!k.matches(&v(n)), "{cons} should not match {n}");
            }
        }
    }

    #[test]
    fn constraint_parse_and_display() {
        assert_eq!(c("~> 1.2").to_string(), "~> 1.2");
        assert_eq!(c("~>1.2.3").to_string(), "~> 1.2.3");
        assert_eq!(c("1.2.3").to_string(), "== 1.2.3");
        assert_eq!(c(">= 1.0 , < 2").to_string(), ">= 1.0, < 2");
        assert_eq!(c("*").to_string(), "*");
        assert!(c("*").is_any());
        for bad in ["=1.0", "~ 1.0", "^1.0", ">= ", "1.0,", ">= x"] {
            assert!(Constraint::parse(bad).is_err(), "{bad}");
        }
        // Round trip through Display.
        for s in ["~> 1.2", ">= 1.0, < 2.0", "== 7.0.9", "~> 1.2.3-beta"] {
            assert_eq!(c(&c(s).to_string()), c(s));
        }
        assert_eq!(Constraint::default_for(&v("7.0.9")).to_string(), "~> 7.0");
    }

    #[test]
    fn flux_examples() {
        // Package:parse_version('~> 1.2.3-beta') -> x=1 y=2 z=3 suffix=beta op=~>
        let k = c("~> 1.2.3-beta");
        let cmp = &k.comparators[0];
        assert_eq!(cmp.op, Op::Tilde);
        assert_eq!((cmp.version.major, cmp.version.minor, cmp.version.patch), (1, 2, 3));
        assert_eq!(cmp.version.suffix.as_deref(), Some("beta"));
        assert_eq!(c(">= 1.2.0").comparators[0].op, Op::Ge);
        assert_eq!(v("1.0").to_string(), "1.0.0");
    }
}
