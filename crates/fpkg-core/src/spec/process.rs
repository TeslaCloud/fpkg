//! The process evaluator: each spec runs in a child (`fpkg __eval-spec`) under CPU,
//! memory, stack, wall-clock and output limits, so a hostile spec kills only it.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Env, Manifest, SpecError, SpecLocation};

const WALL_CLOCK: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const MEMORY_LIMIT: u64 = 512 * 1024 * 1024;
const CPU_SECONDS: u64 = 2;
const STACK_LIMIT: u64 = 64 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(5);

/// How specs are evaluated.
#[derive(Clone, Debug)]
pub enum Evaluator {
    /// A child process: `program args... <request JSON>`.
    Process { program: PathBuf, args: Vec<OsString> },
    /// This process. Only for trusted specs (fpkg's own tests) and inside the
    /// child itself: a hostile spec can crash or exhaust the process.
    InProcess,
}

static EVALUATOR: OnceLock<Evaluator> = OnceLock::new();

/// Chooses the evaluator for the rest of the process (the CLI passes itself
/// with `__eval-spec`). Returns false if one was already chosen.
pub fn set_evaluator(e: Evaluator) -> bool {
    EVALUATOR.set(e).is_ok()
}

/// The evaluator: the one set, else `$FPKG_EVALUATOR`, else an
/// `fpkg-eval-spec` next to the current executable (or one directory up, as
/// for test binaries in `target/debug/deps/`).
pub fn evaluator() -> Result<&'static Evaluator, SpecError> {
    if let Some(e) = EVALUATOR.get() {
        return Ok(e);
    }
    let found = std::env::var_os("FPKG_EVALUATOR").map(PathBuf::from).or_else(|| {
        let exe = std::env::current_exe().ok()?;
        let name = format!("fpkg-eval-spec{}", std::env::consts::EXE_SUFFIX);
        let dir = exe.parent()?;
        [dir.join(&name), dir.parent()?.join(&name)].into_iter().find(|p| p.is_file())
    });
    match found {
        Some(program) => {
            let _ = EVALUATOR.set(Evaluator::Process { program, args: Vec::new() });
            Ok(EVALUATOR.get().unwrap_or(&Evaluator::InProcess))
        }
        None => Err(SpecError::NoEvaluator),
    }
}

/// What the parent asks the child.
#[derive(Serialize, Deserialize)]
struct Request {
    location: SpecLocation,
    /// The file to read (normally `location`'s spec).
    file: PathBuf,
    env: Env,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Reply {
    Ok(Box<Manifest>),
    Err(SpecError),
}

/// The child's entry point: evaluates the request and prints the reply.
/// Returns the exit code (0 when a reply was written, whatever it says).
pub fn child_main(request: Option<&str>) -> i32 {
    let Some(request) = request else {
        eprintln!("usage: __eval-spec <request JSON>");
        return 2;
    };
    let req: Request = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("invalid request: {e}");
            return 2;
        }
    };
    let reply = match super::load_here(&req.location, req.env, &req.file) {
        Ok(m) => Reply::Ok(Box::new(m)),
        Err(e) => Reply::Err(e),
    };
    match serde_json::to_string(&reply) {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(e) => {
            eprintln!("cannot encode the reply: {e}");
            3
        }
    }
}

/// Evaluates in a child process.
pub(crate) fn run(program: &Path, args: &[OsString], loc: &SpecLocation, env: Env, file: &Path) -> Result<Manifest, SpecError> {
    let label = format!("{}{}", loc.rel_dir(), super::SPEC_FILE);
    let crashed = |status: String, stderr: &[u8]| {
        let text = String::from_utf8_lossy(stderr);
        let tail: Vec<&str> = text.lines().rev().take(5).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        SpecError::Crashed { file: label.clone(), status, stderr: tail.join("\n") }
    };
    let request = serde_json::to_string(&Request { location: loc.clone(), file: file.to_path_buf(), env })
        .map_err(|e| SpecError::Eval { file: label.clone(), message: e.to_string() })?;
    let mut cmd = Command::new(program);
    cmd.args(args).arg(request).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure runs between fork and exec and calls only
        // getrlimit and setrlimit, which are async-signal-safe.
        unsafe {
            cmd.pre_exec(apply_limits);
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| SpecError::Eval { file: label.clone(), message: format!("cannot start the evaluator {}: {e}", program.display()) })?;
    let overflow = Arc::new(AtomicBool::new(false));
    let out = child.stdout.take().map(|s| reader(s, MAX_OUTPUT, Some(overflow.clone())));
    let err = child.stderr.take().map(|s| reader(s, MAX_STDERR, None));
    let start = Instant::now();
    let mut killed: Option<String> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        let reason = if start.elapsed() > WALL_CLOCK {
            Some("was killed after 5 s of wall clock time".to_string())
        } else if overflow.load(Ordering::SeqCst) {
            Some("was killed for writing more than 1 MiB of output".to_string())
        } else if resident_bytes(child.id()).is_some_and(|r| r > MEMORY_LIMIT) {
            Some("was killed for using more than 512 MiB of memory".to_string())
        } else {
            None
        };
        if let Some(r) = reason {
            let _ = child.kill();
            let _ = child.wait();
            killed = Some(r);
            break None;
        }
        std::thread::sleep(POLL);
    };
    let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
    if let Some(reason) = killed {
        return Err(crashed(reason, &stderr));
    }
    let Some(status) = status else { return Err(crashed("could not be waited for".into(), &stderr)) };
    if !status.success() {
        return Err(crashed(describe(&status), &stderr));
    }
    match serde_json::from_slice::<Reply>(&stdout) {
        Ok(Reply::Ok(m)) => Ok(*m),
        Ok(Reply::Err(e)) => Err(e),
        Err(e) => Err(crashed(format!("gave an unreadable reply ({e})"), &stderr)),
    }
}

fn describe(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                libc::SIGABRT => "SIGABRT",
                libc::SIGSEGV => "SIGSEGV",
                libc::SIGBUS => "SIGBUS",
                libc::SIGKILL => "SIGKILL",
                libc::SIGXCPU => "SIGXCPU (more than 2 s of CPU time)",
                _ => "a signal",
            };
            return format!("was killed by {name} ({sig})");
        }
    }
    match status.code() {
        Some(c) => format!("exited with status {c}"),
        None => "exited abnormally".into(),
    }
}

fn reader<R: Read + Send + 'static>(mut r: R, cap: usize, overflow: Option<Arc<AtomicBool>>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() + n > cap {
                        if let Some(o) = &overflow {
                            o.store(true, Ordering::SeqCst);
                        }
                        // Keep draining so the child is not blocked on a full pipe.
                        continue;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        buf
    })
}

#[cfg(unix)]
fn apply_limits() -> std::io::Result<()> {
    fn set(resource: libc::c_int, soft: u64, hard: Option<u64>) {
        // SAFETY: getrlimit and setrlimit write and read the struct given.
        unsafe {
            let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if libc::getrlimit(resource as _, &mut cur) != 0 {
                return;
            }
            let max = cur.rlim_max;
            let clamp = |v: u64| if max == libc::RLIM_INFINITY { v as libc::rlim_t } else { (v as libc::rlim_t).min(max) };
            let new = libc::rlimit { rlim_cur: clamp(soft), rlim_max: hard.map(clamp).unwrap_or(max) };
            // Best effort: macOS refuses RLIMIT_AS, and the watchdog covers it.
            let _ = libc::setrlimit(resource as _, &new);
        }
    }
    set(libc::RLIMIT_CPU as _, CPU_SECONDS, Some(CPU_SECONDS + 1));
    set(libc::RLIMIT_AS as _, MEMORY_LIMIT, Some(MEMORY_LIMIT));
    set(libc::RLIMIT_STACK as _, STACK_LIMIT, None);
    set(libc::RLIMIT_CORE as _, 0, None);
    Ok(())
}

/// The resident size of a process, where the platform says.
#[cfg(target_os = "macos")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTASKINFO, 0, (&mut info as *mut libc::proc_taskinfo).cast(), size) };
    (n == size).then_some(info.pti_resident_size)
}

#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * page.max(4096) as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Evaluates many specs, a few child processes at a time; results in order.
pub(crate) fn run_many(locs: &[SpecLocation], env: Env) -> Vec<Result<Manifest, SpecError>> {
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(locs.len().max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Manifest, SpecError>>>> = Mutex::new((0..locs.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(loc) = locs.get(i) else { break };
                    let r = super::load(loc, env);
                    if let Ok(mut all) = results.lock() {
                        all[i] = Some(r);
                    }
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.unwrap_or(Err(SpecError::NoEvaluator)))
        .collect()
}
