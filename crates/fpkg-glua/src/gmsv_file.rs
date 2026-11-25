//! The `file` binary module (gmsv_file) as a built-in: the `File` global, with `read`,
//! `write`, `append`, `delete`, `mkdir` and `open`, over paths relative to `garrysmod/`.
//!
//! As the module does: a name that starts with `/` or `~`, or holds `\b`, `\r`, `:`, `\`, `$`,
//! `~`, `%` or `..`, is refused; `write` creates the folders above the file, `append` does
//! not; contents are written and read up to their first NUL byte; `mkdir` of a folder that
//! exists returns false; `open` takes the modes `r`, `w` and `a` with up to two of `b` and
//! `+`, and its handles have `read`, `write` and `close`. Names keep their case, as the
//! module keeps it. A write into the stand-alone tree creates it first.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use nl::mlua::{self, Lua, MultiValue, Table, UserData, Value};

use crate::runtime::{
    arg, as_bytes, bad_argument, bad_value, check_string, function, state, until_nul,
};
use crate::tree::Tree;

/// Whether gmsv_file takes the name.
pub(crate) fn allowed(name: &str) -> bool {
    if name.starts_with('/') || name.starts_with('~') {
        return false;
    }
    if name
        .chars()
        .any(|c| matches!(c, '\u{8}' | '\r' | ':' | '\\' | '$' | '~' | '%'))
    {
        return false;
    }
    !name.contains("..")
}

/// The real path of a name relative to `garrysmod/`, if the module takes it.
pub(crate) fn path(tree: &Tree, name: &str) -> Option<PathBuf> {
    if !allowed(name) {
        return None;
    }
    tree.game_path(name)
}

/// `path` of a name about to be written, with the tree created when it is not there and the
/// listings of folders forgotten.
fn writable(tree: &Tree, name: &str) -> Option<PathBuf> {
    let p = path(tree, name)?;
    tree.prepare_for(&p).ok()?;
    tree.changed();
    Some(p)
}

fn mode_ok(mode: &str) -> bool {
    let b = mode.as_bytes();
    matches!(b.first(), Some(b'r' | b'w' | b'a'))
        && b.len() <= 3
        && b[1..].iter().all(|c| *c == b'b' || *c == b'+')
}

fn contents(lua: &Lua, args: &MultiValue) -> mlua::Result<Vec<u8>> {
    match arg(args, 2) {
        Value::String(s) => Ok(until_nul(&s.as_bytes()).to_vec()),
        _ => Err(bad_argument(lua, args, 2, "string")),
    }
}

/// Opens the module: sets the global `File`.
pub(crate) fn open(lua: &Lua) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "read",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let Some(p) = path(&state(lua).tree, &name) else {
                return Ok(Value::Boolean(false));
            };
            match std::fs::read(p) {
                Ok(b) => Ok(Value::String(lua.create_string(until_nul(&b))?)),
                Err(_) => Ok(Value::Boolean(false)),
            }
        })?,
    )?;
    t.set(
        "write",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let data = contents(lua, &args)?;
            let Some(p) = writable(&state(lua).tree, &name) else {
                return Ok(false);
            };
            if let Some(dir) = p.parent()
                && std::fs::create_dir_all(dir).is_err()
            {
                return Ok(false);
            }
            Ok(std::fs::write(p, data).is_ok())
        })?,
    )?;
    t.set(
        "append",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let data = contents(lua, &args)?;
            let Some(p) = writable(&state(lua).tree, &name) else {
                return Ok(false);
            };
            Ok(OpenOptions::new()
                .append(true)
                .create(true)
                .open(p)
                .and_then(|mut f| f.write_all(&data))
                .is_ok())
        })?,
    )?;
    t.set(
        "delete",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let Some(p) = path(&state(lua).tree, &name) else {
                return Ok(false);
            };
            state(lua).tree.changed();
            Ok(if p.is_dir() {
                std::fs::remove_dir(p).is_ok()
            } else {
                std::fs::remove_file(p).is_ok()
            })
        })?,
    )?;
    t.set(
        "mkdir",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let Some(p) = writable(&state(lua).tree, &name) else {
                return Ok(false);
            };
            if p.exists() {
                return Ok(false);
            }
            Ok(std::fs::create_dir_all(p).is_ok())
        })?,
    )?;
    t.set(
        "open",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            let name = check_string(lua, &args, 1)?;
            let mode = check_string(lua, &args, 2)?;
            if !mode_ok(&mode) {
                return Ok(Value::Boolean(false));
            }
            let p = if mode.starts_with('r') {
                path(&state(lua).tree, &name)
            } else {
                writable(&state(lua).tree, &name)
            };
            let Some(p) = p else {
                return Ok(Value::Boolean(false));
            };
            let plus = mode.contains('+');
            let mut o = OpenOptions::new();
            match mode.as_bytes()[0] {
                b'r' => o.read(true).write(plus),
                b'w' => o.write(true).create(true).truncate(true).read(plus),
                _ => o.append(true).create(true).read(plus),
            };
            match o.open(p) {
                Ok(f) => Ok(Value::UserData(lua.create_userdata(Handle(Some(f)))?)),
                Err(_) => Ok(Value::Boolean(false)),
            }
        })?,
    )?;
    lua.globals().set("File", t)
}

/// A handle of `File.open`.
struct Handle(Option<std::fs::File>);

/// The methods of handles, as engine functions: `read`, `write` and `close`.
fn methods(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "read",
        function(lua, |lua, args: MultiValue| {
            with_handle(lua, &args, |lua, h, _| {
                let Some(f) = h.0.as_mut() else {
                    return Err(crate::runtime::lua_error(
                        lua,
                        "bad argument #1 to 'read' (open file handle expected)",
                    ));
                };
                let pos = f.stream_position().unwrap_or(0);
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_err() {
                    let _ = f.seek(SeekFrom::Start(pos));
                    return Ok(Value::Boolean(false));
                }
                Ok(Value::String(lua.create_string(buf)?))
            })
        })?,
    )?;
    t.set(
        "write",
        function(lua, |lua, args: MultiValue| {
            with_handle(lua, &args, |lua, h, args| {
                let Some(f) = h.0.as_mut() else {
                    return Err(crate::runtime::lua_error(
                        lua,
                        "bad argument #1 to 'write' (open file handle expected)",
                    ));
                };
                let Some(b) = args.get(1).and_then(as_bytes) else {
                    return Err(bad_value(lua, 2, "string", args.get(1)));
                };
                Ok(Value::Boolean(f.write_all(&b).is_ok()))
            })
        })?,
    )?;
    t.set(
        "close",
        function(lua, |lua, args: MultiValue| {
            with_handle(lua, &args, |_, h, _| {
                Ok(Value::Boolean(h.0.take().is_some()))
            })
        })?,
    )?;
    Ok(t)
}

/// Runs a method on the handle that is its first argument.
fn with_handle(
    lua: &Lua,
    args: &MultiValue,
    f: impl FnOnce(&Lua, &mut Handle, &MultiValue) -> mlua::Result<Value>,
) -> mlua::Result<Value> {
    let bad = || bad_value(lua, 1, "FileHandle", args.front());
    let Some(Value::UserData(ud)) = args.front() else {
        return Err(bad());
    };
    let mut h = ud.borrow_mut::<Handle>().map_err(|_| bad())?;
    f(lua, &mut h, args)
}

impl UserData for Handle {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "FileHandle");
        fields.add_meta_field_with("__index", methods);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_module_refuses() {
        for bad in [
            "/etc/x", "~x", "a:b", "a\\b", "a$b", "a~b", "a%b", "../x", "a/../b", "a\rb",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
        for good in [
            "lua/_flux/environment.lua",
            "gamemodes/x/db/migrate/1_a.lua",
            "data/a.txt",
        ] {
            assert!(allowed(good), "{good}");
        }
    }

    #[test]
    fn modes() {
        for m in ["r", "rb", "w+", "ab+", "r+b"] {
            assert!(mode_ok(m), "{m}");
        }
        for m in ["x", "", "rw", "rbbb", "r+x"] {
            assert!(!mode_ok(m), "{m}");
        }
    }
}
