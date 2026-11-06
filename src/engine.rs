//! Provider-agnostic child-process management for a local playback engine.
//! Spawns the engine with captured stderr (a bounded ring of last lines),
//! optionally pipes its stdout into an audio sink, writes a pidfile so a
//! crashed pane's engine can be reaped on the next start, and kills the
//! child on drop. Uses `std::process` plus OS command-line tools — no new
//! crate dependencies.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const STDERR_RING: usize = 20;
const TERM_GRACE: Duration = Duration::from_secs(2);

/// One command to spawn: program, argv, extra environment.
pub struct Cmd<'a> {
    pub program: &'a Path,
    pub args: &'a [String],
    pub envs: &'a [(String, String)],
}

/// Everything `Engine::spawn` needs. `sink` is an optional command whose
/// stdin receives the engine's stdout (the Linux PCM pipe).
pub struct Spawn<'a> {
    pub engine: Cmd<'a>,
    pub sink: Option<Cmd<'a>>,
    pub pidfile: &'a Path,
}

/// A running engine plus its optional audio sink. Kills both on drop.
pub struct Engine {
    child: Child,
    sink: Option<Child>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// "Browse to: <url>" scraped from the engine's output — librespot's
    /// `--enable-oauth` prints its sign-in URL this way.
    oauth_url: Arc<Mutex<Option<String>>>,
    pidfile: PathBuf,
}

impl Engine {
    /// Reap a stale engine from a crashed pane, then spawn.
    pub fn spawn(opts: Spawn<'_>) -> std::io::Result<Self> {
        reap_stale(opts.pidfile, expected_name(opts.engine.program));
        let mut sink = match opts.sink {
            Some(cmd) => Some(
                Command::new(cmd.program)
                    .args(cmd.args)
                    .envs(cmd.envs.iter().cloned())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            ),
            None => None,
        };
        let mut child = Command::new(opts.engine.program)
            .args(opts.engine.args)
            .envs(opts.engine.envs.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_RING)));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut ring = tail.lock().unwrap();
                    if ring.len() == STDERR_RING {
                        ring.pop_front();
                    }
                    ring.push_back(line);
                }
            });
        }
        // stdout pump: bytes flow to the sink's stdin when there is one;
        // either way, scan for the engine's "Browse to:" OAuth URL.
        let oauth_url = Arc::new(Mutex::new(None));
        if let Some(mut out) = child.stdout.take() {
            let mut sink_in = sink.as_mut().and_then(|s| s.stdin.take());
            let url_slot = oauth_url.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let mut scan = Vec::new();
                loop {
                    match out.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Some(stdin) = sink_in.as_mut() {
                                if stdin.write_all(&buf[..n]).is_err() {
                                    sink_in = None;
                                }
                            }
                            if url_slot.lock().unwrap().is_none() {
                                scan.extend_from_slice(&buf[..n]);
                                if scan.len() > 65536 {
                                    scan.drain(..scan.len() / 2);
                                }
                                if let Some(url) = extract_oauth_url(&scan) {
                                    *url_slot.lock().unwrap() = Some(url);
                                }
                            }
                        }
                    }
                }
            });
        }
        write_pidfile(opts.pidfile, std::process::id(), child.id());
        Ok(Self {
            child,
            sink,
            stderr_tail,
            oauth_url,
            pidfile: opts.pidfile.to_path_buf(),
        })
    }

    /// The engine's process id.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// True when the engine process has exited (crash or clean exit).
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// The engine's "Browse to:" OAuth URL, once printed.
    pub fn oauth_url(&self) -> Option<String> {
        self.oauth_url.lock().unwrap().clone()
    }

    /// The engine's last stderr line — usually its error message.
    pub fn last_error_line(&self) -> Option<String> {
        self.stderr_tail
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
    }

    /// SIGTERM, then SIGKILL after a grace period. Windows goes straight
    /// to TerminateProcess — it has no SIGTERM.
    pub fn stop(&mut self) {
        let _ = terminate(self.child.id());
        let deadline = Instant::now() + TERM_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    break;
                }
            }
        }
        let _ = self.child.wait();
        if let Some(mut sink) = self.sink.take() {
            // The sink exits on EOF when the engine's stdout closes — give
            // it a moment to drain, then kill it outright.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match sink.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = sink.kill();
                        break;
                    }
                }
            }
            let _ = sink.wait();
        }
        let _ = std::fs::remove_file(&self.pidfile);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `kill -TERM`/`taskkill` without the `libc` crate.
fn terminate(pid: u32) -> std::io::Result<()> {
    if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
    } else {
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
    }
    Ok(())
}

fn force_kill(pid: u32) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Whether `pid` names a live process — exposed for integration tests.
pub fn pid_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")),
            Err(_) => false,
        }
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// The process's image name (file stem only), for matching an expected
/// engine binary. None when the pid is gone or the lookup fails.
fn pid_name(pid: u32) -> Option<String> {
    let out = if cfg!(windows) {
        let o = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&o.stdout);
        // First CSV field is the image name, in quotes.
        text.trim_start()
            .strip_prefix('"')?
            .split('"')
            .next()?
            .to_string()
    } else {
        let o = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    Path::new(&out)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
}

fn expected_name(program: &Path) -> String {
    program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn write_pidfile(path: &Path, owner: u32, engine: u32) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{owner}\n{engine}\n"));
}

/// If a pidfile names an engine whose owner is dead, kill the engine.
/// The pid must still belong to a process of the expected name — pids
/// get reused, and killing a recycled pid would hit an unrelated process.
fn reap_stale(pidfile: &Path, engine_name: String) {
    let Ok(text) = std::fs::read_to_string(pidfile) else {
        return;
    };
    let mut lines = text.lines();
    let (Some(owner), Some(engine)) = (
        lines.next().and_then(|l| l.trim().parse::<u32>().ok()),
        lines.next().and_then(|l| l.trim().parse::<u32>().ok()),
    ) else {
        return;
    };
    if pid_alive(owner) {
        // Another pane owns this engine. Reaping it would kill the other
        // pane's audio, so the stale pidfile is left — this assumes Herdr
        // keeps one player pane at a time (a second pane's engine fails
        // registration on the duplicate device name anyway).
        return;
    }
    if pid_alive(engine) && pid_name(engine).map(|n| names_match(&n, &engine_name)) == Some(true) {
        force_kill(engine);
    }
    let _ = std::fs::remove_file(pidfile);
}

/// Pull `Browse to: <url>` out of a byte stream chunk.
fn extract_oauth_url(buf: &[u8]) -> Option<String> {
    let pos = buf.windows(11).position(|w| w == b"Browse to: ")? + 11;
    let end = buf[pos..]
        .iter()
        .position(|b| *b == b'\n' || *b == b'\r')
        .map(|e| pos + e)?;
    let url = String::from_utf8_lossy(&buf[pos..end]).trim().to_string();
    url.starts_with("http").then_some(url)
}

/// `ps comm`/`tasklist` truncate long names, so accept a prefix match in
/// either direction rather than exact equality.
fn names_match(reported: &str, expected: &str) -> bool {
    !expected.is_empty()
        && !reported.is_empty()
        && (expected.starts_with(reported) || reported.starts_with(expected))
}
