//! The engine's `file` library over the virtual tree, and its file objects.
//!
//! Reads go through any search path and find names whatever their case (see `Tree`). Writes
//! (`file.Write`, `file.Append`, `file.Open` in a write mode, `file.CreateDir`,
//! `file.Delete`, `file.Rename`) are confined to `DATA`, as in the game: the name is
//! lower-cased, ASCII letters only (the game writes `A.txt` and `Zdir` as `a.txt` and
//! `zdir`), must be ASCII (the game writes nothing for another name), must end in one of the
//! extensions the game allows (for files) and must not hold `"`, `:` or two spaces in a row.
//! Names are bytes throughout. A write into the stand-alone tree creates it first.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use nl::mlua::{self, Lua, MultiValue, Table, UserData, UserDataMethods, Value};

use crate::runtime::{
    arg, as_bytes, as_number, as_string, bad_argument, bad_value, check_bytes, check_string,
    function, state, truthy,
};
use crate::tree::{Sort, Tree, normalize_bytes};

/// The extensions a file written to `DATA` may have.
pub(crate) const DATA_EXTENSIONS: &[&str] = &[
    "txt", "dat", "json", "xml", "csv", "dem", "vcd", "gma", "mdl", "phy", "vvd", "vtx", "ani",
    "vtf", "vmt", "png", "jpg", "jpeg", "mp3", "wav", "ogg",
];

/// The real path of a name in `DATA` that may be written, lower-cased; `None` when the game
/// refuses the name. `file` is false for folders, which need no extension. Nothing is
/// created.
pub(crate) fn data_path(tree: &Tree, name: &[u8], file: bool) -> Option<PathBuf> {
    if !name.is_ascii() {
        return None;
    }
    let name = name.to_ascii_lowercase();
    if name.contains(&b'"') || name.contains(&b':') || name.windows(2).any(|w| w == b"  ") {
        return None;
    }
    let parts = normalize_bytes(&name)?;
    let last = parts.last()?;
    if file {
        let dot = last.iter().rposition(|b| *b == b'.')?;
        let ext = &last[dot + 1..];
        if !DATA_EXTENSIONS.iter().any(|e| e.as_bytes() == ext) {
            return None;
        }
    }
    let mut rel = b"data/".to_vec();
    rel.extend(parts.join(&b'/'));
    tree.game_path(rel)
}

/// `data_path` of a name about to be written, with the tree created when it is not there and
/// the listings of folders forgotten.
fn writable(tree: &Tree, name: &[u8], file: bool) -> Option<PathBuf> {
    let p = data_path(tree, name, file)?;
    tree.prepare_for(&p).ok()?;
    tree.changed();
    Some(p)
}

/// Lua strings of names.
fn names(lua: &Lua, names: Vec<Vec<u8>>) -> mlua::Result<Table> {
    lua.create_sequence_from(
        names
            .into_iter()
            .map(|n| lua.create_string(n))
            .collect::<mlua::Result<Vec<_>>>()?,
    )
}

fn path_id(v: &Value, default: &str) -> String {
    match v {
        Value::Boolean(true) => "GAME".into(),
        v => as_string(v).unwrap_or_else(|| default.into()),
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let file = lua.create_table()?;
    file.set(
        "Exists",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            Ok(state(lua).tree.exists(&name, &id))
        })?,
    )?;
    file.set(
        "IsDir",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            Ok(state(lua).tree.is_dir(&name, &id))
        })?,
    )?;
    file.set(
        "Size",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            Ok(state(lua).tree.size(&name, &id))
        })?,
    )?;
    file.set(
        "Time",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            Ok(state(lua).tree.time(&name, &id))
        })?,
    )?;
    file.set(
        "Find",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let pattern = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            let sort = Sort::parse(as_string(&arg(&args, 3)).as_deref());
            match state(lua).tree.find(&pattern, &id, sort) {
                Some((files, dirs)) => Ok((
                    Value::Table(names(lua, files)?),
                    Value::Table(names(lua, dirs)?),
                )),
                None => Ok((Value::Nil, Value::Nil)),
            }
        })?,
    )?;
    file.set(
        "Read",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = path_id(&arg(&args, 2), "DATA");
            let st = state(lua);
            let Some(found) = st.tree.find_file(&name, &id) else {
                return Ok(Value::Nil);
            };
            match std::fs::read(found.real) {
                Ok(b) => Ok(Value::String(lua.create_string(b)?)),
                Err(_) => Ok(Value::Nil),
            }
        })?,
    )?;
    file.set(
        "Write",
        crate::runtime::function(lua, |lua, args: MultiValue| write_data(lua, &args, false))?,
    )?;
    file.set(
        "Append",
        crate::runtime::function(lua, |lua, args: MultiValue| write_data(lua, &args, true))?,
    )?;
    file.set(
        "CreateDir",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            if let Some(p) = writable(&state(lua).tree, &name, false) {
                let _ = std::fs::create_dir_all(p);
            }
            Ok(())
        })?,
    )?;
    file.set(
        "Delete",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let st = state(lua);
            let Some(p) = data_path(&st.tree, &name, false) else {
                return Ok(false);
            };
            let p = st.tree.find_any(&name, "DATA").map_or(p, |f| f.real);
            st.tree.changed();
            Ok(if p.is_dir() {
                std::fs::remove_dir(p).is_ok()
            } else {
                std::fs::remove_file(p).is_ok()
            })
        })?,
    )?;
    file.set(
        "Rename",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let from = check_bytes(lua, &args, 1)?;
            let to = check_bytes(lua, &args, 2)?;
            let st = state(lua);
            let source = data_path(&st.tree, &from, true)
                .and_then(|_| st.tree.find_file(&from, "DATA"))
                .map(|f| f.real);
            let taken = st.tree.exists(&to, "DATA");
            match (source, writable(&st.tree, &to, true)) {
                (Some(a), Some(b)) if !taken => Ok(std::fs::rename(a, b).is_ok()),
                _ => Ok(false),
            }
        })?,
    )?;
    file.set("Open", crate::runtime::function(lua, open)?)?;
    file.set(
        "AsyncRead",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_bytes(lua, &args, 1)?;
            let id = check_string(lua, &args, 2)?;
            let cb = match arg(&args, 3) {
                Value::Function(f) => f,
                _ => return Err(bad_argument(lua, &args, 3, "function")),
            };
            let sync = truthy(&arg(&args, 4));
            let st = state(lua);
            let data = st
                .tree
                .find_file(&name, &id)
                .and_then(|f| std::fs::read(f.real).ok());
            let status = if data.is_some() { 0 } else { -1 };
            let call = move |lua: &Lua| -> mlua::Result<()> {
                let data = match &data {
                    Some(d) => Value::String(lua.create_string(d)?),
                    None => Value::Nil,
                };
                let r = crate::runtime::protected_call(
                    lua,
                    &cb,
                    MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&name)?),
                        Value::String(lua.create_string(&id)?),
                        Value::Integer(status),
                        data,
                    ]),
                );
                if let Err(f) = r {
                    state(lua).report(&f);
                }
                Ok(())
            };
            if sync {
                call(lua)?;
            } else {
                let key =
                    lua.create_registry_value(lua.create_function(move |lua, ()| call(lua))?)?;
                st.timers.borrow_mut().defer(key);
            }
            Ok(0)
        })?,
    )?;
    lua.globals().set("file", file)?;
    Ok(())
}

fn write_data(lua: &Lua, args: &MultiValue, append: bool) -> mlua::Result<bool> {
    let name = check_bytes(lua, args, 1)?;
    let Some(bytes) = args.get(1).and_then(as_bytes) else {
        return Err(bad_argument(lua, args, 2, "string"));
    };
    let Some(p) = writable(&state(lua).tree, &name, true) else {
        return Ok(false);
    };
    let mut o = OpenOptions::new();
    if append {
        o.append(true).create(true);
    } else {
        o.write(true).create(true).truncate(true);
    }
    Ok(o.open(p).and_then(|mut f| f.write_all(&bytes)).is_ok())
}

fn open(lua: &Lua, args: MultiValue) -> mlua::Result<Value> {
    let name = check_bytes(lua, &args, 1)?;
    let mode = check_string(lua, &args, 2)?;
    let id = path_id(&arg(&args, 3), "GAME");
    let st = state(lua);
    let lower = name.to_ascii_lowercase();
    let refused_ext = [&b".db"[..], b".mdmp", b".dmp"]
        .iter()
        .any(|e| lower.ends_with(e));
    let last = lower
        .rsplit(|b| *b == b'/' || *b == b'\\')
        .next()
        .unwrap_or_default();
    let refused_name = [
        &b"server.cfg"[..],
        b"autoexec.cfg",
        b"config.cfg",
        b"listenserver.cfg",
        b"mount.cfg",
    ]
    .contains(&last);
    if refused_ext || refused_name {
        return Ok(Value::Nil);
    }
    let write = mode.starts_with('w') || mode.starts_with('a');
    let handle = if write {
        if !id.eq_ignore_ascii_case("DATA") {
            return Ok(Value::Nil);
        }
        let Some(p) = writable(&st.tree, &name, true) else {
            return Ok(Value::Nil);
        };
        let mut o = OpenOptions::new();
        if mode.starts_with('a') {
            o.append(true).create(true);
        } else {
            o.write(true).create(true).truncate(true);
        }
        o.open(p).ok()
    } else if mode.starts_with('r') {
        st.tree
            .find_file(&name, &id)
            .and_then(|f| std::fs::File::open(f.real).ok())
    } else {
        None
    };
    match handle {
        Some(f) => Ok(Value::UserData(lua.create_userdata(FileObject(Some(f)))?)),
        None => Ok(Value::Nil),
    }
}

/// A file opened with `file.Open`.
struct FileObject(Option<std::fs::File>);

impl FileObject {
    fn read_exact_n(&mut self, n: usize) -> Option<Vec<u8>> {
        let f = self.0.as_mut()?;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn write(&mut self, b: &[u8]) -> bool {
        self.0.as_mut().is_some_and(|f| f.write_all(b).is_ok())
    }

    fn tell(&mut self) -> u64 {
        self.0
            .as_mut()
            .and_then(|f| f.stream_position().ok())
            .unwrap_or(0)
    }

    fn size(&mut self) -> u64 {
        self.0
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len())
    }
}

/// The number argument of a method (the first after the object), or the game's error.
fn number_arg(lua: &Lua, args: &MultiValue) -> mlua::Result<f64> {
    args.front()
        .and_then(as_number)
        .ok_or_else(|| bad_value(lua, 2, "number", args.front()))
}

fn number(n: Option<f64>) -> Value {
    n.map_or(Value::Nil, Value::Number)
}

/// A method of File objects: the object, then the other arguments.
type Method = fn(&Lua, &mut FileObject, &MultiValue) -> mlua::Result<Value>;

fn read_le<const N: usize>(this: &mut FileObject) -> Option<[u8; N]> {
    this.read_exact_n(N).and_then(|b| b.try_into().ok())
}

/// The methods of File objects, as engine functions.
fn methods(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let add = |name: &str, f: Method| -> mlua::Result<()> {
        t.set(
            name,
            function(lua, move |lua, mut args: MultiValue| {
                let this = args.pop_front();
                let bad = || bad_value(lua, 1, "File", this.as_ref());
                let Some(Value::UserData(ud)) = &this else {
                    return Err(bad());
                };
                let mut obj = ud.borrow_mut::<FileObject>().map_err(|_| bad())?;
                f(lua, &mut obj, &args)
            })?,
        )
    };
    add("Close", |_, this, _| {
        this.0 = None;
        Ok(Value::Nil)
    })?;
    add("Flush", |_, this, _| {
        if let Some(f) = this.0.as_mut() {
            let _ = f.flush();
        }
        Ok(Value::Nil)
    })?;
    add("Size", |_, this, _| Ok(Value::Number(this.size() as f64)))?;
    add("Tell", |_, this, _| Ok(Value::Number(this.tell() as f64)))?;
    add("EndOfFile", |_, this, _| {
        let size = this.size();
        Ok(Value::Boolean(this.tell() >= size))
    })?;
    add("Seek", |lua, this, args| {
        let pos = number_arg(lua, args)?;
        if let Some(f) = this.0.as_mut() {
            let _ = f.seek(SeekFrom::Start(pos.max(0.0) as u64));
        }
        Ok(Value::Nil)
    })?;
    add("Skip", |lua, this, args| {
        let n = number_arg(lua, args)?;
        if let Some(f) = this.0.as_mut() {
            let _ = f.seek(SeekFrom::Current(n as i64));
        }
        Ok(Value::Number(this.tell() as f64))
    })?;
    add("Read", |lua, this, args| {
        let n = match args.front() {
            None | Some(Value::Nil) => None,
            Some(_) => Some(number_arg(lua, args)?),
        };
        let left = this.size().saturating_sub(this.tell()) as usize;
        let n = n.map_or(left, |n| (n.max(0.0) as usize).min(left));
        if n == 0 {
            return Ok(Value::Nil);
        }
        match this.read_exact_n(n) {
            Some(b) => Ok(Value::String(lua.create_string(b)?)),
            None => Ok(Value::Nil),
        }
    })?;
    add("ReadLine", |lua, this, _| {
        let Some(f) = this.0.as_mut() else {
            return Ok(Value::Nil);
        };
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while let Ok(1) = f.read(&mut byte) {
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        if line.is_empty() {
            return Ok(Value::Nil);
        }
        Ok(Value::String(lua.create_string(line)?))
    })?;
    add("ReadByte", |_, this, _| {
        Ok(number(read_le::<1>(this).map(|b| f64::from(b[0]))))
    })?;
    add("ReadBool", |_, this, _| {
        Ok(read_le::<1>(this).map_or(Value::Nil, |b| Value::Boolean(b[0] != 0)))
    })?;
    add("ReadShort", |_, this, _| {
        Ok(number(
            read_le(this).map(|b| f64::from(i16::from_le_bytes(b))),
        ))
    })?;
    add("ReadUShort", |_, this, _| {
        Ok(number(
            read_le(this).map(|b| f64::from(u16::from_le_bytes(b))),
        ))
    })?;
    add("ReadLong", |_, this, _| {
        Ok(number(
            read_le(this).map(|b| f64::from(i32::from_le_bytes(b))),
        ))
    })?;
    add("ReadULong", |_, this, _| {
        Ok(number(
            read_le(this).map(|b| f64::from(u32::from_le_bytes(b))),
        ))
    })?;
    add("ReadFloat", |_, this, _| {
        Ok(number(
            read_le(this).map(|b| f64::from(f32::from_le_bytes(b))),
        ))
    })?;
    add("ReadDouble", |_, this, _| {
        Ok(number(read_le(this).map(f64::from_le_bytes)))
    })?;
    add("Write", |lua, this, args| {
        match args.front().and_then(as_bytes) {
            Some(b) => {
                this.write(&b);
                Ok(Value::Nil)
            }
            None => Err(bad_value(lua, 2, "string", args.front())),
        }
    })?;
    add("WriteByte", |lua, this, args| {
        this.write(&[number_arg(lua, args)? as u8]);
        Ok(Value::Nil)
    })?;
    add("WriteBool", |_, this, args| {
        this.write(&[u8::from(args.front().is_some_and(truthy))]);
        Ok(Value::Nil)
    })?;
    add("WriteShort", |lua, this, args| {
        this.write(&(number_arg(lua, args)? as i16).to_le_bytes());
        Ok(Value::Nil)
    })?;
    add("WriteUShort", |lua, this, args| {
        this.write(&(number_arg(lua, args)? as u16).to_le_bytes());
        Ok(Value::Nil)
    })?;
    add("WriteLong", |lua, this, args| {
        this.write(&(number_arg(lua, args)? as i32).to_le_bytes());
        Ok(Value::Nil)
    })?;
    add("WriteULong", |lua, this, args| {
        this.write(&(number_arg(lua, args)? as u32).to_le_bytes());
        Ok(Value::Nil)
    })?;
    add("WriteFloat", |lua, this, args| {
        this.write(&(number_arg(lua, args)? as f32).to_le_bytes());
        Ok(Value::Nil)
    })?;
    add("WriteDouble", |lua, this, args| {
        this.write(&number_arg(lua, args)?.to_le_bytes());
        Ok(Value::Nil)
    })?;
    Ok(t)
}

impl UserData for FileObject {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "File");
        fields.add_meta_field("MetaID", 34);
        fields.add_meta_field_with("__index", methods);
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method("__tostring", |_, _, ()| Ok("File"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_names_follow_the_game_s_rules() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "gm");
        let data = dir.path().join(".fpkg/garrysmod/data");
        assert_eq!(
            data_path(&tree, b"A/B.TXT", true).unwrap(),
            data.join("a/b.txt")
        );
        assert!(data_path(&tree, b"x.lua", true).is_none());
        assert!(data_path(&tree, b"x", true).is_none());
        assert!(data_path(&tree, b"a:b.txt", true).is_none());
        assert!(data_path(&tree, b"a  b.txt", true).is_none());
        assert!(data_path(&tree, b"../x.txt", true).is_none());
        assert!(data_path(&tree, b"a/../../x.txt", true).is_none());
        assert_eq!(data_path(&tree, b"dir", false).unwrap(), data.join("dir"));
    }
}
