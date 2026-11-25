//! MySQL's client/server protocol, blocking, as libmariadb 3.2 speaks it for MySQLOO: the
//! handshake with libmariadb's capabilities (several statements in one query only when asked,
//! several results always, no local files, the `utf8mb4` of its default character set), the
//! authentication plugins it has built in, TLS, text queries and prepared statements with the
//! text and the binary protocol, ping and quit; and the insert id and affected rows of each
//! result as libmariadb reports them (the insert id only an OK packet changes; the affected rows
//! of a stored result set are its number of rows).
//!
//! The connect (resolving aside) has one deadline for the TCP connection, the handshake, TLS
//! and authentication. Afterwards nothing times out: a query waits as long as the server takes.
//! What comes from the server is bounded before it sizes anything: a packet at 1 GiB (with
//! libmariadb's error 2020), a result set at 65535 columns, a public key at 16384 bits. A packet
//! that cannot be read, or one that is too big, leaves the connection broken, so that the next
//! command does not read what is left of it.
//!
//! Errors are libmariadb's: the server's code and message, or the client's (`Can't connect to
//! server on '127.0.0.1' (115)`, `Lost connection to server during query`, ...).

use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use super::auth;
use super::tls;
use super::value::{self, Binary, Meta, kind};

pub(crate) const CR_UNKNOWN_ERROR: u32 = 2000;
pub(crate) const CR_CONNECTION_ERROR: u32 = 2002;
pub(crate) const CR_CONN_HOST_ERROR: u32 = 2003;
pub(crate) const CR_UNKNOWN_HOST: u32 = 2005;
pub(crate) const CR_SERVER_GONE_ERROR: u32 = 2006;
pub(crate) const CR_VERSION_ERROR: u32 = 2007;
pub(crate) const CR_SERVER_LOST: u32 = 2013;
pub(crate) const CR_NET_PACKET_TOO_LARGE: u32 = 2020;
pub(crate) const CR_SSL_CONNECTION_ERROR: u32 = 2026;
pub(crate) const CR_MALFORMED_PACKET: u32 = 2027;
pub(crate) const CR_NO_PREPARE_STMT: u32 = 2030;
pub(crate) const CR_SECURE_AUTH: u32 = 2049;
pub(crate) const CR_SERVER_LOST_EXTENDED: u32 = 2055;
pub(crate) const CR_AUTH_PLUGIN_CANNOT_LOAD: u32 = 2059;

const CLIENT_LONG_PASSWORD: u32 = 1;
const CLIENT_LONG_FLAG: u32 = 4;
const CLIENT_CONNECT_WITH_DB: u32 = 8;
const CLIENT_PROTOCOL_41: u32 = 512;
const CLIENT_SSL: u32 = 2048;
const CLIENT_TRANSACTIONS: u32 = 8192;
const CLIENT_SECURE_CONNECTION: u32 = 32768;
const CLIENT_MULTI_STATEMENTS: u32 = 1 << 16;
const CLIENT_MULTI_RESULTS: u32 = 1 << 17;
const CLIENT_PS_MULTI_RESULTS: u32 = 1 << 18;
const CLIENT_PLUGIN_AUTH: u32 = 1 << 19;
const CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 1 << 21;

pub(crate) const SERVER_MORE_RESULTS_EXIST: u16 = 8;
pub(crate) const SERVER_STATUS_NO_BACKSLASH_ESCAPES: u16 = 512;
/// The result set of the OUT and INOUT parameters of a CALL in a prepared statement, which
/// MySQL follows with an OK packet without saying that more results exist.
pub(crate) const SERVER_PS_OUT_PARAMS: u16 = 0x1000;

/// `utf8mb4_general_ci`, the collation of libmariadb's default character set.
const UTF8MB4_GENERAL_CI: u8 = 45;
const MAX_PACKET: u32 = 1 << 24;
const MAX_PAYLOAD: usize = 0xff_ffff;
/// The largest packet read or written, libmariadb's default `max_allowed_packet`.
pub(crate) const MAX_ALLOWED_PACKET: usize = 1 << 30;
/// The most columns a result set may have.
pub(crate) const MAX_COLUMNS: u64 = 65535;

const COM_QUIT: u8 = 0x01;
const COM_QUERY: u8 = 0x03;
const COM_PING: u8 = 0x0e;
const COM_STMT_PREPARE: u8 = 0x16;
const COM_STMT_EXECUTE: u8 = 0x17;
const COM_STMT_CLOSE: u8 = 0x19;

/// The Unix socket of a connection to `localhost` without one, libmariadb's default.
#[cfg(unix)]
const DEFAULT_SOCKET: &str = "/tmp/mysql.sock";

/// An error as libmariadb reports it: its number and its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Error {
    pub code: u32,
    pub message: Vec<u8>,
}

impl Error {
    pub(crate) fn new(code: u32, message: impl Into<Vec<u8>>) -> Error {
        Error {
            code,
            message: message.into(),
        }
    }

    fn lost() -> Error {
        Error::new(CR_SERVER_LOST, "Lost connection to server during query")
    }

    fn gone() -> Error {
        Error::new(CR_SERVER_GONE_ERROR, "Server has gone away")
    }

    fn malformed() -> Error {
        Error::new(CR_MALFORMED_PACKET, "Received malformed packet")
    }

    fn too_large() -> Error {
        Error::new(
            CR_NET_PACKET_TOO_LARGE,
            "Got packet bigger than 'max_allowed_packet'",
        )
    }

    /// An error of the connect at a stage of it: the reason of TLS when it is one (an alert of
    /// the server after the handshake), that the connect timed out when its deadline passed
    /// (a read or write that waited too long; only the connect has a deadline), the lost
    /// connection otherwise.
    fn lost_at(what: &str, e: &Raw) -> Error {
        match e {
            Raw::TooLarge => Error::too_large(),
            Raw::Io(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Error::new(
                    CR_SERVER_LOST_EXTENDED,
                    format!("Lost connection to server at '{what}': the connect timed out"),
                )
            }
            Raw::Io(e) => match tls::tls_error(e) {
                Some(reason) => Error::new(
                    CR_SSL_CONNECTION_ERROR,
                    format!(
                        "SSL connection error: {}",
                        truncated(reason.to_string().as_bytes(), 100)
                    ),
                ),
                None => Error::new(
                    CR_SERVER_LOST_EXTENDED,
                    format!(
                        "Lost connection to server at '{what}', system error: {}",
                        errno(e)
                    ),
                ),
            },
        }
    }

    /// Whether the connection is out of step after it.
    fn breaks(&self) -> bool {
        matches!(
            self.code,
            CR_MALFORMED_PACKET | CR_NET_PACKET_TOO_LARGE | CR_SERVER_LOST | CR_SERVER_GONE_ERROR
        )
    }
}

/// Why a packet could not be read.
enum Raw {
    Io(std::io::Error),
    TooLarge,
}

/// What a connection is made with.
#[derive(Clone, Debug, Default)]
pub(crate) struct Settings {
    pub host: Vec<u8>,
    pub user: Vec<u8>,
    pub password: Vec<u8>,
    pub database: Vec<u8>,
    pub port: u32,
    pub socket: Vec<u8>,
    pub multi_statements: bool,
    pub tls: Option<tls::Settings>,
    /// How long the whole connect may take; zero for no limit.
    pub connect_timeout: Duration,
}

/// What a socket can be told about waiting.
trait Timeouts {
    fn set_timeouts(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl Timeouts for TcpStream {
    fn set_timeouts(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(timeout)?;
        self.set_write_timeout(timeout)
    }
}

#[cfg(unix)]
impl Timeouts for std::os::unix::net::UnixStream {
    fn set_timeouts(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(timeout)?;
        self.set_write_timeout(timeout)
    }
}

/// A socket whose reads and writes end at a deadline while there is one: before each, it is
/// told to wait no longer than the time that is left.
struct Socket<S> {
    inner: S,
    deadline: Option<Instant>,
}

impl<S: Timeouts> Socket<S> {
    fn new(inner: S, deadline: Option<Instant>) -> Socket<S> {
        Socket { inner, deadline }
    }

    fn arm(&self) -> std::io::Result<()> {
        let Some(deadline) = self.deadline else {
            return Ok(());
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        self.inner.set_timeouts(Some(left))
    }

    fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
        if deadline.is_none() {
            let _ = self.inner.set_timeouts(None);
        }
    }
}

impl<S: Read + Timeouts> Read for Socket<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.arm()?;
        self.inner.read(buf)
    }
}

impl<S: Write + Timeouts> Write for Socket<S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.arm()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

type Tls = rustls::StreamOwned<rustls::ClientConnection, Socket<TcpStream>>;

enum Stream {
    Tcp(Socket<TcpStream>),
    #[cfg(unix)]
    Unix(Socket<std::os::unix::net::UnixStream>),
    Tls(Box<Tls>),
    /// What is left while TLS takes the TCP stream over.
    Closed,
}

fn closed() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::NotConnected)
}

impl Stream {
    fn set_deadline(&mut self, deadline: Option<Instant>) {
        match self {
            Stream::Tcp(s) => s.set_deadline(deadline),
            #[cfg(unix)]
            Stream::Unix(s) => s.set_deadline(deadline),
            Stream::Tls(t) => t.sock.set_deadline(deadline),
            Stream::Closed => {}
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
            Stream::Closed => Err(closed()),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
            Stream::Closed => Err(closed()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
            Stream::Closed => Err(closed()),
        }
    }
}

/// A column of a result set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Column {
    pub name: Vec<u8>,
    pub meta: Meta,
}

/// A result set, every field as text (`None` for NULL).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Set {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Option<Vec<u8>>>>,
}

/// One result of a statement as the server reported it: its result set (an empty one for a
/// statement without), the insert id and the affected rows. A query gives one for each of its
/// results, in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Answer {
    pub set: Set,
    pub insert_id: u64,
    pub affected_rows: u64,
}

impl Answer {
    fn new(set: Set, insert_id: u64, affected_rows: u64) -> Answer {
        Answer {
            set,
            insert_id,
            affected_rows,
        }
    }
}

/// A prepared statement on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Statement {
    pub id: u32,
    pub params: u16,
}

/// A parameter of a prepared statement, with the type it is sent as.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Param {
    Null,
    Double(f64),
    Long(i32),
    Text(Vec<u8>),
}

enum Response {
    Ok,
    /// A result set, and whether it is that of OUT parameters.
    Set(Set, bool),
}

/// A connection.
pub(crate) struct Conn {
    io: BufReader<Stream>,
    seq: u8,
    caps: u32,
    max_packet: usize,
    /// The status flags of the server as of the last packet that had them.
    pub status: u16,
    /// The insert id of the last OK packet.
    pub insert_id: u64,
    /// The affected rows as of the last response.
    pub affected_rows: u64,
    /// The version the server announced, without the `5.5.5-` that MariaDB puts in front.
    pub server_info: Vec<u8>,
    /// `127.0.0.1 via TCP/IP` or `Localhost via UNIX socket`.
    pub host_info: Vec<u8>,
    broken: bool,
}

/// A reader of the fields of a packet.
struct Cur<'a> {
    b: &'a [u8],
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Cur<'a> {
        Cur { b }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.b.len() < n {
            return Err(Error::malformed());
        }
        let (head, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn uint(&mut self, n: usize) -> Result<u64, Error> {
        let b = self.take(n)?;
        Ok(b.iter().rev().fold(0u64, |a, x| (a << 8) | *x as u64))
    }

    /// A length-encoded integer; `None` for the 0xfb of NULL.
    fn lenenc(&mut self) -> Result<Option<u64>, Error> {
        match self.u8()? {
            0xfb => Ok(None),
            0xfc => self.uint(2).map(Some),
            0xfd => self.uint(3).map(Some),
            0xfe => self.uint(8).map(Some),
            n => Ok(Some(n as u64)),
        }
    }

    fn lenenc_int(&mut self) -> Result<u64, Error> {
        self.lenenc()?.ok_or_else(Error::malformed)
    }

    fn lenenc_bytes(&mut self) -> Result<Option<&'a [u8]>, Error> {
        match self.lenenc()? {
            None => Ok(None),
            Some(n) => self
                .take(usize::try_from(n).map_err(|_| Error::malformed())?)
                .map(Some),
        }
    }

    fn nul(&mut self) -> Result<&'a [u8], Error> {
        let n = self
            .b
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(Error::malformed)?;
        let s = self.take(n)?;
        self.take(1)?;
        Ok(s)
    }

    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.b)
    }
}

fn lenenc(out: &mut Vec<u8>, n: u64) {
    if n < 251 {
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.push(0xfc);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n < 1 << 24 {
        out.push(0xfd);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
    } else {
        out.push(0xfe);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

/// The number of a getaddrinfo error, from its message.
fn resolver_code(e: &std::io::Error) -> i32 {
    let text = e.to_string();
    if text.contains("Temporary failure") {
        -3
    } else if text.contains("No address associated") {
        -5
    } else {
        -2
    }
}

/// The errno of an error of a socket.
fn errno(e: &std::io::Error) -> i32 {
    if let Some(n) = e.raw_os_error() {
        return n;
    }
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => 110,
        std::io::ErrorKind::ConnectionRefused => 111,
        _ => 0,
    }
}

/// The errno libmariadb reports for a TCP connection that failed: its connect does not block,
/// and a failure that comes after the wait (refused, timed out, unreachable) keeps the
/// EINPROGRESS of the start.
fn tcp_errno(e: &std::io::Error) -> i32 {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::HostUnreachable
        | std::io::ErrorKind::ConnectionReset => 115,
        _ => errno(e),
    }
}

fn truncated(b: &[u8], n: usize) -> String {
    String::from_utf8_lossy(&b[..b.len().min(n)]).into_owned()
}

fn open_tcp(s: &Settings, deadline: Option<Instant>) -> Result<TcpStream, Error> {
    let host = String::from_utf8_lossy(&s.host).into_owned();
    let port = if s.port == 0 { 3306 } else { s.port as u16 };
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| {
            Error::new(
                CR_UNKNOWN_HOST,
                format!(
                    "Unknown server host '{}' ({})",
                    truncated(&s.host, 100),
                    resolver_code(&e)
                ),
            )
        })?
        .collect();
    let mut last = None;
    for addr in addrs {
        let attempt = match deadline {
            None => TcpStream::connect(addr),
            Some(d) => match d.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => TcpStream::connect_timeout(&addr, left),
                _ => Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            },
        };
        match attempt {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    let code = last.as_ref().map_or(0, tcp_errno);
    Err(Error::new(
        CR_CONN_HOST_ERROR,
        format!(
            "Can't connect to server on '{}' ({code})",
            truncated(&s.host, 64)
        ),
    ))
}

impl Conn {
    /// Connects: through a Unix socket for `localhost` or no host (on a system that has them),
    /// over TCP otherwise; then the handshake, TLS when it is asked for and the server has it,
    /// and authentication, all before the deadline of `connect_timeout`.
    pub(crate) fn connect(s: &Settings) -> Result<Conn, Error> {
        let deadline = (!s.connect_timeout.is_zero()).then(|| Instant::now() + s.connect_timeout);
        #[cfg(unix)]
        let local = s.host.is_empty() || s.host == b"localhost";
        #[cfg(not(unix))]
        let local = false;
        let (stream, host_info) = if local {
            #[cfg(unix)]
            {
                let path = if s.socket.is_empty() {
                    DEFAULT_SOCKET.as_bytes().to_vec()
                } else {
                    s.socket.clone()
                };
                let shown = truncated(&path, 64);
                let stream = std::os::unix::net::UnixStream::connect(
                    String::from_utf8_lossy(&path).as_ref(),
                )
                .map_err(|e| {
                    Error::new(
                        CR_CONNECTION_ERROR,
                        format!(
                            "Can't connect to local server through socket '{shown}' ({})",
                            errno(&e)
                        ),
                    )
                })?;
                (
                    Stream::Unix(Socket::new(stream, deadline)),
                    b"Localhost via UNIX socket".to_vec(),
                )
            }
            #[cfg(not(unix))]
            unreachable!()
        } else {
            let stream = open_tcp(s, deadline)?;
            (
                Stream::Tcp(Socket::new(stream, deadline)),
                format!("{} via TCP/IP", truncated(&s.host, 64)).into_bytes(),
            )
        };
        let mut conn = Conn {
            io: BufReader::new(stream),
            seq: 0,
            caps: 0,
            max_packet: MAX_ALLOWED_PACKET,
            status: 0,
            insert_id: 0,
            affected_rows: 0,
            server_info: Vec::new(),
            host_info,
            broken: false,
        };
        conn.handshake(s, local)?;
        conn.io.get_mut().set_deadline(None);
        Ok(conn)
    }

    /// The largest packet read or written, for the tests of the limit.
    #[cfg(test)]
    pub(crate) fn set_max_packet(&mut self, n: usize) {
        self.max_packet = n;
    }

    /// Reads a packet, put together from as many as it takes, up to the largest one allowed.
    fn read_raw(&mut self) -> Result<Vec<u8>, Raw> {
        let mut payload = Vec::new();
        loop {
            let mut head = [0u8; 4];
            self.io.read_exact(&mut head).map_err(Raw::Io)?;
            let len = u32::from_le_bytes([head[0], head[1], head[2], 0]) as usize;
            self.seq = head[3].wrapping_add(1);
            let start = payload.len();
            if start + len > self.max_packet {
                return Err(Raw::TooLarge);
            }
            payload.resize(start + len, 0);
            self.io.read_exact(&mut payload[start..]).map_err(Raw::Io)?;
            if len < MAX_PAYLOAD {
                return Ok(payload);
            }
        }
    }

    fn write_raw(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        let mut rest = payload;
        loop {
            let n = rest.len().min(MAX_PAYLOAD);
            out.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
            out.push(self.seq);
            self.seq = self.seq.wrapping_add(1);
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if n < MAX_PAYLOAD {
                break;
            }
        }
        let stream = self.io.get_mut();
        stream.write_all(&out)?;
        stream.flush()
    }

    /// The next packet of a response; a connection that breaks is lost for good.
    fn read(&mut self) -> Result<Vec<u8>, Error> {
        if self.broken {
            return Err(Error::gone());
        }
        self.read_raw().map_err(|e| {
            self.broken = true;
            match e {
                Raw::TooLarge => Error::too_large(),
                Raw::Io(_) => Error::lost(),
            }
        })
    }

    /// Sends a command, which begins a new sequence and leaves no affected rows yet.
    fn command(&mut self, payload: &[u8]) -> Result<(), Error> {
        if self.broken {
            return Err(Error::gone());
        }
        if payload.len() > self.max_packet {
            return Err(Error::too_large());
        }
        self.seq = 0;
        self.affected_rows = u64::MAX;
        self.write_raw(payload).map_err(|_| {
            self.broken = true;
            Error::gone()
        })
    }

    /// Leaves the connection broken after an error that puts it out of step.
    fn guard<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        if let Err(e) = &result
            && e.breaks()
        {
            self.broken = true;
        }
        result
    }

    fn handshake(&mut self, s: &Settings, local: bool) -> Result<(), Error> {
        let packet = self
            .read_raw()
            .map_err(|e| Error::lost_at("handshake: reading initial communication packet", &e))?;
        let mut c = Cur::new(&packet);
        let protocol = c.u8()?;
        if protocol == 0xff {
            return Err(server_error(&packet));
        }
        if protocol != 10 {
            return Err(Error::new(
                CR_VERSION_ERROR,
                format!("Protocol mismatch. Server Version = {protocol} Client Version = 10"),
            ));
        }
        let mut version = c.nul()?.to_vec();
        if version.starts_with(b"5.5.5-") {
            version.drain(..6);
        }
        self.server_info = version;
        c.u32()?;
        let mut nonce = c.take(8)?.to_vec();
        c.u8()?;
        let mut server_caps = c.u16()? as u32;
        let mut plugin = b"mysql_native_password".to_vec();
        if !c.b.is_empty() {
            c.u8()?;
            self.status = c.u16()?;
            server_caps |= (c.u16()? as u32) << 16;
            let data_len = c.u8()? as usize;
            c.take(10)?;
            if server_caps & CLIENT_SECURE_CONNECTION != 0 {
                let n = data_len.saturating_sub(8).max(13).min(c.b.len());
                let part = c.take(n)?;
                let part = part.strip_suffix(&[0]).unwrap_or(part);
                nonce.extend_from_slice(part);
            }
            if server_caps & CLIENT_PLUGIN_AUTH != 0 {
                let rest = c.rest();
                let name = rest.split(|b| *b == 0).next().unwrap_or_default();
                if !name.is_empty() {
                    plugin = name.to_vec();
                }
            }
        }

        let mut caps = CLIENT_LONG_PASSWORD
            | CLIENT_LONG_FLAG
            | CLIENT_PROTOCOL_41
            | CLIENT_TRANSACTIONS
            | CLIENT_SECURE_CONNECTION
            | CLIENT_MULTI_RESULTS
            | CLIENT_PS_MULTI_RESULTS
            | CLIENT_PLUGIN_AUTH
            | CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA;
        if s.multi_statements {
            caps |= CLIENT_MULTI_STATEMENTS;
        }
        if !s.database.is_empty() {
            caps |= CLIENT_CONNECT_WITH_DB;
        }
        caps &= server_caps | CLIENT_PROTOCOL_41;
        let mut secure = local;
        if let Some(tls) = &s.tls
            && server_caps & CLIENT_SSL != 0
            && !local
        {
            caps |= CLIENT_SSL;
            let mut request = Vec::with_capacity(32);
            request.extend_from_slice(&caps.to_le_bytes());
            request.extend_from_slice(&MAX_PACKET.to_le_bytes());
            request.push(UTF8MB4_GENERAL_CI);
            request.extend_from_slice(&[0; 23]);
            self.write_raw(&request).map_err(|e| {
                Error::lost_at("sending connection information to server", &Raw::Io(e))
            })?;
            let Stream::Tcp(tcp) = std::mem::replace(self.io.get_mut(), Stream::Closed) else {
                return Err(Error::new(
                    CR_SSL_CONNECTION_ERROR,
                    "SSL connection error: not a TCP connection",
                ));
            };
            let host = String::from_utf8_lossy(&s.host).into_owned();
            let encrypted = tls::handshake(tcp, &host, tls).map_err(|e| {
                Error::new(
                    CR_SSL_CONNECTION_ERROR,
                    format!("SSL connection error: {}", truncated(e.as_bytes(), 100)),
                )
            })?;
            *self.io.get_mut() = Stream::Tls(Box::new(encrypted));
            secure = true;
        }
        self.caps = caps;

        let known = matches!(
            plugin.as_slice(),
            b"mysql_native_password" | b"caching_sha2_password" | b"sha256_password"
        );
        if !known {
            plugin = b"mysql_native_password".to_vec();
        }
        let first = self.auth_data(&plugin, &s.password, &nonce, secure);
        let mut response = Vec::with_capacity(64 + first.len());
        response.extend_from_slice(&caps.to_le_bytes());
        response.extend_from_slice(&MAX_PACKET.to_le_bytes());
        response.push(UTF8MB4_GENERAL_CI);
        response.extend_from_slice(&[0; 23]);
        response.extend_from_slice(&s.user);
        response.push(0);
        if caps & CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA != 0 {
            lenenc(&mut response, first.len() as u64);
        } else {
            response.push(first.len() as u8);
        }
        response.extend_from_slice(&first);
        if caps & CLIENT_CONNECT_WITH_DB != 0 {
            response.extend_from_slice(&s.database);
            response.push(0);
        }
        if caps & CLIENT_PLUGIN_AUTH != 0 {
            response.extend_from_slice(&plugin);
            response.push(0);
        }
        self.write_raw(&response)
            .map_err(|e| Error::lost_at("sending authentication information", &Raw::Io(e)))?;
        self.authenticate(s, plugin, nonce, secure)
    }

    /// The first answer of a plugin to the nonce.
    fn auth_data(&self, plugin: &[u8], password: &[u8], nonce: &[u8], secure: bool) -> Vec<u8> {
        match plugin {
            b"caching_sha2_password" => auth::caching_sha2(password, nonce),
            b"sha256_password" => {
                if password.is_empty() {
                    vec![0]
                } else if secure {
                    let mut p = password.to_vec();
                    p.push(0);
                    p
                } else {
                    vec![1]
                }
            }
            _ => auth::native(password, nonce),
        }
    }

    fn authenticate(
        &mut self,
        s: &Settings,
        mut plugin: Vec<u8>,
        mut nonce: Vec<u8>,
        secure: bool,
    ) -> Result<(), Error> {
        let mut switched = false;
        loop {
            let packet = self
                .read_raw()
                .map_err(|e| Error::lost_at("reading authorization packet", &e))?;
            let send = |conn: &mut Conn, data: &[u8]| {
                conn.write_raw(data)
                    .map_err(|e| Error::lost_at("sending authentication information", &Raw::Io(e)))
            };
            match packet.first() {
                Some(0x00) => {
                    let mut c = Cur::new(&packet[1..]);
                    c.lenenc_int()?;
                    c.lenenc_int()?;
                    if c.b.len() >= 2 {
                        self.status = c.u16()?;
                    }
                    return Ok(());
                }
                Some(0xff) => return Err(server_error(&packet)),
                Some(0xfe) if !switched => {
                    switched = true;
                    if packet.len() == 1 {
                        return Err(Error::new(
                            CR_SECURE_AUTH,
                            "Connection with old authentication protocol refused.",
                        ));
                    }
                    let mut c = Cur::new(&packet[1..]);
                    let name = c.nul().unwrap_or_default().to_vec();
                    let data = c.rest();
                    nonce = data.strip_suffix(&[0]).unwrap_or(data).to_vec();
                    plugin = name;
                    if !matches!(
                        plugin.as_slice(),
                        b"mysql_native_password" | b"caching_sha2_password" | b"sha256_password"
                    ) {
                        let name = String::from_utf8_lossy(&plugin);
                        return Err(Error::new(
                            CR_AUTH_PLUGIN_CANNOT_LOAD,
                            format!(
                                "fpkg's mysqloo module does not implement the authentication plugin {name}"
                            ),
                        ));
                    }
                    let data = self.auth_data(&plugin, &s.password, &nonce, secure);
                    send(self, &data)?;
                }
                Some(0x01) => {
                    let more = &packet[1..];
                    match (plugin.as_slice(), more) {
                        (b"caching_sha2_password", [0x03]) => {}
                        (b"caching_sha2_password", [0x04]) => {
                            if secure {
                                let mut p = s.password.clone();
                                p.push(0);
                                send(self, &p)?;
                            } else {
                                send(self, &[0x02])?;
                            }
                        }
                        (b"caching_sha2_password" | b"sha256_password", pem) => {
                            let encrypted = auth::encrypt_password(&s.password, &nonce, pem)
                                .map_err(|e| Error::new(CR_UNKNOWN_ERROR, e))?;
                            send(self, &encrypted)?;
                        }
                        _ => return Err(Error::malformed()),
                    }
                }
                _ => return Err(Error::malformed()),
            }
        }
    }

    fn read_ok(&mut self, packet: &[u8]) -> Result<(), Error> {
        let mut c = Cur::new(&packet[1..]);
        self.affected_rows = c.lenenc_int()?;
        self.insert_id = c.lenenc_int()?;
        if c.b.len() >= 2 {
            self.status = c.u16()?;
        }
        Ok(())
    }

    /// The status of an EOF packet, which also becomes that of the connection.
    fn read_eof(&mut self, packet: &[u8]) -> u16 {
        if packet.len() >= 5 {
            self.status = u16::from_le_bytes([packet[3], packet[4]]);
        }
        self.status
    }

    fn read_err(&mut self, packet: &[u8]) -> Error {
        self.status &= !SERVER_MORE_RESULTS_EXIST;
        server_error(packet)
    }

    fn column(packet: &[u8]) -> Result<Column, Error> {
        let mut c = Cur::new(packet);
        for _ in 0..4 {
            c.lenenc_bytes()?;
        }
        let name = c.lenenc_bytes()?.unwrap_or_default().to_vec();
        c.lenenc_bytes()?;
        c.lenenc_int()?;
        c.u16()?;
        let length = c.u32()?;
        let kind = c.u8()?;
        let flags = c.u16()?;
        let decimals = c.u8()?;
        Ok(Column {
            name,
            meta: Meta {
                kind,
                flags,
                decimals,
                length,
            },
        })
    }

    /// Column definitions up to their EOF packet, and its status. The columns are kept as they
    /// come; their number has been checked.
    fn columns(&mut self, count: u64) -> Result<(Vec<Column>, u16), Error> {
        if count > MAX_COLUMNS {
            return Err(Error::malformed());
        }
        let mut columns = Vec::new();
        for _ in 0..count {
            let p = self.read()?;
            columns.push(Conn::column(&p)?);
        }
        let eof = self.read()?;
        if eof.first() != Some(&0xfe) || eof.len() >= 9 {
            return Err(Error::malformed());
        }
        let status = self.read_eof(&eof);
        Ok((columns, status))
    }

    /// A result set after its header, and whether it holds OUT parameters.
    fn set(&mut self, header: &[u8], binary: bool) -> Result<(Set, bool), Error> {
        let mut c = Cur::new(header);
        let count = c.lenenc_int()?;
        if count == 0 {
            return Err(Error::malformed());
        }
        let (columns, status) = self.columns(count)?;
        let mut out_params = status & SERVER_PS_OUT_PARAMS != 0;
        let mut rows = Vec::new();
        loop {
            let p = self.read()?;
            match p.first() {
                Some(0xfe) if p.len() < 9 => {
                    out_params |= self.read_eof(&p) & SERVER_PS_OUT_PARAMS != 0;
                    break;
                }
                Some(0xff) => return Err(self.read_err(&p)),
                _ => {}
            }
            rows.push(if binary {
                binary_row(&p, &columns)?
            } else {
                text_row(&p, columns.len())?
            });
        }
        Ok((Set { columns, rows }, out_params))
    }

    fn response(&mut self, binary: bool) -> Result<Response, Error> {
        let p = self.read()?;
        match p.first() {
            Some(0x00) => {
                self.read_ok(&p)?;
                Ok(Response::Ok)
            }
            Some(0xff) => Err(self.read_err(&p)),
            Some(0xfb) => {
                self.write_raw(&[]).map_err(|_| {
                    self.broken = true;
                    Error::gone()
                })?;
                let after = self.read()?;
                if after.first() == Some(&0x00) {
                    self.read_ok(&after)?;
                }
                Err(Error::new(
                    CR_UNKNOWN_ERROR,
                    "fpkg's mysqloo module does not implement LOAD DATA LOCAL INFILE",
                ))
            }
            Some(_) => {
                let (set, out_params) = self.set(&p, binary)?;
                Ok(Response::Set(set, out_params))
            }
            None => Err(Error::malformed()),
        }
    }

    /// Sends a query (`COM_QUERY`) and reads each of its results into `out`, until the last
    /// result or the first error. The affected rows of a result set are its number of rows; its
    /// insert id is that of the last OK packet on the connection.
    pub(crate) fn query(&mut self, sql: &[u8], out: &mut Vec<Answer>) -> Result<(), Error> {
        let r = self.query_results(sql, out);
        self.guard(r)
    }

    fn query_results(&mut self, sql: &[u8], out: &mut Vec<Answer>) -> Result<(), Error> {
        let mut payload = Vec::with_capacity(sql.len() + 1);
        payload.push(COM_QUERY);
        payload.extend_from_slice(sql);
        self.command(&payload)?;
        loop {
            match self.response(false)? {
                Response::Ok => out.push(Answer::new(
                    Set::default(),
                    self.insert_id,
                    self.affected_rows,
                )),
                Response::Set(set, _) => {
                    self.affected_rows = set.rows.len() as u64;
                    out.push(Answer::new(set, self.insert_id, self.affected_rows));
                }
            }
            if self.status & SERVER_MORE_RESULTS_EXIST == 0 {
                return Ok(());
            }
            self.affected_rows = u64::MAX;
        }
    }

    /// Prepares a statement (`COM_STMT_PREPARE`).
    pub(crate) fn prepare(&mut self, sql: &[u8]) -> Result<Statement, Error> {
        let r = self.prepare_statement(sql);
        self.guard(r)
    }

    fn prepare_statement(&mut self, sql: &[u8]) -> Result<Statement, Error> {
        let mut payload = Vec::with_capacity(sql.len() + 1);
        payload.push(COM_STMT_PREPARE);
        payload.extend_from_slice(sql);
        self.command(&payload)?;
        let p = self.read()?;
        match p.first() {
            Some(0x00) => {}
            Some(0xff) => return Err(self.read_err(&p)),
            _ => return Err(Error::malformed()),
        }
        let mut c = Cur::new(&p[1..]);
        let id = c.u32()?;
        let columns = c.u16()?;
        let params = c.u16()?;
        if params > 0 {
            self.columns(params as u64)?;
        }
        if columns > 0 {
            self.columns(columns as u64)?;
        }
        Ok(Statement { id, params })
    }

    /// Runs a prepared statement (`COM_STMT_EXECUTE`) with the parameters, and reads each of
    /// its results into `out`. The affected rows and the insert id of the first result are
    /// those of the connection after it (no affected rows, 2^64-1, for a result set); those of
    /// a later one are those of its OK packet, or for a result set the number of rows of the
    /// result set before. The result set of OUT parameters is followed by one more result.
    pub(crate) fn execute(
        &mut self,
        statement: &Statement,
        params: &[Param],
        out: &mut Vec<Answer>,
    ) -> Result<(), Error> {
        let r = self.execute_statement(statement, params, out);
        self.guard(r)
    }

    fn execute_statement(
        &mut self,
        statement: &Statement,
        params: &[Param],
        out: &mut Vec<Answer>,
    ) -> Result<(), Error> {
        let mut payload = Vec::with_capacity(16 + params.len() * 10);
        payload.push(COM_STMT_EXECUTE);
        payload.extend_from_slice(&statement.id.to_le_bytes());
        payload.push(0);
        payload.extend_from_slice(&1u32.to_le_bytes());
        if !params.is_empty() {
            let mut nulls = vec![0u8; params.len().div_ceil(8)];
            for (i, p) in params.iter().enumerate() {
                if matches!(p, Param::Null) {
                    nulls[i / 8] |= 1 << (i % 8);
                }
            }
            payload.extend_from_slice(&nulls);
            payload.push(1);
            for p in params {
                let t = match p {
                    Param::Null => kind::NULL,
                    Param::Double(_) => kind::DOUBLE,
                    Param::Long(_) => kind::LONG,
                    Param::Text(_) => kind::STRING,
                };
                payload.extend_from_slice(&[t, 0]);
            }
            for p in params {
                match p {
                    Param::Null => {}
                    Param::Double(d) => payload.extend_from_slice(&d.to_le_bytes()),
                    Param::Long(n) => payload.extend_from_slice(&n.to_le_bytes()),
                    Param::Text(t) => {
                        lenenc(&mut payload, t.len() as u64);
                        payload.extend_from_slice(t);
                    }
                }
            }
        }
        self.command(&payload)?;
        let mut first = true;
        let (mut affected, mut insert_id) = (0, 0);
        loop {
            let mut more = false;
            match self.response(true)? {
                Response::Ok => {
                    affected = self.affected_rows;
                    insert_id = self.insert_id;
                    out.push(Answer::new(Set::default(), insert_id, affected));
                }
                Response::Set(set, out_params) => {
                    if first {
                        affected = self.affected_rows;
                        insert_id = self.insert_id;
                    }
                    let rows = set.rows.len() as u64;
                    out.push(Answer::new(set, insert_id, affected));
                    affected = rows;
                    more = out_params;
                }
            }
            first = false;
            if self.status & SERVER_MORE_RESULTS_EXIST == 0 && !more {
                return Ok(());
            }
            self.affected_rows = u64::MAX;
        }
    }

    /// Closes a prepared statement (`COM_STMT_CLOSE`): the server sends nothing back.
    pub(crate) fn close_statement(&mut self, statement: &Statement) {
        let mut payload = vec![COM_STMT_CLOSE];
        payload.extend_from_slice(&statement.id.to_le_bytes());
        let _ = self.command(&payload);
    }

    /// Asks the server whether it is there (`COM_PING`).
    pub(crate) fn ping(&mut self) -> Result<(), Error> {
        let r = self.ping_server();
        self.guard(r)
    }

    fn ping_server(&mut self) -> Result<(), Error> {
        self.command(&[COM_PING])?;
        let p = self.read()?;
        match p.first() {
            Some(0x00) => self.read_ok(&p),
            Some(0xff) => Err(self.read_err(&p)),
            _ => Err(Error::malformed()),
        }
    }

    /// Says goodbye to the server (`COM_QUIT`).
    pub(crate) fn quit(&mut self) {
        if !self.broken {
            let _ = self.command(&[COM_QUIT]);
        }
        self.broken = true;
    }

    /// Whether the server is in `NO_BACKSLASH_ESCAPES` mode, as the last status said.
    pub(crate) fn no_backslash_escapes(&self) -> bool {
        self.status & SERVER_STATUS_NO_BACKSLASH_ESCAPES != 0
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.quit();
    }
}

/// The error of an ERR packet: its code and message.
fn server_error(packet: &[u8]) -> Error {
    let mut c = Cur::new(packet.get(1..).unwrap_or_default());
    let code = c.u16().unwrap_or(0) as u32;
    if c.b.first() == Some(&b'#') && c.b.len() >= 6 {
        let _ = c.take(6);
    }
    Error::new(code, c.rest().to_vec())
}

pub(crate) fn text_row(packet: &[u8], count: usize) -> Result<Vec<Option<Vec<u8>>>, Error> {
    let mut c = Cur::new(packet);
    let mut row = Vec::with_capacity(count);
    for _ in 0..count {
        row.push(c.lenenc_bytes()?.map(<[u8]>::to_vec));
    }
    Ok(row)
}

pub(crate) fn binary_row(packet: &[u8], columns: &[Column]) -> Result<Vec<Option<Vec<u8>>>, Error> {
    let mut c = Cur::new(packet);
    if c.u8()? != 0 {
        return Err(Error::malformed());
    }
    let nulls = c.take((columns.len() + 9) / 8)?;
    let mut row = Vec::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        let bit = i + 2;
        if nulls[bit / 8] & (1 << (bit % 8)) != 0 {
            row.push(None);
            continue;
        }
        let m = &col.meta;
        let unsigned = m.flags & value::UNSIGNED != 0;
        let v = match m.kind {
            kind::TINY => {
                let b = c.u8()?;
                if unsigned {
                    Binary::Unsigned(b as u64)
                } else {
                    Binary::Signed(b as i8 as i64)
                }
            }
            kind::SHORT | kind::YEAR => {
                let n = c.u16()?;
                if unsigned {
                    Binary::Unsigned(n as u64)
                } else {
                    Binary::Signed(n as i16 as i64)
                }
            }
            kind::LONG | kind::INT24 => {
                let n = c.u32()?;
                if unsigned {
                    Binary::Unsigned(n as u64)
                } else {
                    Binary::Signed(n as i32 as i64)
                }
            }
            kind::LONGLONG => {
                let n = c.uint(8)?;
                if unsigned {
                    Binary::Unsigned(n)
                } else {
                    Binary::Signed(n as i64)
                }
            }
            kind::FLOAT => Binary::Float(f32::from_bits(c.u32()?)),
            kind::DOUBLE => Binary::Double(f64::from_bits(c.uint(8)?)),
            kind::DATE
            | kind::DATETIME
            | kind::TIMESTAMP
            | kind::NEWDATE
            | kind::DATETIME2
            | kind::TIMESTAMP2 => {
                let len = c.u8()? as usize;
                let b = c.take(len)?;
                let mut d = Cur::new(b);
                let (mut y, mut mo, mut day, mut h, mut mi, mut s, mut us) = (0, 0, 0, 0, 0, 0, 0);
                if len >= 4 {
                    y = d.u16()?;
                    mo = d.u8()?;
                    day = d.u8()?;
                }
                if len >= 7 {
                    h = d.u8()?;
                    mi = d.u8()?;
                    s = d.u8()?;
                }
                if len >= 11 {
                    us = d.u32()?;
                }
                Binary::DateTime(y, mo, day, h, mi, s, us)
            }
            kind::TIME | kind::TIME2 => {
                let len = c.u8()? as usize;
                let b = c.take(len)?;
                let mut d = Cur::new(b);
                let (mut neg, mut days, mut h, mut mi, mut s, mut us) = (false, 0, 0, 0, 0, 0);
                if len >= 8 {
                    neg = d.u8()? != 0;
                    days = d.u32()?;
                    h = d.u8()?;
                    mi = d.u8()?;
                    s = d.u8()?;
                }
                if len >= 12 {
                    us = d.u32()?;
                }
                Binary::Time(neg, days, h, mi, s, us)
            }
            kind::NULL => {
                row.push(None);
                continue;
            }
            _ => Binary::Bytes(c.lenenc_bytes()?.unwrap_or_default().to_vec()),
        };
        row.push(Some(value::binary_text(m, v)));
    }
    Ok(row)
}
