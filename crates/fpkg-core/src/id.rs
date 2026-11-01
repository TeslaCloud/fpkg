//! Package ids: `^[a-z][a-z0-9_]{0,63}$`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A validated package id. Registry names are ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid package id '{id}': {reason} (ids match ^[a-z][a-z0-9_]{{0,63}}$)")]
pub struct IdError {
    pub id: String,
    pub reason: &'static str,
}

impl PackageId {
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        match Self::problem(&s) {
            None => Ok(PackageId(s)),
            Some(reason) => Err(IdError { id: s, reason }),
        }
    }

    /// Why `s` is not an id, or `None` if it is one.
    pub fn problem(s: &str) -> Option<&'static str> {
        let bytes = s.as_bytes();
        match bytes.first() {
            None => return Some("it is empty"),
            Some(c) if !c.is_ascii_lowercase() => return Some("it must start with a lowercase letter"),
            _ => {}
        }
        if bytes.len() > 64 {
            return Some("it is longer than 64 characters");
        }
        if !bytes.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_') {
            return Some("only lowercase letters, digits and '_' are allowed");
        }
        None
    }

    pub fn is_valid(s: &str) -> bool {
        Self::problem(s).is_none()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PackageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for PackageId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        PackageId::new(s)
    }
}

impl Serialize for PackageId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackageId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        PackageId::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        for ok in ["a", "sfs", "active_record", "utf8", "a_1", &"a".repeat(64)] {
            assert!(PackageId::is_valid(ok), "{ok}");
        }
        for bad in ["", "1a", "_a", "Active", "a-b", "a.b", "../x", &"a".repeat(65), "é"] {
            assert!(!PackageId::is_valid(bad), "{bad}");
        }
    }
}
