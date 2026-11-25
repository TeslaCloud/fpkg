//! `require`: the modules loaded at startup, the built-in binary modules, then Lua modules in
//! `includes/modules/` of `LUA`.
//!
//! A module is loaded once; its name is kept in `_MODULES`. The binary modules are built into
//! the runtime: `file` (gmsv_file's `File`), `pg` (gmsv_pg's `pg`) and `mysqloo` (MySQLOO's
//! `mysqloo`); a build without the `postgres` or the `mysql` feature has no `pg` or no `mysqloo`,
//! and `require` says so. A binary module the runtime does not have fails even when the server
//! has its `.dll`.

use std::rc::Rc;

use nl::mlua::{self, Lua, MultiValue, Table, Value};

#[cfg(any(not(feature = "postgres"), not(feature = "mysql")))]
use crate::runtime::Opener;
use crate::runtime::{check_string, lua_error, state, truthy};

/// The modules the game's `includes/init.lua` requires at startup.
pub(crate) const STARTUP: &[&str] = &[
    "baseclass",
    "concommand",
    "saverestore",
    "hook",
    "gamemode",
    "weapons",
    "scripted_ents",
    "player_manager",
    "numpad",
    "team",
    "undo",
    "cleanup",
    "duplicator",
    "constraint",
    "construct",
    "usermessage",
    "list",
    "cvars",
    "http",
    "properties",
    "widgets",
    "cookie",
    "utf8",
    "drive",
    "ai_task",
    "ai_schedule",
];

#[cfg(any(not(feature = "postgres"), not(feature = "mysql")))]
fn unavailable(message: &'static str) -> Opener {
    Rc::new(move |_| Err(mlua::Error::RuntimeError(message.into())))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    {
        let st = state(lua);
        let mut m = st.modules.borrow_mut();
        m.insert("file".into(), Rc::new(crate::gmsv_file::open));
        #[cfg(feature = "postgres")]
        m.insert("pg".into(), Rc::new(crate::pg::open));
        #[cfg(not(feature = "postgres"))]
        m.insert(
            "pg".into(),
            unavailable(
                "this build of fpkg has no PostgreSQL support: the pg module is not built in",
            ),
        );
        #[cfg(feature = "mysql")]
        m.insert("mysqloo".into(), Rc::new(crate::mysqloo::open));
        #[cfg(not(feature = "mysql"))]
        m.insert(
            "mysqloo".into(),
            unavailable(
                "this build of fpkg has no MySQL support: the mysqloo module is not built in",
            ),
        );
    }
    let loaded = lua.create_table()?;
    for name in STARTUP {
        loaded.raw_set(*name, true)?;
    }
    let g = lua.globals();
    g.raw_set("_MODULES", loaded)?;
    g.raw_set("require", crate::runtime::function(lua, require)?)?;
    Ok(())
}

fn require(lua: &Lua, args: MultiValue) -> mlua::Result<()> {
    let name = check_string(lua, &args, 1)?;
    let g = lua.globals();
    let loaded: Table = match g.raw_get::<Value>("_MODULES")? {
        Value::Table(t) => t,
        _ => {
            let t = lua.create_table()?;
            g.raw_set("_MODULES", t.clone())?;
            t
        }
    };
    if truthy(&loaded.raw_get::<Value>(name.as_str())?) {
        return Ok(());
    }
    let st = state(lua);
    let opener = st.modules.borrow().get(&name).cloned();
    if let Some(open) = opener {
        open(lua)?;
        loaded.raw_set(name.as_str(), true)?;
        return Ok(());
    }
    let lua_path = format!("includes/modules/{name}.lua");
    if let Some(found) = st.tree.find_file(&lua_path, "LUA") {
        let text = crate::include::script_text(&std::fs::read(&found.real).unwrap_or_default());
        if !text.is_empty() {
            let chunk = found.game.unwrap_or_else(|| lua_path.clone());
            if crate::include::run_file(lua, &text, &chunk, &lua_path).is_some() {
                loaded.raw_set(name.as_str(), true)?;
            }
            return Ok(());
        }
    }
    let binary = ["linux64", "linux", "win64", "win32", "osx64", "osx"]
        .iter()
        .any(|p| {
            st.tree
                .exists(format!("lua/bin/gmsv_{name}_{p}.dll"), "MOD")
        });
    if binary {
        return Err(lua_error(
            lua,
            format!(
                "Couldn't load module library! (the binary module '{name}' is not built into fpkg's runtime)"
            ),
        ));
    }
    Err(lua_error(lua, "Module not found!"))
}
