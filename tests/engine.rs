use herdr_music::engine::{pid_alive, Cmd, Engine, Spawn};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Fake engine: this same test executable re-invoked as
/// `engine-<hash> fake_engine --exact --nocapture` with a mode in
/// `HERDR_MUSIC_FAKE_ENGINE`. A trivial pass when run as part of the
/// suite; the env-gated behavior only activates in the spawned copy.
#[test]
fn fake_engine() {
    let Ok(mode) = std::env::var("HERDR_MUSIC_FAKE_ENGINE") else {
        return;
    };
    if let Ok(path) = std::env::var("HERDR_MUSIC_FAKE_ARGV_FILE") {
        let args: Vec<String> = std::env::args().collect();
        std::fs::write(path, args.join("\n")).ok();
    }
    match mode.as_str() {
        "exit-early" => {
            eprintln!("boom: something failed");
            std::process::exit(1);
        }
        "echo" => {
            println!("pcm-bytes");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            park();
        }
        "sink" => {
            use std::io::Read;
            let mut data = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut data);
            if let Ok(path) = std::env::var("HERDR_MUSIC_FAKE_SINK_FILE") {
                std::fs::write(path, data).ok();
            }
        }
        _ => park(),
    }
}

fn park() -> ! {
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn state_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hm-engine-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_cmd(
    mode: &str,
    extra_envs: &[(&str, &str)],
) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    let mut envs = vec![("HERDR_MUSIC_FAKE_ENGINE".to_string(), mode.to_string())];
    envs.extend(
        extra_envs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string())),
    );
    (
        std::env::current_exe().unwrap(),
        vec![
            "fake_engine".to_string(),
            "--exact".to_string(),
            "--nocapture".to_string(),
        ],
        envs,
    )
}

fn spawn_fake(
    mode: &str,
    pidfile: &Path,
    extra_envs: &[(&str, &str)],
    sink: Option<Cmd<'_>>,
) -> Engine {
    let (program, args, envs) = fake_cmd(mode, extra_envs);
    Engine::spawn(Spawn {
        engine: Cmd {
            program: &program,
            args: &args,
            envs: &envs,
        },
        sink,
        pidfile,
    })
    .unwrap()
}

/// Wait up to `timeout` for `f` to become true.
fn until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn engine_runs_until_dropped_and_pidfile_is_cleaned() {
    let dir = state_dir("drop");
    let pidfile = dir.join("engine.pid");
    let pid;
    {
        let mut engine = spawn_fake("sleep", &pidfile, &[], None);
        pid = engine.id();
        assert!(!engine.exited());
        let text = std::fs::read_to_string(&pidfile).unwrap();
        assert!(text.contains(&pid.to_string()), "pidfile: {text}");
    } // drop → stop()
    assert!(
        until(Duration::from_secs(5), || !pid_alive(pid)),
        "engine pid {pid} still alive"
    );
    assert!(!pidfile.exists());
}

#[test]
fn early_exit_reports_the_stderr_line() {
    let dir = state_dir("early");
    let pidfile = dir.join("engine.pid");
    let mut engine = spawn_fake("exit-early", &pidfile, &[], None);
    assert!(until(Duration::from_secs(5), || engine.exited()));
    assert_eq!(
        engine.last_error_line().as_deref(),
        Some("boom: something failed")
    );
}

#[test]
fn stop_terminates_within_the_grace_deadline() {
    let dir = state_dir("stop");
    let pidfile = dir.join("engine.pid");
    let mut engine = spawn_fake("sleep", &pidfile, &[], None);
    let pid = engine.id();
    let start = Instant::now();
    engine.stop();
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "stop took {:?}",
        start.elapsed()
    );
    assert!(until(Duration::from_secs(3), || !pid_alive(pid)));
}

#[test]
fn engine_receives_the_args_it_was_spawned_with() {
    let dir = state_dir("argv");
    let argv_file = dir.join("argv.txt");
    let pidfile = dir.join("engine.pid");
    let mut engine = spawn_fake(
        "sleep",
        &pidfile,
        &[("HERDR_MUSIC_FAKE_ARGV_FILE", argv_file.to_str().unwrap())],
        None,
    );
    assert!(until(Duration::from_secs(5), || argv_file.exists()));
    let got = std::fs::read_to_string(&argv_file).unwrap();
    for arg in ["fake_engine", "--exact", "--nocapture"] {
        assert!(got.contains(arg), "argv file: {got}");
    }
    engine.stop();
}

#[test]
fn engine_stdout_reaches_the_sink_stdin() {
    let dir = state_dir("sink");
    let sink_file = dir.join("sink.txt");
    let pidfile = dir.join("engine.pid");
    let (sink_program, sink_args, sink_envs) = fake_cmd(
        "sink",
        &[("HERDR_MUSIC_FAKE_SINK_FILE", sink_file.to_str().unwrap())],
    );
    let mut engine = spawn_fake(
        "echo",
        &pidfile,
        &[],
        Some(Cmd {
            program: &sink_program,
            args: &sink_args,
            envs: &sink_envs,
        }),
    );
    std::thread::sleep(Duration::from_millis(300));
    engine.stop(); // engine EOF ends the sink, which writes the file
    assert!(until(Duration::from_secs(5), || sink_file.exists()));
    let got = std::fs::read_to_string(&sink_file).unwrap();
    // The spawned copy's stdout carries harness noise; the payload got
    // through if our marker is in it.
    assert!(got.contains("pcm-bytes"), "sink file: {got}");
}

#[test]
fn stale_engine_is_reaped_when_its_owner_died() {
    let dir = state_dir("reap");
    let pidfile = dir.join("engine.pid");

    // A live engine and a real dead pid standing in for the crashed owner.
    let mut stale = spawn_fake("sleep", &pidfile, &[], None);
    let stale_pid = stale.id();
    let mut dead = Command::new(std::env::current_exe().unwrap())
        .args(["fake_engine", "--exact", "--nocapture"])
        .env("HERDR_MUSIC_FAKE_ENGINE", "exit-early")
        .spawn()
        .unwrap();
    dead.wait().unwrap();
    let dead_pid = dead.id();
    assert!(!pid_alive(dead_pid));

    // Forge the stale pidfile: dead owner + live engine.
    std::fs::write(&pidfile, format!("{dead_pid}\n{stale_pid}\n")).unwrap();

    // The next spawn must kill the orphan before starting its own engine.
    // (Check via try_wait, not kill -0: we own the Child, so a killed
    // engine would linger as a zombie until reaped.)
    let mut fresh = spawn_fake("sleep", &pidfile, &[], None);
    assert!(
        until(Duration::from_secs(5), || stale.exited()),
        "stale engine {stale_pid} survived the reap"
    );
    assert!(pid_alive(fresh.id()));
    fresh.stop();
}

/// A stale pidfile pointing at a recycled pid that isn't our engine must
/// NOT kill that process — pids get reused.
#[cfg(unix)]
#[test]
fn stale_pidfile_never_kills_a_recycled_pid() {
    let dir = state_dir("recycle");
    let pidfile = dir.join("engine.pid");
    let mut bystander = Command::new("sleep").arg("30").spawn().unwrap();
    let bystander_pid = bystander.id();
    // Owner pid that is definitely dead: the bystander's +1 is still real
    // risk-free only if unused; use a short-lived already-exited child.
    let mut dead = Command::new("sleep").arg("0").spawn().unwrap();
    dead.wait().unwrap();
    std::fs::write(&pidfile, format!("{}\n{bystander_pid}\n", dead.id())).unwrap();

    let mut engine = spawn_fake("sleep", &pidfile, &[], None);
    std::thread::sleep(Duration::from_millis(300));
    assert!(pid_alive(bystander_pid), "reaper killed an unrelated pid");
    bystander.kill().unwrap();
    engine.stop();
}
