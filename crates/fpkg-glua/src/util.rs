//! The engine half of `util` (CRC, hashes, Base64, JSON, KeyValues) and the clocks
//! (`SysTime`, `CurTime`, `RealTime`, `FrameTime`, `engine.TickInterval`, ...).

use base64::Engine as _;
use md5::Digest as _;
use nl::mlua::{self, Lua, MultiValue, Table, Value};

use crate::json::{JsonOptions, from_json, to_json};
use crate::keyvalues::{KeyValue, keyvalues_to_table, parse_keyvalues, table_to_keyvalues};
use crate::runtime::{TICK, arg, as_bytes, bad_argument, state, truthy};

/// `util.CRC`: the CRC-32 of the bytes, as a decimal string.
pub fn crc(bytes: &[u8]) -> String {
    crc32fast::hash(bytes).to_string()
}

/// The account ids of SteamID64s begin here.
const STEAM64_BASE: u64 = 76_561_197_960_265_728;

/// The largest account number of a SteamID: a larger `Z` counts as this one, as in the game.
const MAX_ACCOUNT: u64 = i32::MAX as u64;

/// `util.SteamIDTo64`: `STEAM_X:Y:Z` as a SteamID64, `"0"` for anything else.
pub fn steamid_to_64(id: &str) -> String {
    let parse = || -> Option<u64> {
        let rest = id.trim().strip_prefix("STEAM_")?;
        let mut parts = rest.split(':');
        let _universe: u64 = parts.next()?.parse().ok()?;
        let y: u64 = parts.next()?.parse().ok()?;
        let z = parts.next()?;
        if parts.next().is_some() || y > 1 || z.is_empty() || !z.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let z = z.parse::<u64>().unwrap_or(u64::MAX).min(MAX_ACCOUNT);
        Some(STEAM64_BASE + z * 2 + y)
    };
    parse().map_or_else(|| "0".into(), |n| n.to_string())
}

/// `util.SteamIDFrom64`: a SteamID64 as `STEAM_0:Y:Z`, `"STEAM_0:0:0"` for anything else.
pub fn steamid_from_64(id: &str) -> String {
    match id
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_sub(STEAM64_BASE))
    {
        Some(account) => format!("STEAM_0:{}:{}", account % 2, account / 2),
        None => "STEAM_0:0:0".into(),
    }
}

/// `util.Base64Encode`: lines of 76 characters joined by `\n` unless `inline`.
pub fn base64_encode(bytes: &[u8], inline: bool) -> String {
    let s = base64::engine::general_purpose::STANDARD.encode(bytes);
    if inline {
        return s;
    }
    s.as_bytes()
        .chunks(76)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `util.Base64Decode`: blanks are skipped; `None` for anything else that is not Base64.
pub fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    let clean: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD.decode(clean).ok()
}

fn bytes_arg(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Vec<u8>> {
    args.get(n - 1)
        .and_then(as_bytes)
        .ok_or_else(|| bad_argument(lua, args, n, "string"))
}

fn table_arg(lua: &Lua, args: &MultiValue, n: usize) -> mlua::Result<Table> {
    match arg(args, n) {
        Value::Table(t) => Ok(t),
        _ => Err(bad_argument(lua, args, n, "table")),
    }
}

fn ordered(lua: &Lua, entries: &[(Vec<u8>, KeyValue)]) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (i, (k, v)) in entries.iter().enumerate() {
        let e = lua.create_table()?;
        e.raw_set("Key", lua.create_string(k)?)?;
        let v = match v {
            KeyValue::String(s) => Value::String(lua.create_string(s)?),
            KeyValue::Int(n) => Value::Integer(i64::from(*n)),
            KeyValue::Float(f) => Value::Number(f64::from(*f)),
            KeyValue::Block(b) => Value::Table(ordered(lua, b)?),
        };
        e.raw_set("Value", v)?;
        t.raw_set(i + 1, e)?;
    }
    Ok(t)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    let util: Table = match g.get::<Value>("util")? {
        Value::Table(t) => t,
        _ => lua.create_table()?,
    };
    util.set(
        "CRC",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            Ok(crc(&bytes_arg(lua, &args, 1)?))
        })?,
    )?;
    util.set(
        "MD5",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            Ok(hex::encode(md5::Md5::digest(bytes_arg(lua, &args, 1)?)))
        })?,
    )?;
    util.set(
        "SHA1",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            Ok(hex::encode(sha1::Sha1::digest(bytes_arg(lua, &args, 1)?)))
        })?,
    )?;
    util.set(
        "SHA256",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            Ok(hex::encode(sha2::Sha256::digest(bytes_arg(lua, &args, 1)?)))
        })?,
    )?;
    util.set(
        "Base64Encode",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let b = bytes_arg(lua, &args, 1)?;
            Ok(base64_encode(&b, truthy(&arg(&args, 2))))
        })?,
    )?;
    util.set(
        "Base64Decode",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let b = bytes_arg(lua, &args, 1)?;
            match base64_decode(&b) {
                Some(d) => Ok(Value::String(lua.create_string(d)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    util.set(
        "TableToJSON",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let t = table_arg(lua, &args, 1)?;
            match to_json(&t, truthy(&arg(&args, 2)))? {
                Some(b) => Ok(Value::String(lua.create_string(b)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    util.set(
        "JSONToTable",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let text = bytes_arg(lua, &args, 1)?;
            let opts = JsonOptions {
                ignore_limits: truthy(&arg(&args, 2)),
                ignore_conversions: truthy(&arg(&args, 3)),
            };
            from_json(lua, &text, opts)
        })?,
    )?;
    util.set(
        "KeyValuesToTable",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let text = bytes_arg(lua, &args, 1)?;
            Ok(
                keyvalues_to_table(lua, &text, truthy(&arg(&args, 2)), truthy(&arg(&args, 3)))?
                    .map_or(Value::Table(lua.create_table()?), Value::Table),
            )
        })?,
    )?;
    util.set(
        "KeyValuesToTablePreserveOrder",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let text = bytes_arg(lua, &args, 1)?;
            match parse_keyvalues(&text, truthy(&arg(&args, 2)), truthy(&arg(&args, 3))) {
                Some((_, entries)) => ordered(lua, &entries),
                None => lua.create_table(),
            }
        })?,
    )?;
    util.set(
        "TableToKeyValues",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let t = table_arg(lua, &args, 1)?;
            let root = as_bytes(&arg(&args, 2)).unwrap_or_else(|| b"TableToKeyValues".to_vec());
            lua.create_string(table_to_keyvalues(&t, &root)?)
        })?,
    )?;
    util.set(
        "SteamIDTo64",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let b = bytes_arg(lua, &args, 1)?;
            Ok(steamid_to_64(&String::from_utf8_lossy(&b)))
        })?,
    )?;
    util.set(
        "SteamIDFrom64",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let b = bytes_arg(lua, &args, 1)?;
            Ok(steamid_from_64(&String::from_utf8_lossy(&b)))
        })?,
    )?;
    g.set("util", util)?;

    let now = crate::runtime::function(lua, |lua, ()| Ok(state(lua).now()))?;
    for name in ["SysTime", "CurTime", "RealTime", "UnPredictedCurTime"] {
        g.set(name, now.clone())?;
    }
    let tick = crate::runtime::function(lua, |_, ()| Ok(TICK))?;
    g.set("FrameTime", tick.clone())?;
    g.set("RealFrameTime", tick.clone())?;
    g.set(
        "FrameNumber",
        crate::runtime::function(lua, |lua, ()| Ok(state(lua).turns.get()))?,
    )?;
    let engine: Table = match g.get::<Value>("engine")? {
        Value::Table(t) => t,
        _ => lua.create_table()?,
    };
    engine.set("TickInterval", tick.clone())?;
    engine.set("AbsoluteFrameTime", tick)?;
    engine.set(
        "TickCount",
        crate::runtime::function(lua, |lua, ()| Ok(state(lua).turns.get()))?,
    )?;
    engine.set(
        "ActiveGamemode",
        crate::runtime::function(lua, |lua, ()| Ok(state(lua).tree.gamemode().to_string()))?,
    )?;
    g.set("engine", engine)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_vectors() {
        assert_eq!(crc(b"hello"), "907060870");
        assert_eq!(crc(b"a"), "3904355907");
        assert_eq!(crc(b""), "0");
        assert_eq!(crc(b"123456789"), "3421780262");
        assert_eq!(
            crc(b"The quick brown fox jumps over the lazy dog"),
            "1095738169"
        );
    }

    #[test]
    fn steam_ids() {
        assert_eq!(steamid_to_64("STEAM_0:1:12345"), "76561197960290419");
        assert_eq!(steamid_to_64("STEAM_1:0:0"), "76561197960265728");
        assert_eq!(
            steamid_to_64("STEAM_0:0:18446744073709551615"),
            "76561202255233022"
        );
        assert_eq!(
            steamid_to_64("STEAM_0:1:99999999999999999999999"),
            "76561202255233023"
        );
        assert_eq!(steamid_to_64("STEAM_0:0:2147483647"), "76561202255233022");
        assert_eq!(steamid_to_64("STEAM_0:0:-1"), "0");
        assert_eq!(steamid_to_64("BOT"), "0");
        assert_eq!(steamid_from_64("76561197960290419"), "STEAM_0:1:12345");
        assert_eq!(steamid_from_64("12"), "STEAM_0:0:0");
    }

    #[test]
    fn base64_lines() {
        assert_eq!(
            base64_encode(b"Base64 Encoding", false),
            "QmFzZTY0IEVuY29kaW5n"
        );
        let long = vec![b'x'; 100];
        let s = base64_encode(&long, false);
        let lines: Vec<&str> = s.split('\n').collect();
        assert_eq!(lines[0].len(), 76);
        assert_eq!(lines.len(), 2);
        assert!(!base64_encode(&long, true).contains('\n'));
        assert_eq!(base64_decode(s.as_bytes()).unwrap(), long);
        assert_eq!(
            base64_decode(b"QmFzZTY0IEVuY29kaW5n").unwrap(),
            b"Base64 Encoding"
        );
        assert!(base64_decode(b"!!").is_none());
    }
}
