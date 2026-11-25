//! Compares the runtime's stock libraries with the game's own Lua on generated inputs and
//! scripted sequences: `string`, `table` (with `SortedPairs*`), `math`, `math.ease`, `utf8`,
//! colors, the helpers of `util` and the global ones, `hook`, `concommand`, `cvars`,
//! `baseclass`, `weapons` and `scripted_ents`. Ignored unless run by hand with `FPKG_GMOD_LUA`
//! set to the game's `garrysmod/lua` folder:
//!
//! ```text
//! FPKG_GMOD_LUA=~/.local/share/Steam/steamapps/common/GarrysMod/garrysmod/lua \
//!     cargo test -p fpkg-glua --test gmod_compare -- --ignored --nocapture
//! ```
//!
//! The game's files (`GAME_FILES`) are run on top of a second runtime, so both states share
//! everything else (the engine functions the game implements in C are the runtime's in both);
//! the same driver then prints one line per call in each, and the lines must match.
//! `FPKG_COMPARE_OUT=<file>` also writes every difference there, and the game's lines to
//! `<file>.game`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use fpkg_glua::{Captured, Config, Output, Runtime, Tree};

const DRIVER: &str = include_str!("gmod_compare.lua");

/// Stand-ins, the same in both states, for what the runtime does not implement: the engine's
/// vectors and angles (the game's files make some at load, and the curve functions take
/// them), and `list.Set`, `duplicator.Allow` and `ents.Iterator`, which the game's registries
/// call; and `ErrorNoHalt` and `ErrorNoHaltWithStack`, which hand what is reported to the
/// driver's `REPORTED` while it is set.
const PRELUDE: &str = r#"
local function class(name, a, b, c)
  local meta = FindMetaTable(name)

  local function new(x, y, z)
    return setmetatable({ [a] = x or 0, [b] = y or 0, [c] = z or 0 }, meta)
  end

  meta.__add = function(u, v) return new(u[a] + v[a], u[b] + v[b], u[c] + v[c]) end
  meta.__sub = function(u, v) return new(u[a] - v[a], u[b] - v[b], u[c] - v[c]) end
  meta.__unm = function(u) return new(-u[a], -u[b], -u[c]) end
  meta.__mul = function(u, v)
    if type(u) == 'number' then u, v = v, u end
    if type(v) == 'number' then return new(u[a] * v, u[b] * v, u[c] * v) end
    return new(u[a] * v[a], u[b] * v[b], u[c] * v[c])
  end
  meta.__div = function(u, v) return new(u[a] / v, u[b] / v, u[c] / v) end
  meta.__eq = function(u, v) return u[a] == v[a] and u[b] == v[b] and u[c] == v[c] end
  meta.__tostring = function(u) return string.format('%.17g %.17g %.17g', u[a], u[b], u[c]) end
  meta.__index = meta
  meta.Add = function(u, v) u[a], u[b], u[c] = u[a] + v[a], u[b] + v[b], u[c] + v[c] end
  meta.Sub = function(u, v) u[a], u[b], u[c] = u[a] - v[a], u[b] - v[b], u[c] - v[c] end
  meta.Mul = function(u, n) u[a], u[b], u[c] = u[a] * n, u[b] * n, u[c] * n end
  meta.Div = function(u, n) u[a], u[b], u[c] = u[a] / n, u[b] / n, u[c] / n end

  return new
end

Vector = class('Vector', 'x', 'y', 'z')
Angle = class('Angle', 'p', 'y', 'r')

do
  local plain, stacked = ErrorNoHalt, ErrorNoHaltWithStack

  ErrorNoHalt = function(...)
    if REPORTED then return REPORTED(...) end

    return plain(...)
  end

  ErrorNoHaltWithStack = function(...)
    if REPORTED then return REPORTED(...) end

    return stacked(...)
  end
end

list.Set = function() end
duplicator.Allow = function() end
ents.Iterator = function() return function() end, {}, 0 end
"#;

/// The game's files that replace the runtime's in the second state, in the game's order.
const GAME_FILES: &[&str] = &[
    "includes/util.lua",
    "includes/util/color.lua",
    "includes/extensions/string.lua",
    "includes/extensions/table.lua",
    "includes/extensions/math.lua",
    "includes/extensions/util.lua",
    "includes/modules/utf8.lua",
    "includes/modules/hook.lua",
    "includes/modules/concommand.lua",
    "includes/modules/cvars.lua",
    "includes/modules/baseclass.lua",
    "includes/modules/weapons.lua",
    "includes/modules/scripted_ents.lua",
];

/// Functions that must come from the game's files in the second state and from the
/// runtime's in the first.
const CHECKED: &[&str] = &[
    "string.Explode",
    "table.Copy",
    "SortedPairs",
    "math.Round",
    "utf8.len",
    "Color",
    "HSVToColor",
    "tobool",
    "util.Stack",
    "hook.Add",
    "hook.Call",
    "concommand.Add",
    "concommand.Run",
    "cvars.AddChangeCallback",
    "cvars.Bool",
    "baseclass.Get",
    "weapons.Register",
    "scripted_ents.Get",
];

/// The names of the libraries' functions and values, sorted: what one state defines and the
/// other does not is printed (the runtime leaves some to `missing.lua` on purpose).
const NAMES: &str = r#"
local out = {}
local function list(prefix, t)
  for k, v in pairs(t) do
    if type(k) == 'string' and (type(v) == 'function' or type(v) == 'number' or type(v) == 'string' or type(v) == 'boolean' or IsColor(v)) then
      out[#out + 1] = prefix..k
    end
  end
end
list('', _G)
for _, name in ipairs({ 'string', 'table', 'math', 'util', 'hook', 'concommand', 'cvars', 'utf8', 'debug' }) do
  list(name..'.', _G[name])
end
list('math.ease.', math.ease)
list('Color:', FindMetaTable('Color'))
table.sort(out)
return table.concat(out, ' ')
"#;

fn runtime(tree: Tree) -> (Runtime, Rc<RefCell<Captured>>) {
    let (output, console) = Output::capture();
    let rt = Runtime::new(Config {
        tree,
        output,
        color: false,
    })
    .unwrap();
    (rt, console)
}

fn run(rt: &Runtime, code: &str) -> String {
    let f = rt.load(code.as_bytes(), "check.lua").unwrap();
    let v = rt
        .call(&f, ())
        .unwrap_or_else(|e| panic!("{e}\n{}", e.traceback()));
    rt.lua()
        .unpack::<Option<String>>(v.into_iter().next().unwrap_or(nl::mlua::Value::Nil))
        .unwrap()
        .unwrap_or_default()
}

/// The driver's lines and its count of calls by library.
fn run_driver(rt: &Runtime) -> (Vec<String>, String) {
    let f = rt.load(DRIVER.as_bytes(), "compare.lua").unwrap();
    let out = rt
        .call(&f, ())
        .unwrap_or_else(|e| panic!("{e}\n{}", e.traceback()));
    let mut values = out.into_iter();
    let text: String = rt.lua().unpack(values.next().unwrap()).unwrap();
    let summary: String = rt.lua().unpack(values.next().unwrap()).unwrap();
    (text.lines().map(str::to_string).collect(), summary)
}

#[test]
#[ignore = "needs the game's Lua: set FPKG_GMOD_LUA"]
fn the_stock_libraries_match_the_game_s() {
    let lua_dir = PathBuf::from(std::env::var("FPKG_GMOD_LUA").expect("FPKG_GMOD_LUA"));
    let garrysmod = lua_dir.parent().unwrap();
    let server_root = garrysmod.parent().unwrap();
    let tree = || Tree::server(server_root, "sandbox", &garrysmod.join("gamemodes/sandbox"));
    let (ours, _) = runtime(tree());
    let (game, console) = runtime(tree());
    run(&ours, PRELUDE);
    run(&game, PRELUDE);
    run(&game, "function AddConsoleCommand() end");
    for file in GAME_FILES {
        run(&game, &format!("include('{file}')"));
        assert_eq!(
            game.reported_errors(),
            0,
            "{file} did not load:\n{}",
            console.borrow().err_text()
        );
    }
    let sources = format!(
        "local out = {{}} for _, f in ipairs({{ {} }}) do out[#out + 1] = debug.getinfo(f, 'S').source end return table.concat(out, ' ')",
        CHECKED.join(", ")
    );
    for (rt, prefix) in [(&ours, "=fpkg/"), (&game, "@lua/includes/")] {
        let found = run(rt, &sources);
        assert!(found.split(' ').all(|s| s.starts_with(prefix)), "{found}");
    }
    let our_names: std::collections::BTreeSet<String> =
        run(&ours, NAMES).split(' ').map(str::to_string).collect();
    let game_names: std::collections::BTreeSet<String> =
        run(&game, NAMES).split(' ').map(str::to_string).collect();
    println!(
        "only in the game's state: {:?}",
        game_names.difference(&our_names).collect::<Vec<_>>()
    );
    println!(
        "only in the runtime's: {:?}",
        our_names.difference(&game_names).collect::<Vec<_>>()
    );
    let (a, summary) = run_driver(&ours);
    let (b, _) = run_driver(&game);
    assert_eq!(
        a.len(),
        b.len(),
        "the drivers made different numbers of calls"
    );
    let differences: Vec<String> = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| x != y)
        .map(|(x, y)| format!("fpkg: {x}\ngame: {y}"))
        .collect();
    println!("{} calls compared, {} differ", a.len(), differences.len());
    println!("calls by library: {summary}");
    if let Ok(path) = std::env::var("FPKG_COMPARE_OUT") {
        std::fs::write(&path, differences.join("\n\n")).unwrap();
        std::fs::write(format!("{path}.game"), b.join("\n")).unwrap();
    }
    let mut shown = std::collections::BTreeSet::new();
    for d in &differences {
        let name = d[6..].split('(').next().unwrap_or("").to_string();
        if shown.insert(name) {
            println!("{d}\n");
        }
    }
    assert!(differences.is_empty(), "{} differences", differences.len());
}
