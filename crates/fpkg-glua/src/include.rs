//! Running Lua as the engine does: `include`, `CompileString`, `RunString`, `RunStringEx`,
//! `CompileFile` and `AddCSLuaFile`.
//!
//! `include` looks for its file next to the file that is running (the innermost file being
//! included, not the file that defined the calling function), then from the roots of `LUA`;
//! it takes `.lua` files only. A file that is missing, empty, does not compile or raises an
//! error is reported and `include` returns nothing: the including file goes on, as on the
//! server. Chunks are named after their path relative to `garrysmod/`
//! (`@gamemodes/hl2rp/gamemode/init.lua`).

use nl::mlua::{self, Function, Lua, MultiValue, Value};

use crate::runtime::{
    Failure, arg, as_bytes, as_string, bad_argument, caller_stack, load_chunk, protected_call,
    state, truthy, until_nul,
};

/// The one macro the engine expands in scripts.
const MACRO: &str = "DEFINE_BASECLASS";
const EXPANSION: &str = "local BaseClass = baseclass.Get";

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set("include", crate::runtime::function(lua, include)?)?;
    g.set(
        "AddCSLuaFile",
        crate::runtime::function(lua, |_, _: MultiValue| Ok(()))?,
    )?;
    g.set(
        "CompileString",
        crate::runtime::function(lua, compile_string)?,
    )?;
    g.set(
        "RunString",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            run_string(lua, args, "RunString")
        })?,
    )?;
    g.set(
        "RunStringEx",
        crate::runtime::function(lua, |lua, args: MultiValue| {
            run_string(lua, args, "RunStringEx")
        })?,
    )?;
    g.set("CompileFile", crate::runtime::function(lua, compile_file)?)?;
    Ok(())
}

/// The text the engine runs: up to the first NUL, with the macro expanded.
pub(crate) fn script_text(bytes: &[u8]) -> Vec<u8> {
    let bytes = until_nul(bytes);
    if !contains(bytes, MACRO.as_bytes()) {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len() + 64);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(MACRO.as_bytes()) {
            out.extend_from_slice(EXPANSION.as_bytes());
            i += MACRO.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Where the caller of an engine function is, as the engine writes it in its errors.
pub(crate) fn location(lua: &Lua) -> String {
    let mut at = "<nowhere>".to_string();
    for level in 0..2 {
        if let Some(Some(s)) = lua.inspect_stack(level, |d| {
            let line = d.current_line()?;
            let src = d.source().source?.into_owned();
            Some(format!("{src} (line {line})"))
        }) {
            at = s;
        }
    }
    at
}

fn report_here(lua: &Lua, message: String) {
    state(lua).report(&Failure {
        message,
        stack: caller_stack(lua),
    });
}

fn include(lua: &Lua, args: MultiValue) -> mlua::Result<MultiValue> {
    let Some(path) = args.front().and_then(as_string) else {
        return Err(bad_argument(lua, &args, 1, "string"));
    };
    let path = path.replace('\\', "/");
    if !path.ends_with(".lua") {
        report_here(
            lua,
            format!(
                "Couldn't include file '{path}' - Not a .lua file! ({})",
                location(lua)
            ),
        );
        return Ok(MultiValue::new());
    }
    let st = state(lua);
    let current = st.paths.borrow().last().cloned().flatten();
    let candidates = [current.map(|c| format!("{c}/{path}")), Some(path.clone())];
    for candidate in candidates.into_iter().flatten() {
        let Some(found) = st.tree.find_file(&candidate, "LUA") else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&found.real) else {
            continue;
        };
        let text = script_text(&bytes);
        if text.is_empty() {
            continue;
        }
        let name = found.game.clone().unwrap_or_else(|| candidate.clone());
        return Ok(run_file(lua, &text, &name, &candidate).unwrap_or_default());
    }
    report_here(
        lua,
        format!(
            "Couldn't include file '{path}' - File not found or is empty ({})",
            location(lua)
        ),
    );
    Ok(MultiValue::new())
}

/// The folder of a `LUA` path, as the include path stack holds it.
pub(crate) fn folder_of(lua_path: &str) -> Option<String> {
    let parts = crate::tree::normalize(lua_path)?;
    let dir = parts[..parts.len().saturating_sub(1)].join("/");
    (!dir.is_empty()).then_some(dir)
}

/// Compiles and runs a file's text with its folder on the include path stack. A failure is
/// reported and `None` returned.
pub(crate) fn run_file(
    lua: &Lua,
    text: &[u8],
    chunk_name: &str,
    lua_path: &str,
) -> Option<MultiValue> {
    let st = state(lua);
    let f = match load_chunk(lua, text, chunk_name) {
        Ok(f) => f,
        Err(e) => {
            st.report(&e);
            return None;
        }
    };
    st.paths.borrow_mut().push(folder_of(lua_path));
    let r = protected_call(lua, &f, MultiValue::new());
    st.paths.borrow_mut().pop();
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            st.report(&e);
            None
        }
    }
}

fn too_short(lua: &Lua, name: &str) {
    state(lua).out(format!("Not running script {name} - it's too short.\n").as_bytes());
}

fn compile_string(lua: &Lua, args: MultiValue) -> mlua::Result<Value> {
    let Some(code) = args.front().and_then(as_bytes) else {
        return Err(bad_argument(lua, &args, 1, "string"));
    };
    let name = as_string(&arg(&args, 2)).unwrap_or_else(|| "CompileString".into());
    let handle = !matches!(arg(&args, 3), Value::Boolean(false));
    let text = script_text(&code);
    if text.is_empty() {
        too_short(lua, &name);
        return if handle {
            Ok(Value::Nil)
        } else {
            Ok(Value::String(
                lua.create_string("Invalid script - or too short.")?,
            ))
        };
    }
    match load_chunk(lua, &text, &name) {
        Ok(f) => Ok(Value::Function(f)),
        Err(e) if handle => {
            state(lua).report(&e);
            Ok(Value::Nil)
        }
        Err(e) => Ok(Value::String(lua.create_string(&e.message)?)),
    }
}

fn run_string(lua: &Lua, args: MultiValue, default_name: &str) -> mlua::Result<Value> {
    let Some(code) = args.front().and_then(as_bytes) else {
        return Err(bad_argument(lua, &args, 1, "string"));
    };
    let name = as_string(&arg(&args, 2)).unwrap_or_else(|| default_name.into());
    let handle = !matches!(arg(&args, 3), Value::Boolean(false));
    let text = script_text(&code);
    if text.is_empty() {
        too_short(lua, &name);
        return Ok(Value::Nil);
    }
    let failed = match load_chunk(lua, &text, &name) {
        Ok(f) => protected_call(lua, &f, MultiValue::new()).err(),
        Err(e) => Some(e),
    };
    match failed {
        None => Ok(Value::Nil),
        Some(e) if handle => {
            state(lua).report(&e);
            Ok(Value::Nil)
        }
        Some(e) => Ok(Value::String(lua.create_string(&e.message)?)),
    }
}

fn compile_file(lua: &Lua, args: MultiValue) -> mlua::Result<Option<Function>> {
    let Some(path) = args.front().and_then(as_string) else {
        return Err(bad_argument(lua, &args, 1, "string"));
    };
    let show = args.get(1).is_none_or(truthy);
    let st = state(lua);
    let Some(found) = st.tree.find_file(&path, "LUA") else {
        return Ok(None);
    };
    let text = script_text(&std::fs::read(&found.real).unwrap_or_default());
    let name = found.game.unwrap_or(path);
    match load_chunk(lua, &text, &name) {
        Ok(f) => Ok(Some(f)),
        Err(e) => {
            if show {
                st.report(&e);
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_macro_is_expanded_and_text_ends_at_nul() {
        assert_eq!(
            script_text(b"DEFINE_BASECLASS('x')\nDEFINE_BASECLASS 'y'"),
            b"local BaseClass = baseclass.Get('x')\nlocal BaseClass = baseclass.Get 'y'".to_vec()
        );
        assert_eq!(script_text(b"print(1)\0junk"), b"print(1)".to_vec());
    }

    #[test]
    fn folders_of_lua_paths() {
        assert_eq!(folder_of("hl2rp/lib/x.lua").as_deref(), Some("hl2rp/lib"));
        assert_eq!(folder_of("x.lua"), None);
        assert_eq!(folder_of("a/../b/x.lua").as_deref(), Some("b"));
    }
}
