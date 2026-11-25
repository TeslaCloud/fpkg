//! The libpq connection string the module builds from the arguments of `connect` and its
//! `extra` text: `key=value` pairs read with libpq's rules (blanks around `=`, values in
//! single quotes with backslash escapes, unquoted values up to the next blank), the last
//! occurrence of a key winning.
//!
//! The keys this module acts on are `host`, `hostaddr`, `port`, `dbname`, `user`,
//! `password`, `sslmode`, `connect_timeout`, `application_name` and `options`. Another key that
//! libpq knows is an error that says this module does not implement it, and one that libpq does
//! not know is libpq's error.

/// What the connection string sets.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Options {
    pub host: Option<Vec<u8>>,
    pub hostaddr: Option<Vec<u8>>,
    pub port: Option<Vec<u8>>,
    pub dbname: Option<Vec<u8>>,
    pub user: Option<Vec<u8>>,
    pub password: Option<Vec<u8>>,
    pub sslmode: Option<Vec<u8>>,
    pub connect_timeout: Option<Vec<u8>>,
    pub application_name: Option<Vec<u8>>,
    pub options: Option<Vec<u8>>,
}

/// The keys of libpq's connection strings that this module does not act on.
const LIBPQ_ONLY: &[&str] = &[
    "service",
    "passfile",
    "channel_binding",
    "client_encoding",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "tcp_user_timeout",
    "sslnegotiation",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslcertmode",
    "sslpassword",
    "sslkeylogfile",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "require_auth",
    "min_protocol_version",
    "max_protocol_version",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "gssencmode",
    "krbsrvname",
    "gsslib",
    "gssdelegation",
    "replication",
    "target_session_attrs",
    "load_balance_hosts",
    "scram_client_key",
    "scram_server_key",
    "oauth_issuer",
    "oauth_client_id",
    "oauth_client_secret",
    "oauth_scope",
    "requiressl",
];

/// C's `isspace` in the C locale.
fn blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Reads a connection string, or says why it cannot be read, in libpq's words.
pub(crate) fn parse(text: &[u8]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut at = 0;
    while at < text.len() {
        if blank(text[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < text.len() && text[at] != b'=' && !blank(text[at]) {
            at += 1;
        }
        let name = String::from_utf8_lossy(&text[start..at]).into_owned();
        while at < text.len() && blank(text[at]) {
            at += 1;
        }
        if at >= text.len() || text[at] != b'=' {
            return Err(format!(
                "missing \"=\" after \"{name}\" in connection info string"
            ));
        }
        at += 1;
        while at < text.len() && blank(text[at]) {
            at += 1;
        }
        let mut value = Vec::new();
        if at < text.len() && text[at] == b'\'' {
            at += 1;
            loop {
                if at >= text.len() {
                    return Err("unterminated quoted string in connection info string".into());
                }
                match text[at] {
                    b'\\' => {
                        at += 1;
                        if at < text.len() {
                            value.push(text[at]);
                            at += 1;
                        }
                    }
                    b'\'' => {
                        at += 1;
                        break;
                    }
                    c => {
                        value.push(c);
                        at += 1;
                    }
                }
            }
        } else {
            while at < text.len() {
                let c = text[at];
                if blank(c) {
                    at += 1;
                    break;
                }
                if c == b'\\' {
                    at += 1;
                    if at < text.len() {
                        value.push(text[at]);
                        at += 1;
                    }
                } else {
                    value.push(c);
                    at += 1;
                }
            }
        }
        store(&mut options, &name, value)?;
    }
    Ok(options)
}

fn store(options: &mut Options, name: &str, value: Vec<u8>) -> Result<(), String> {
    let slot = match name {
        "host" => &mut options.host,
        "hostaddr" => &mut options.hostaddr,
        "port" => &mut options.port,
        "dbname" => &mut options.dbname,
        "user" => &mut options.user,
        "password" => &mut options.password,
        "sslmode" => &mut options.sslmode,
        "connect_timeout" => &mut options.connect_timeout,
        "application_name" => &mut options.application_name,
        "options" => &mut options.options,
        other if LIBPQ_ONLY.contains(&other) => {
            return Err(format!(
                "fpkg's pg module does not implement the connection option \"{other}\""
            ));
        }
        other => return Err(format!("invalid connection option \"{other}\"")),
    };
    *slot = Some(value);
    Ok(())
}

/// A value of the connection string in single quotes, with a backslash in front of a quote
/// and of a backslash, as the module writes the arguments of `connect`.
pub(crate) fn quoted(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 2);
    out.push(b'\'');
    for &c in value {
        if c == b'\'' || c == b'\\' {
            out.push(b'\\');
        }
        out.push(c);
    }
    out.push(b'\'');
    out
}

/// libpq's `pqParseIntParam`: a whole number that fits 32 bits, blanks around it allowed.
pub(crate) fn int_param(value: &[u8], option: &str) -> Result<i32, String> {
    let error = || {
        format!(
            "invalid integer value \"{}\" for connection option \"{option}\"",
            String::from_utf8_lossy(value)
        )
    };
    let mut at = 0;
    while at < value.len() && blank(value[at]) {
        at += 1;
    }
    let mut negative = false;
    if at < value.len() && (value[at] == b'+' || value[at] == b'-') {
        negative = value[at] == b'-';
        at += 1;
    }
    let digits = at;
    let mut n: i64 = 0;
    while at < value.len() && value[at].is_ascii_digit() {
        n = n
            .saturating_mul(10)
            .saturating_add((value[at] - b'0') as i64);
        at += 1;
    }
    if at == digits {
        return Err(error());
    }
    while at < value.len() && blank(value[at]) {
        at += 1;
    }
    if at != value.len() {
        return Err(error());
    }
    let n = if negative { -n } else { n };
    i32::try_from(n).map_err(|_| error())
}
