//! The stock Lua of the runtime, embedded: each file runs once at startup, in the order the
//! game's `includes/init.lua` loads what it stands for, with the table of the runtime's
//! internals as its argument; `missing.lua` runs last, with the list of the server API.

use nl::mlua::{self, Lua, Table};

/// The files, in load order, with the names their chunks get.
const FILES: &[(&str, &str)] = &[
    ("base.lua", include_str!("lua/base.lua")),
    ("util.lua", include_str!("lua/util.lua")),
    ("color.lua", include_str!("lua/color.lua")),
    ("sql.lua", include_str!("lua/sql.lua")),
    ("hook.lua", include_str!("lua/hook.lua")),
    ("concommand.lua", include_str!("lua/concommand.lua")),
    ("convars.lua", include_str!("lua/convars.lua")),
    ("registries.lua", include_str!("lua/registries.lua")),
    ("utf8.lua", include_str!("lua/utf8.lua")),
    ("string.lua", include_str!("lua/string.lua")),
    ("table.lua", include_str!("lua/table.lua")),
    ("math.lua", include_str!("lua/math.lua")),
    ("engine.lua", include_str!("lua/engine.lua")),
];

const API: &str = include_str!("lua/api.lua");
const MISSING: &str = include_str!("lua/missing.lua");

fn chunk(lua: &Lua, name: &str, source: &str) -> mlua::Result<mlua::Function> {
    lua.load(source)
        .set_name(format!("=fpkg/{name}"))
        .set_mode(mlua::chunk::ChunkMode::Text)
        .into_function()
}

pub(crate) fn load(lua: &Lua, host: &Table) -> mlua::Result<()> {
    for (name, source) in FILES {
        chunk(lua, name, source)?.call::<()>(host.clone())?;
    }
    let api: Table = chunk(lua, "api.lua", API)?.call(())?;
    chunk(lua, "missing.lua", MISSING)?.call::<()>(api)
}
