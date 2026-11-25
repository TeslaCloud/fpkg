//! A Garry's Mod server-side Lua environment without the game.
//!
//! [`Runtime`] is one Lua state on the VM of the game's server (the NL fork of LuaJIT, with
//! the GLua syntax on), with what the engine and the game's stock Lua give a server: the
//! engine functions (`include`, `file`, `util`, `timer`, `sql` on SQLite, ...) in Rust, the
//! stock libraries (`string`, `table` and `math` extensions, `hook`, `concommand`, convars,
//! `utf8`, `Color`, ...) written for fpkg and embedded, the built-in binary modules
//! `require` finds (`file`), and the game's paths mapped onto disk ([`Tree`]). A name of the
//! server API it does not implement fails loudly when called.
//!
//! The runtime knows nothing about tasks; fpkg builds them on it. Nothing runs between calls:
//! timers and `Think` advance only in [`Runtime::pump`], [`Runtime::settle`] and
//! [`Runtime::wait_until`].

mod files;
mod gmsv_file;
mod include;
mod json;
mod keyvalues;
mod modules;
mod output;
mod runtime;
mod sql;
mod stock;
mod timer;
pub mod tree;
mod util;

pub use output::{Captured, Output, Stream, strip_escapes};
pub use runtime::{Config, Failure, Frame, Opener, Poster, Runtime, check_syntax};
pub use tree::{Layout, Sort, Tree};

/// What can go wrong before any Lua runs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the Lua runtime could not start: {0}")]
    Start(String),
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests;
