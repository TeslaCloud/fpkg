//! What a `DatabaseConnection` and its queries share (gmsv_pg's `Session`): the connection,
//! whether it was closed with `disconnect`, and the type categories read when it was opened.
//!
//! Before the first `connect` that works there is no connection, and everything but the
//! listener methods says so. A connection that the server ended stays as it was for escaping
//! and is not opened again: what needs the server fails as lost, since this module does not
//! reconnect. One closed with `disconnect` is gone until the next `connect`.

use std::collections::HashMap;
use std::rc::Rc;

use super::conninfo;
use super::text::{self, Encoding};
use super::wire::{Done, Link, Statement};

pub(crate) const NO_CONNECTION: &str = "pg - no connection, connect to a database first.";
pub(crate) const CLOSED: &str = "pg - connection is closed, connect to the database again.";
pub(crate) const NO_RECONNECT: &str =
    "fpkg's pg module does not implement reconnecting a lost connection";

/// Why there is no connection to use.
enum Unready {
    Closed,
    Lost,
}

impl Unready {
    fn message(&self) -> &'static str {
        match self {
            Unready::Closed => CLOSED,
            Unready::Lost => NO_RECONNECT,
        }
    }
}

#[derive(Default)]
pub(crate) struct Session {
    link: Option<Link>,
    /// Whether a `connect` has worked.
    connected: bool,
    /// The encoding of the connection as of the last time it was used.
    encoding: Option<Encoding>,
    /// The category (`typcategory`) of each type by its OID.
    pub(crate) types: Rc<HashMap<u32, u8>>,
}

impl Session {
    /// `Peek`: an error unless a `connect` has worked.
    pub(crate) fn peek(&self) -> Result<(), &'static str> {
        if self.connected {
            Ok(())
        } else {
            Err(NO_CONNECTION)
        }
    }

    /// The connection, ready for a statement.
    fn ready(&mut self) -> Result<&mut Link, Unready> {
        match self.link.as_mut() {
            None => Err(Unready::Closed),
            Some(link) if link.open => Ok(link),
            Some(_) => Err(Unready::Lost),
        }
    }

    fn note_encoding(&mut self) {
        if let Some(link) = &self.link {
            self.encoding = Some(link.encoding);
        }
    }

    /// The encoding that well formed text is judged by.
    pub(crate) fn encoding(&self) -> Encoding {
        self.encoding.unwrap_or(Encoding::Single)
    }

    /// Connects with the connection string, replacing the connection there is. If that
    /// fails, the connection stays as it was.
    pub(crate) fn connect(&mut self, conninfo: &[u8]) -> Result<(), String> {
        let options = conninfo::parse(conninfo)?;
        let mut link = super::wire::connect(&options)?;
        let set = link
            .exec(b"select oid, typcategory from pg_type")
            .map_err(|e| String::from_utf8_lossy(&e).into_owned())?;
        let mut types = HashMap::with_capacity(set.rows.len());
        for row in &set.rows {
            let fields = set.fields(row);
            let oid = fields
                .first()
                .copied()
                .flatten()
                .and_then(|o| std::str::from_utf8(o).ok())
                .and_then(|o| o.parse::<u32>().ok());
            let category = fields.get(1).copied().flatten().and_then(|c| c.first());
            if let (Some(oid), Some(&category)) = (oid, category) {
                types.insert(oid, category);
            }
        }
        self.link = Some(link);
        self.connected = true;
        self.types = Rc::new(types);
        self.note_encoding();
        Ok(())
    }

    /// Closes the connection for good.
    pub(crate) fn disconnect(&mut self) {
        self.link = None;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.open)
    }

    pub(crate) fn server_version(&self) -> i32 {
        match &self.link {
            Some(l) if l.open => l.server_version,
            _ => 0,
        }
    }

    pub(crate) fn protocol_version(&self) -> i32 {
        if self.is_open() { 3 } else { 0 }
    }

    /// Runs a statement of a query (`Execute`).
    pub(crate) fn execute(&mut self, statement: &Statement) -> Done {
        let done = match self.ready() {
            Ok(link) => link.run(statement),
            Err(why) => Done {
                error: why.message().as_bytes().to_vec(),
                lost: matches!(why, Unready::Lost),
                ..Done::default()
            },
        };
        self.note_encoding();
        done
    }

    pub(crate) fn prepare(&mut self, name: &[u8], definition: &[u8]) -> Result<(), Vec<u8>> {
        if name.is_empty() {
            return Err(b"pg - prepared query name is empty".to_vec());
        }
        let link = self.ready().map_err(|e| e.message().as_bytes().to_vec())?;
        link.prepare(name, definition)
    }

    pub(crate) fn unprepare(&mut self, name: &[u8]) -> Result<(), Vec<u8>> {
        let link = self.ready().map_err(|e| e.message().as_bytes().to_vec())?;
        let quoted = text::escape_identifier(name, link.encoding).map_err(String::into_bytes)?;
        let mut sql = b"DEALLOCATE ".to_vec();
        sql.extend_from_slice(&quoted);
        link.exec(&sql).map(|_| ())
    }

    /// `PQsetClientEncoding`.
    pub(crate) fn set_encoding(&mut self, encoding: &[u8]) -> Result<(), Vec<u8>> {
        let link = self.ready().map_err(|e| e.message().as_bytes().to_vec())?;
        if encoding.len() > 100 {
            return Err(b"pg - unexpected response from the server".to_vec());
        }
        let mut sql = b"set client_encoding to '".to_vec();
        sql.extend_from_slice(encoding);
        sql.push(b'\'');
        let result = link.exec(&sql).map(|_| ());
        self.note_encoding();
        result
    }

    /// Escapes text for the inside of single quotes, with what the connection knows of the
    /// server. One that was lost still knows; one that was closed does not.
    pub(crate) fn escape(&self, text: &[u8]) -> Result<Vec<u8>, String> {
        match &self.link {
            Some(l) => text::escape_string(text, l.encoding, l.std_strings),
            None => Err(CLOSED.into()),
        }
    }

    /// Quotes a name: well formed text by the module's own rules, without the connection, and
    /// other text as libpq would, with it.
    pub(crate) fn quote_name(&self, text: &[u8]) -> Result<Vec<u8>, String> {
        if text::well_formed(text, self.encoding()) {
            return Ok(text::quote_plain_name(text));
        }
        match &self.link {
            Some(l) => text::escape_identifier(text, l.encoding),
            None => Err(CLOSED.into()),
        }
    }
}
