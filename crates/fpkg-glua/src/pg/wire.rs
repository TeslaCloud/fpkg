//! A connection to a server in PostgreSQL's protocol 3.0, through postgres-protocol's
//! messages and authentication, over TCP or a Unix-domain socket and encrypted when `sslmode`
//! asks for it. It blocks, and runs one statement at a time, with text in both directions, the
//! way libpq runs them for gmsv_pg: a statement without parameters as a simple query (several
//! in one, the last result being the query's), one with parameters or a prepared one with the
//! extended protocol, and what libpq writes into its messages: the host it tried, the server's
//! error fields in its layout, "server closed the connection unexpectedly".

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use bytes::{BufMut, BytesMut};
use fallible_iterator::FallibleIterator;
use postgres_protocol::authentication::{md5_hash, sasl};
use postgres_protocol::message::backend::{DataRowBody, Message};
use postgres_protocol::message::frontend;

use super::conninfo::{self, Options};
use super::text::{self, Encoding, Fields};
use super::tls;

/// What libpq says when the server is gone in the middle of something.
const CLOSED: &str = "server closed the connection unexpectedly\n\tThis probably means the server terminated abnormally\n\tbefore or while processing the request.";

const COPY_IN: &str = "pg - COPY FROM STDIN is not supported";
const COPY_OUT: &str = "pg - COPY TO STDOUT is not supported";
const REPLICATION: &str = "pg - replication is not supported";

/// The text of an error of the system, without the "(os error n)" that Rust adds.
pub(crate) fn os_text(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if e.raw_os_error().is_some() => s[..i].to_string(),
        _ => s,
    }
}

enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tls(Box<tls::Tls>),
}

impl Stream {
    fn set_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => {
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
            #[cfg(unix)]
            Stream::Unix(s) => {
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
            Stream::Tls(s) => {
                s.sock.set_read_timeout(t)?;
                s.sock.set_write_timeout(t)
            }
        }
    }

    fn shutdown(&self) {
        let _ = match self {
            Stream::Tcp(s) => s.shutdown(std::net::Shutdown::Both),
            #[cfg(unix)]
            Stream::Unix(s) => s.shutdown(std::net::Shutdown::Both),
            Stream::Tls(s) => s.sock.shutdown(std::net::Shutdown::Both),
        };
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// A column of a result: its name and the OID of its type.
#[derive(Clone, Debug, Default)]
pub(crate) struct Column {
    pub name: Vec<u8>,
    pub type_oid: u32,
}

/// A result that is no error: its columns, its rows (`DataRow` messages) and its command tag.
#[derive(Default)]
pub(crate) struct Set {
    pub columns: Vec<Column>,
    pub rows: Vec<DataRowBody>,
    pub tag: Vec<u8>,
}

impl Set {
    /// The fields of a row, NULL as `None`.
    pub(crate) fn fields<'a>(&self, row: &'a DataRowBody) -> Vec<Option<&'a [u8]>> {
        let buf = row.buffer();
        let mut out = Vec::with_capacity(self.columns.len());
        let mut ranges = row.ranges();
        while let Ok(Some(range)) = ranges.next() {
            out.push(range.map(|r| &buf[r]));
        }
        out
    }
}

/// What the server said while a statement ran that is no error.
pub(crate) struct Notice {
    pub message: Vec<u8>,
    pub fields: Fields,
}

/// What came of a statement, as the module concludes it.
#[derive(Default)]
pub(crate) struct Done {
    pub success: bool,
    pub error: Vec<u8>,
    /// The fields of the first error the server sent, if it did and the statement was not
    /// refused.
    pub details: Option<Fields>,
    pub lost: bool,
    pub set: Option<Set>,
    pub notices: Vec<Notice>,
}

/// How a statement goes to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// As a simple query, which may be several statements.
    Plain,
    /// One statement with `$1`, `$2` and so on.
    Parameterised,
    /// A statement prepared before, `text` being its name.
    Prepared,
}

pub(crate) struct Statement {
    pub kind: Kind,
    pub text: Vec<u8>,
    pub params: Vec<Option<Vec<u8>>>,
}

/// What has come of a statement while its results are read.
#[derive(Default)]
struct Outcome {
    sent: bool,
    last: Option<Set>,
    error: Vec<u8>,
    failed: Option<Fields>,
    refused: Option<&'static str>,
    notices: Vec<Notice>,
    lost: bool,
    /// What libpq has in the error message of the connection by the end of the statement.
    connection: Vec<u8>,
}

impl Outcome {
    /// An error result: its message goes into the error, and into the connection's.
    fn fail(&mut self, message: Vec<u8>, fields: Fields) {
        if !self.error.is_empty() {
            self.error.push(b'\n');
        }
        self.error.extend_from_slice(text::trimmed(&message));
        self.connection.extend_from_slice(&message);
        if self.failed.is_none() {
            self.failed = Some(fields);
        }
    }

    fn lose(&mut self, why: &str) {
        self.lost = true;
        self.connection.extend_from_slice(why.as_bytes());
        self.connection.push(b'\n');
    }

    /// The module's `conclude`.
    fn conclude(mut self) -> Done {
        let mut done = Done {
            lost: self.lost,
            ..Done::default()
        };
        if let Some(refused) = self.refused {
            done.error = refused.as_bytes().to_vec();
        } else if self.lost {
            let all = text::trimmed(&self.connection).to_vec();
            if all.starts_with(&self.error) {
                done.error = all;
            } else {
                self.error.push(b'\n');
                self.error.extend_from_slice(&all);
                done.error = self.error;
            }
        } else if self.sent && self.failed.is_none() {
            done.success = true;
            done.set = Some(self.last.unwrap_or_default());
        } else {
            done.error = self.error;
        }
        if self.refused.is_none() {
            done.details = self.failed;
        }
        done.notices = self.notices;
        done
    }
}

/// A message from the server, with the two that postgres-protocol does not read the way they
/// are needed here: a row description whose names need not be UTF-8, and `CopyBothResponse`.
enum In {
    Msg(Message),
    Columns(Vec<Column>),
    CopyBoth,
    /// An authentication request of a kind that postgres-protocol does not know.
    Authentication(u32),
}

/// The authentication requests that postgres-protocol reads.
const AUTHENTICATIONS: &[u32] = &[0, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12];

fn row_description(body: &[u8]) -> Option<Vec<Column>> {
    let count = u16::from_be_bytes(body.get(..2)?.try_into().ok()?) as usize;
    let mut at = 2;
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        let end = at + body.get(at..)?.iter().position(|c| *c == 0)?;
        let name = body[at..end].to_vec();
        at = end + 1;
        let fixed = body.get(at..at + 18)?;
        let type_oid = u32::from_be_bytes(fixed[6..10].try_into().ok()?);
        at += 18;
        columns.push(Column { name, type_oid });
    }
    (at == body.len()).then_some(columns)
}

fn put_cstr(buf: &mut BytesMut, s: &[u8]) {
    buf.put_slice(s);
    buf.put_u8(0);
}

/// A message with its tag and length.
fn put_message(buf: &mut BytesMut, tag: u8, body: impl FnOnce(&mut BytesMut)) {
    buf.put_u8(tag);
    let at = buf.len();
    buf.put_i32(0);
    body(buf);
    let len = (buf.len() - at) as i32;
    buf[at..at + 4].copy_from_slice(&len.to_be_bytes());
}

fn put_parse(buf: &mut BytesMut, name: &[u8], query: &[u8]) {
    put_message(buf, b'P', |b| {
        put_cstr(b, name);
        put_cstr(b, query);
        b.put_i16(0);
    });
}

fn put_bind(buf: &mut BytesMut, statement: &[u8], params: &[Option<Vec<u8>>]) {
    put_message(buf, b'B', |b| {
        put_cstr(b, b"");
        put_cstr(b, statement);
        b.put_i16(0);
        b.put_u16(params.len() as u16);
        for p in params {
            match p {
                Some(v) => {
                    b.put_i32(v.len() as i32);
                    b.put_slice(v);
                }
                None => b.put_i32(-1),
            }
        }
        b.put_i16(1);
        b.put_i16(0);
    });
}

fn put_describe_execute_sync(buf: &mut BytesMut) {
    let _ = frontend::describe(b'P', "", buf);
    let _ = frontend::execute("", 0, buf);
    frontend::sync(buf);
}

/// `sscanf(value, "%d.%d.%d")` turned into a number as libpq turns `server_version`.
fn server_version(value: &[u8]) -> i32 {
    let mut parts = Vec::new();
    let mut at = 0;
    while parts.len() < 3 {
        if !parts.is_empty() {
            if value.get(at) != Some(&b'.') {
                break;
            }
            at += 1;
        }
        while value.get(at).is_some_and(|c| c.is_ascii_whitespace()) {
            at += 1;
        }
        let start = at;
        if matches!(value.get(at), Some(b'-' | b'+')) {
            at += 1;
        }
        while value.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        match std::str::from_utf8(&value[start..at])
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
        {
            Some(n) => parts.push(n),
            None => break,
        }
    }
    match parts[..] {
        [major, minor, revision] => (100 * major + minor) * 100 + revision,
        [major, minor] if major >= 10 => 100 * 100 * major + minor,
        [major, minor] => (100 * major + minor) * 100,
        [major] => 100 * 100 * major,
        _ => 0,
    }
}

/// An open connection, or one that was lost and is kept for what libpq still knows of it.
pub(crate) struct Link {
    stream: Stream,
    input: BytesMut,
    /// When the start-up has to be over, while it is going on.
    deadline: Option<Instant>,
    pub server_version: i32,
    pub encoding: Encoding,
    pub std_strings: bool,
    pub open: bool,
}

impl Drop for Link {
    fn drop(&mut self) {
        if self.open {
            let mut buf = BytesMut::new();
            frontend::terminate(&mut buf);
            let _ = self.stream.write_all(&buf);
        }
        self.stream.shutdown();
    }
}

/// Why an attempt to connect did not work, and whether it was the server that said no.
struct Refusal {
    message: String,
    /// Whether the server said no during authentication, which is what libpq tries the other
    /// way of encryption after (and not an error after authentication, such as an unknown
    /// database).
    by_server: bool,
    /// Whether the session was encrypted when it was refused.
    encrypted: bool,
    /// Whether it was encryption that could not be set up.
    negotiation: bool,
}

impl From<String> for Refusal {
    fn from(message: String) -> Refusal {
        Refusal {
            message,
            by_server: false,
            encrypted: false,
            negotiation: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SslMode {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

/// Where an attempt goes.
enum Target {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

/// What an attempt needs to know besides where it goes.
struct Plan<'a> {
    options: &'a Options,
    user: Vec<u8>,
    database: Vec<u8>,
    /// The host name for SNI and the certificate.
    host: String,
    deadline: Option<Instant>,
}

impl Plan<'_> {
    fn remaining(&self) -> Result<Option<Duration>, String> {
        match self.deadline {
            None => Ok(None),
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    Err("timeout expired".into())
                } else {
                    Ok(Some(left))
                }
            }
        }
    }
}

/// Connects as libpq connects for `PQconnectdb`, or says why it did not, in libpq's words.
pub(crate) fn connect(options: &Options) -> Result<Link, String> {
    let mode = match options.sslmode.as_deref().unwrap_or(b"prefer") {
        b"disable" => SslMode::Disable,
        b"allow" => SslMode::Allow,
        b"prefer" => SslMode::Prefer,
        b"require" => SslMode::Require,
        b"verify-ca" => SslMode::VerifyCa,
        b"verify-full" => SslMode::VerifyFull,
        other => {
            return Err(format!(
                "invalid sslmode value: \"{}\"",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let rootcert = options
        .sslrootcert
        .as_ref()
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned());
    let mut mode = mode;
    if rootcert.as_deref() == Some("system") {
        if options.sslmode.is_none() {
            mode = SslMode::VerifyFull;
        } else if mode != SslMode::VerifyFull {
            return Err(format!(
                "weak sslmode \"{}\" may not be used with sslrootcert=system (use \"verify-full\")",
                String::from_utf8_lossy(options.sslmode.as_deref().unwrap_or_default())
            ));
        }
    }
    let file = rootcert
        .filter(|r| r != "system")
        .map(std::path::PathBuf::from);
    let verify = match mode {
        SslMode::VerifyCa | SslMode::VerifyFull => tls::Verify {
            check: if mode == SslMode::VerifyCa {
                tls::Check::Chain
            } else {
                tls::Check::Full
            },
            roots: file.map_or(tls::Roots::Builtin, tls::Roots::File),
        },
        _ => match file.filter(|f| f.exists()) {
            Some(f) => tls::Verify {
                check: tls::Check::Chain,
                roots: tls::Roots::File(f),
            },
            None => tls::Verify {
                check: tls::Check::Nothing,
                roots: tls::Roots::Builtin,
            },
        },
    };
    let timeout = match &options.connect_timeout {
        Some(v) => conninfo::int_param(v, "connect_timeout")?,
        None => 0,
    };
    let lists = [&options.host, &options.hostaddr, &options.port];
    if lists
        .iter()
        .any(|v| v.as_ref().is_some_and(|v| v.contains(&b',')))
    {
        return Err(
            "fpkg's pg module does not implement several hosts in one connection string".into(),
        );
    }
    let port_text = options.port.as_ref().filter(|p| !p.is_empty());
    let port = match port_text {
        None => 5432,
        Some(p) => {
            let n = conninfo::int_param(p, "port")?;
            if !(1..=65535).contains(&n) {
                return Err(format!(
                    "invalid port number: \"{}\"",
                    String::from_utf8_lossy(p)
                ));
            }
            n as u16
        }
    };
    let shown_port = port_text.map_or("5432".to_string(), |p| {
        String::from_utf8_lossy(p).into_owned()
    });
    let user = options.user.clone().unwrap_or_default();
    let database = options
        .dbname
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| user.clone());
    let host = String::from_utf8_lossy(options.host.as_deref().unwrap_or_default()).into_owned();
    let hostaddr = options
        .hostaddr
        .as_ref()
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned());

    let mut targets: Vec<(Target, String)> = Vec::new();
    if let Some(addr) = &hostaddr {
        let ip: IpAddr = addr.parse().map_err(|_| {
            format!("could not parse network address \"{addr}\": Name or service not known")
        })?;
        targets.push((
            Target::Tcp(SocketAddr::new(ip, port)),
            format!("connection to server at \"{addr}\", port {shown_port} failed: "),
        ));
    } else if host.is_empty() || host.starts_with('/') {
        #[cfg(unix)]
        {
            let dir = if host.is_empty() {
                "/tmp"
            } else {
                host.as_str()
            };
            let path = std::path::Path::new(dir).join(format!(".s.PGSQL.{port}"));
            let shown = format!(
                "connection to server on socket \"{}\" failed: ",
                path.display()
            );
            targets.push((Target::Unix(path), shown));
        }
        #[cfg(not(unix))]
        return Err(
            "fpkg's pg module does not implement Unix-domain sockets on this system".into(),
        );
    } else {
        let addrs = (host.as_str(), port).to_socket_addrs().map_err(|e| {
            let why = e.to_string();
            let why = why
                .strip_prefix("failed to lookup address information: ")
                .unwrap_or(&why)
                .to_string();
            format!("could not translate host name \"{host}\" to address: {why}")
        })?;
        for addr in addrs {
            let ip = addr.ip().to_string();
            let shown = if ip == host {
                format!("connection to server at \"{host}\", port {shown_port} failed: ")
            } else {
                format!("connection to server at \"{host}\" ({ip}), port {shown_port} failed: ")
            };
            targets.push((Target::Tcp(addr), shown));
        }
    }

    let mut errors = String::new();
    for (target, shown) in targets {
        let deadline =
            (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout.max(2) as u64));
        let plan = Plan {
            options,
            user: user.clone(),
            database: database.clone(),
            host: if host.is_empty() {
                hostaddr.clone().unwrap_or_default()
            } else {
                host.clone()
            },
            deadline,
        };
        errors.push_str(&shown);
        match attempt(&target, mode, &verify, &plan, &shown, &mut errors) {
            Ok(link) => return Ok(link),
            Err(why) => {
                errors.push_str(&why);
                errors.push('\n');
            }
        }
    }
    Err(String::from_utf8_lossy(text::trimmed(errors.as_bytes())).into_owned())
}

/// One address, with the retries that the `sslmode` allows; what went wrong in a try that is
/// followed by another goes into `errors`, and the other try begins with `shown` once more, as
/// libpq names the server before each.
fn attempt(
    target: &Target,
    mode: SslMode,
    verify: &tls::Verify,
    plan: &Plan,
    shown: &str,
    errors: &mut String,
) -> Result<Link, String> {
    let mut again = |r: &Refusal| {
        errors.push_str(&r.message);
        errors.push('\n');
        errors.push_str(shown);
    };
    match mode {
        SslMode::Disable => start(target, None, plan).map_err(|r| r.message),
        SslMode::Allow => match start(target, None, plan) {
            Ok(link) => Ok(link),
            Err(r) if r.by_server => {
                again(&r);
                start(target, Some((verify, true)), plan).map_err(|r| r.message)
            }
            Err(r) => Err(r.message),
        },
        SslMode::Prefer => match start(target, Some((verify, false)), plan) {
            Ok(link) => Ok(link),
            Err(r) if r.negotiation || (r.by_server && r.encrypted) => {
                again(&r);
                start(target, None, plan).map_err(|r| r.message)
            }
            Err(r) => Err(r.message),
        },
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => {
            start(target, Some((verify, true)), plan).map_err(|r| r.message)
        }
    }
}

/// Opens the socket, negotiates encryption if `tls` says so (how much to check, and whether
/// it is required), and goes through the start-up of a session.
fn start(target: &Target, tls: Option<(&tls::Verify, bool)>, plan: &Plan) -> Result<Link, Refusal> {
    let stream = match target {
        Target::Tcp(addr) => {
            let left = plan.remaining()?;
            let socket = match left {
                Some(t) => TcpStream::connect_timeout(addr, t),
                None => TcpStream::connect(addr),
            }
            .map_err(|e| match e.kind() {
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    "timeout expired".to_string()
                }
                _ => format!(
                    "{}\n\tIs the server running on that host and accepting TCP/IP connections?",
                    os_text(&e)
                ),
            })?;
            let _ = socket.set_nodelay(true);
            socket
                .set_read_timeout(plan.remaining()?)
                .and_then(|_| socket.set_write_timeout(plan.remaining().ok().flatten()))
                .map_err(|e| os_text(&e))?;
            match tls {
                None => Stream::Tcp(socket),
                Some((verify, required)) => negotiate(socket, addr.ip(), verify, required, plan)?,
            }
        }
        #[cfg(unix)]
        Target::Unix(path) => {
            let socket = std::os::unix::net::UnixStream::connect(path).map_err(|e| {
                format!(
                    "{}\n\tIs the server running locally and accepting connections on that socket?",
                    os_text(&e)
                )
            })?;
            let s = Stream::Unix(socket);
            s.set_timeout(plan.remaining()?).map_err(|e| os_text(&e))?;
            s
        }
    };
    let mut link = Link {
        stream,
        input: BytesMut::new(),
        deadline: plan.deadline,
        server_version: 0,
        encoding: Encoding::Single,
        std_strings: true,
        open: false,
    };
    let encrypted = matches!(link.stream, Stream::Tls(_));
    link.startup(plan).map_err(|r| Refusal { encrypted, ..r })?;
    link.deadline = None;
    link.stream.set_timeout(None).map_err(|e| os_text(&e))?;
    link.open = true;
    Ok(link)
}

/// Asks the server for TLS and encrypts the socket if it agrees. A server that does not
/// leaves it as it is, unless encryption is required.
fn negotiate(
    mut socket: TcpStream,
    ip: IpAddr,
    verify: &tls::Verify,
    required: bool,
    plan: &Plan,
) -> Result<Stream, Refusal> {
    let mut buf = BytesMut::new();
    frontend::ssl_request(&mut buf);
    socket.write_all(&buf).map_err(|e| send_error(&e))?;
    let mut answer = [0u8; 1];
    loop {
        match socket.read(&mut answer) {
            Ok(0) => return Err(Refusal::from(CLOSED.to_string())),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Refusal::from(receive_error(&e))),
        }
    }
    match answer[0] {
        b'S' => {
            socket
                .set_read_timeout(plan.remaining()?)
                .map_err(|e| os_text(&e))?;
            let tls =
                tls::handshake(socket, &plan.host, ip, verify).map_err(|message| Refusal {
                    negotiation: true,
                    ..Refusal::from(message)
                })?;
            Ok(Stream::Tls(Box::new(tls)))
        }
        b'N' if required => Err(Refusal::from(
            "server does not support SSL, but SSL was required".to_string(),
        )),
        b'N' => Ok(Stream::Tcp(socket)),
        b'E' => Err(Refusal::from(
            "server sent an error response during SSL exchange".to_string(),
        )),
        c => Err(Refusal::from(format!(
            "received invalid response to SSL negotiation: {}",
            c as char
        ))),
    }
}

fn send_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset => CLOSED.into(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timeout expired".into(),
        _ => format!("could not send data to server: {}", os_text(e)),
    }
}

fn receive_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::UnexpectedEof => CLOSED.into(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timeout expired".into(),
        _ => format!("could not receive data from server: {}", os_text(e)),
    }
}

/// The fields of an error or a notice.
fn fields_of(mut it: postgres_protocol::message::backend::ErrorFields<'_>) -> Fields {
    let mut out = Vec::new();
    while let Ok(Some(f)) = it.next() {
        out.push((f.type_(), f.value_bytes().to_vec()));
    }
    Fields(out)
}

impl Link {
    fn send(&mut self, buf: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(buf)
            .and_then(|_| self.stream.flush())
            .map_err(|e| send_error(&e))
    }

    /// Reads until there is a whole message.
    fn next(&mut self) -> Result<In, String> {
        loop {
            if self.input.len() >= 5 {
                let tag = self.input[0];
                let len = u32::from_be_bytes([
                    self.input[1],
                    self.input[2],
                    self.input[3],
                    self.input[4],
                ]) as usize;
                if !(4..=0x4000_0000).contains(&len) {
                    return Err(desync(tag, len));
                }
                if self.input.len() > len {
                    match tag {
                        b'T' => {
                            let frame = self.input.split_to(len + 1);
                            return row_description(&frame[5..])
                                .map(In::Columns)
                                .ok_or_else(|| desync(tag, len));
                        }
                        b'W' => {
                            let _ = self.input.split_to(len + 1);
                            return Ok(In::CopyBoth);
                        }
                        b'R' if len >= 8 => {
                            let code = u32::from_be_bytes([
                                self.input[5],
                                self.input[6],
                                self.input[7],
                                self.input[8],
                            ]);
                            if !AUTHENTICATIONS.contains(&code) {
                                let _ = self.input.split_to(len + 1);
                                return Ok(In::Authentication(code));
                            }
                            match Message::parse(&mut self.input) {
                                Ok(Some(m)) => return Ok(In::Msg(m)),
                                Ok(None) => {}
                                Err(_) => return Err(desync(tag, len)),
                            }
                        }
                        _ => match Message::parse(&mut self.input) {
                            Ok(Some(m)) => return Ok(In::Msg(m)),
                            Ok(None) => {}
                            Err(_) => return Err(desync(tag, len)),
                        },
                    }
                }
            }
            if let Some(deadline) = self.deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("timeout expired".into());
                }
                self.stream
                    .set_timeout(Some(left))
                    .map_err(|e| os_text(&e))?;
            }
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(CLOSED.into()),
                Ok(n) => self.input.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(receive_error(&e)),
            }
        }
    }

    fn note(&mut self, name: &[u8], value: &[u8]) {
        match name {
            b"server_version" => self.server_version = server_version(value),
            b"client_encoding" => self.encoding = Encoding::of(value),
            b"standard_conforming_strings" => self.std_strings = value == b"on",
            _ => {}
        }
    }

    fn note_status(&mut self, m: &Message) {
        if let Message::ParameterStatus(body) = m {
            let name = body.name().unwrap_or("").as_bytes().to_vec();
            let value = body.value().unwrap_or("").as_bytes().to_vec();
            self.note(&name, &value);
        }
    }

    /// The start-up message, authentication and what the server says before it is ready.
    fn startup(&mut self, plan: &Plan) -> Result<(), Refusal> {
        let mut buf = BytesMut::new();
        put_startup(&mut buf, plan);
        self.send(&buf)?;
        let password = plan.options.password.clone().unwrap_or_default();
        let no_password = || Refusal::from("fe_sendauth: no password supplied".to_string());
        let mut scram: Option<sasl::ScramSha256> = None;
        loop {
            let m = self.next()?;
            let mut out = BytesMut::new();
            match m {
                In::Msg(Message::AuthenticationOk) => break,
                In::Msg(Message::AuthenticationCleartextPassword) => {
                    if password.is_empty() {
                        return Err(no_password());
                    }
                    let _ = frontend::password_message(&password, &mut out);
                }
                In::Msg(Message::AuthenticationMd5Password(body)) => {
                    if password.is_empty() {
                        return Err(no_password());
                    }
                    let hash = md5_hash(&plan.user, &password, body.salt());
                    let _ = frontend::password_message(hash.as_bytes(), &mut out);
                }
                In::Msg(Message::AuthenticationSasl(body)) => {
                    let mut mechanisms = body.mechanisms();
                    let mut scram_offered = false;
                    while let Ok(Some(m)) = mechanisms.next() {
                        scram_offered |= m == sasl::SCRAM_SHA_256;
                    }
                    if !scram_offered {
                        return Err(Refusal::from(
                            "none of the server's SASL authentication mechanisms are supported"
                                .to_string(),
                        ));
                    }
                    if password.is_empty() {
                        return Err(no_password());
                    }
                    let s = sasl::ScramSha256::new(&password, sasl::ChannelBinding::unsupported());
                    let _ =
                        frontend::sasl_initial_response(sasl::SCRAM_SHA_256, s.message(), &mut out);
                    scram = Some(s);
                }
                In::Msg(Message::AuthenticationSaslContinue(body)) => {
                    let s = scram.as_mut().ok_or_else(|| {
                        Refusal::from("unexpected SASL response from the server".to_string())
                    })?;
                    s.update(body.data())
                        .map_err(|e| Refusal::from(format!("malformed SCRAM message ({e})")))?;
                    let _ = frontend::sasl_response(s.message(), &mut out);
                }
                In::Msg(Message::AuthenticationSaslFinal(body)) => {
                    let s = scram.as_mut().ok_or_else(|| {
                        Refusal::from("unexpected SASL response from the server".to_string())
                    })?;
                    s.finish(body.data())
                        .map_err(|_| Refusal::from("incorrect server signature".to_string()))?;
                }
                In::Msg(
                    Message::AuthenticationGss
                    | Message::AuthenticationGssContinue(_)
                    | Message::AuthenticationSspi,
                ) => {
                    return Err(Refusal::from(
                        "fpkg's pg module does not implement GSSAPI or SSPI authentication"
                            .to_string(),
                    ));
                }
                In::Msg(
                    Message::AuthenticationKerberosV5 | Message::AuthenticationScmCredential,
                ) => {
                    return Err(Refusal::from(
                        "fpkg's pg module does not implement the authentication method the server asks for"
                            .to_string(),
                    ));
                }
                In::Msg(Message::ErrorResponse(body)) => {
                    let fields = fields_of(body.fields());
                    return Err(Refusal {
                        message: lossy(&text::message(&fields, None, self.encoding, true)),
                        by_server: true,
                        encrypted: false,
                        negotiation: false,
                    });
                }
                In::Msg(Message::NoticeResponse(_)) => {}
                In::Msg(m @ Message::ParameterStatus(_)) => self.note_status(&m),
                In::Authentication(code) => {
                    return Err(Refusal::from(format!(
                        "authentication method {code} not supported"
                    )));
                }
                _ => {
                    return Err(Refusal::from(
                        "expected authentication request from server, but received another message"
                            .to_string(),
                    ));
                }
            }
            if !out.is_empty() {
                self.send(&out)?;
            }
        }
        loop {
            match self.next()? {
                In::Msg(Message::ReadyForQuery(_)) => return Ok(()),
                In::Msg(Message::ErrorResponse(body)) => {
                    let fields = fields_of(body.fields());
                    return Err(Refusal {
                        message: lossy(&text::message(&fields, None, self.encoding, true)),
                        by_server: false,
                        encrypted: false,
                        negotiation: false,
                    });
                }
                In::Msg(m @ Message::ParameterStatus(_)) => self.note_status(&m),
                _ => {}
            }
        }
    }

    /// Runs a statement for a query of Lua.
    pub(crate) fn run(&mut self, statement: &Statement) -> Done {
        let mut buf = BytesMut::new();
        match statement.kind {
            Kind::Plain => put_message(&mut buf, b'Q', |b| put_cstr(b, &statement.text)),
            Kind::Parameterised => {
                if statement.params.len() > 65535 {
                    return Done {
                        error: b"number of parameters must be between 0 and 65535".to_vec(),
                        ..Done::default()
                    };
                }
                put_parse(&mut buf, b"", &statement.text);
                put_bind(&mut buf, b"", &statement.params);
                put_describe_execute_sync(&mut buf);
            }
            Kind::Prepared => {
                if statement.params.len() > 65535 {
                    return Done {
                        error: b"number of parameters must be between 0 and 65535".to_vec(),
                        ..Done::default()
                    };
                }
                put_bind(&mut buf, &statement.text, &statement.params);
                put_describe_execute_sync(&mut buf);
            }
        }
        let query = (statement.kind != Kind::Prepared).then_some(&statement.text[..]);
        self.exchange(&buf, query, statement.kind != Kind::Plain, false)
    }

    /// `PQexec` of a statement of the module's own: notices go nowhere.
    pub(crate) fn exec(&mut self, sql: &[u8]) -> Result<Set, Vec<u8>> {
        let mut buf = BytesMut::new();
        put_message(&mut buf, b'Q', |b| put_cstr(b, sql));
        let done = self.exchange(&buf, Some(sql), false, false);
        if done.success {
            Ok(done.set.unwrap_or_default())
        } else {
            Err(done.error)
        }
    }

    /// `PQprepare`: the statement under its name, or the server's error.
    pub(crate) fn prepare(&mut self, name: &[u8], definition: &[u8]) -> Result<(), Vec<u8>> {
        let mut buf = BytesMut::new();
        put_parse(&mut buf, name, definition);
        frontend::sync(&mut buf);
        let done = self.exchange(&buf, Some(definition), true, true);
        if done.success {
            Ok(())
        } else {
            Err(done.error)
        }
    }

    /// Sends what is in `buf` and reads what comes of it until the server is ready again.
    /// `query` is the text the cursor display of an error is about, `extended` whether it went
    /// with the extended protocol, and `prepare` whether it is a statement being prepared.
    fn exchange(
        &mut self,
        buf: &[u8],
        query: Option<&[u8]>,
        extended: bool,
        prepare: bool,
    ) -> Done {
        let mut out = Outcome::default();
        if let Err(why) = self.send(buf) {
            out.lose(&why);
            self.open = false;
            return out.conclude();
        }
        out.sent = true;
        let mut current: Option<Set> = None;
        loop {
            let m = match self.next() {
                Ok(m) => m,
                Err(why) => {
                    out.lose(&why);
                    self.open = false;
                    break;
                }
            };
            match m {
                In::Columns(columns) => {
                    current = Some(Set {
                        columns,
                        ..Set::default()
                    })
                }
                In::CopyBoth => {
                    out.refused = Some(REPLICATION);
                    out.lost = true;
                    self.open = false;
                    break;
                }
                In::Msg(Message::DataRow(row)) => {
                    if let Some(set) = current.as_mut() {
                        if !well_formed(&row, set.columns.len()) {
                            out.lose(&desync(b'D', row.buffer().len() + 6));
                            self.open = false;
                            break;
                        }
                        set.rows.push(row);
                    }
                }
                In::Authentication(code) => {
                    out.lose(&format!("authentication method {code} not supported"));
                    self.open = false;
                    break;
                }
                In::Msg(Message::CommandComplete(body)) => {
                    let mut set = current.take().unwrap_or_default();
                    set.tag = body.tag().unwrap_or("").as_bytes().to_vec();
                    out.last = Some(set);
                }
                In::Msg(Message::EmptyQueryResponse) => {
                    current = None;
                    out.last = Some(Set::default());
                }
                In::Msg(Message::ParseComplete) if prepare => out.last = Some(Set::default()),
                In::Msg(Message::ErrorResponse(body)) => {
                    current = None;
                    let fields = fields_of(body.fields());
                    let message = text::message(&fields, query, self.encoding, true);
                    out.fail(message, fields);
                }
                In::Msg(Message::NoticeResponse(body)) => {
                    let fields = fields_of(body.fields());
                    let message = text::message(&fields, query, self.encoding, false);
                    out.notices.push(Notice {
                        message: text::trimmed(&message).to_vec(),
                        fields,
                    });
                }
                In::Msg(Message::CopyInResponse(_)) => {
                    out.refused = Some(COPY_IN);
                    let mut b = BytesMut::new();
                    let _ = frontend::copy_fail(COPY_IN, &mut b);
                    if extended {
                        frontend::sync(&mut b);
                    }
                    if let Err(why) = self.send(&b) {
                        out.lose(&why);
                        self.open = false;
                        break;
                    }
                }
                In::Msg(Message::CopyOutResponse(_)) => out.refused = Some(COPY_OUT),
                In::Msg(m @ Message::ParameterStatus(_)) => self.note_status(&m),
                In::Msg(Message::ReadyForQuery(_)) => break,
                In::Msg(_) => {}
            }
        }
        out.conclude()
    }
}

/// Whether the fields of a row lie within its message, and are as many as the columns.
fn well_formed(row: &DataRowBody, columns: usize) -> bool {
    let mut ranges = row.ranges();
    let mut count = 0;
    loop {
        match ranges.next() {
            Ok(Some(_)) => count += 1,
            Ok(None) => return count == columns,
            Err(_) => return false,
        }
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(text::trimmed(b)).into_owned()
}

/// What libpq says when a message makes no sense.
fn desync(tag: u8, len: usize) -> String {
    format!(
        "lost synchronization with server: got message type \"{}\", length {len}",
        tag as char
    )
}

fn put_startup(buf: &mut BytesMut, plan: &Plan) {
    let at = buf.len();
    buf.put_i32(0);
    buf.put_i32(0x0003_0000);
    let mut param = |k: &[u8], v: &[u8]| {
        if !v.is_empty() {
            put_cstr(buf, k);
            put_cstr(buf, v);
        }
    };
    param(b"user", &plan.user);
    param(b"database", &plan.database);
    if let Some(o) = &plan.options.options {
        param(b"options", o);
    }
    if let Some(a) = &plan.options.application_name {
        param(b"application_name", a);
    }
    buf.put_u8(0);
    let len = (buf.len() - at) as i32;
    buf[at..at + 4].copy_from_slice(&len.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_versions_as_libpq_reads_them() {
        assert_eq!(server_version(b"18.1 (Debian 18.1-1.pgdg13+2)"), 180001);
        assert_eq!(server_version(b"14.19"), 140019);
        assert_eq!(server_version(b"9.6.1"), 90601);
        assert_eq!(server_version(b"9.6devel"), 90600);
        assert_eq!(server_version(b"19devel"), 190000);
        assert_eq!(server_version(b"x"), 0);
    }

    #[test]
    fn row_descriptions_with_names_of_any_bytes() {
        let mut body = vec![0, 2];
        for (name, oid) in [(&b"a\xe9"[..], 23u32), (b"", 25)] {
            body.extend_from_slice(name);
            body.push(0);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
            body.extend_from_slice(&oid.to_be_bytes());
            body.extend_from_slice(&[0; 8]);
        }
        let columns = row_description(&body).unwrap();
        assert_eq!(columns[0].name, b"a\xe9");
        assert_eq!(columns[0].type_oid, 23);
        assert_eq!(columns[1].type_oid, 25);
        assert!(row_description(&body[..body.len() - 1]).is_none());
    }

    #[test]
    fn the_conclusion_of_a_statement() {
        let mut lost = Outcome {
            sent: true,
            ..Outcome::default()
        };
        lost.fail(
            b"FATAL:  terminating connection due to administrator command\n".to_vec(),
            Fields(vec![(b'C', b"57P01".to_vec())]),
        );
        lost.lose(CLOSED);
        let done = lost.conclude();
        assert!(!done.success && done.lost);
        assert_eq!(
            String::from_utf8(done.error).unwrap(),
            format!("FATAL:  terminating connection due to administrator command\n{CLOSED}")
        );
        assert_eq!(done.details.unwrap().get(b'C'), Some(&b"57P01"[..]));

        let mut refused = Outcome {
            sent: true,
            refused: Some(COPY_IN),
            ..Outcome::default()
        };
        refused.fail(
            b"ERROR:  COPY from stdin failed\n".to_vec(),
            Fields::default(),
        );
        let done = refused.conclude();
        assert_eq!(done.error, COPY_IN.as_bytes());
        assert!(done.details.is_none());

        let fine = Outcome {
            sent: true,
            ..Outcome::default()
        }
        .conclude();
        assert!(fine.success && fine.set.is_some());
    }
}
