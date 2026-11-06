//! The Spotify-specific half of local playback: how to invoke librespot.
//! The generic child lifecycle lives in `crate::engine`; this module only
//! knows flags, paths, and which OS gets which backend.
use std::path::{Path, PathBuf};

/// Pinned in herdr-plugin.toml's `[[build]]` entries; checked against the
/// binary at start so an upgrade surfaces a clear message, not argv errors.
pub const ENGINE_VERSION: &str = "0.8.0";
const ENGINE_ENV: &str = "HERDR_MUSIC_ENGINE";

/// Port for the engine's OAuth redirect catcher (127.0.0.1 only).
pub const OAUTH_PORT: u16 = 8877;

/// The credentials file the engine's OAuth writes into `cache_dir`.
pub fn credentials_path(cache_dir: &std::path::Path) -> PathBuf {
    cache_dir.join("credentials.json")
}

/// librespot argv for `os` (a `std::env::consts::OS` value). Discovery
/// isn't built into our binary, so no flag disables it. Linux writes PCM
/// to stdout for the sink to consume; macOS/Windows play through rodio
/// (CoreAudio/WASAPI).
pub fn engine_argv(os: &str, name: &str, cache_dir: &std::path::Path) -> Vec<String> {
    let mut args = vec![
        "--name".to_string(),
        name.to_string(),
        "--device-type".to_string(),
        "computer".to_string(),
        "--enable-oauth".to_string(),
        "--oauth-port".to_string(),
        OAUTH_PORT.to_string(),
        "--system-cache".to_string(),
        cache_dir.to_string_lossy().into_owned(),
        "--disable-audio-cache".to_string(),
    ];
    if os == "linux" {
        args.extend([
            "--backend".into(),
            "pipe".into(),
            "--device".into(),
            "-".into(),
        ]);
    } else {
        args.extend(["--backend".into(), "rodio".into()]);
    }
    args
}

/// The engine binary: `HERDR_MUSIC_ENGINE` override (the test hook), else
/// `target/librespot/bin/librespot` next to the running executable's
/// `target/release` directory, with the platform's `.exe` suffix.
pub fn engine_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var(ENGINE_ENV).ok().filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let target_dir = exe.parent()?.parent()?;
    Some(
        target_dir
            .join("librespot")
            .join("bin")
            .join(format!("librespot{}", std::env::consts::EXE_SUFFIX)),
    )
}

/// A fully-resolved engine spawn: program, argv, optional Linux audio
/// sink, the pidfile location, and whether a browser visit is needed to
/// mint engine credentials (`oauth_pending`).
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub sink: Option<(PathBuf, Vec<String>)>,
    pub pidfile: PathBuf,
    pub oauth_pending: bool,
}

/// Build the invocation for a real start. `Err(reason)` is a *disabled*
/// reason — the adapter turns it into `LocalStatus::Disabled`, not an
/// error.
pub fn invocation(name: &str, state_dir: &Path) -> Result<Invocation, String> {
    let program = engine_path()
        .filter(|p| p.is_file())
        .ok_or("local engine not installed — re-run `herdr plugin install`".to_string())?;
    // The env override is a test/dev hook; it may point at anything, so
    // the version check only applies to the installed binary.
    if std::env::var(ENGINE_ENV)
        .ok()
        .filter(|p| !p.is_empty())
        .is_none()
    {
        check_version(&program)?;
    }
    let cache_dir = state_dir.join("engine-cache");
    let sink = if std::env::consts::OS == "linux" {
        Some(
            pick_sink(on_path)
                .ok_or("no audio player found — install pw-play, paplay, or aplay".to_string())?,
        )
    } else {
        None
    };
    Ok(Invocation {
        program,
        args: engine_argv(std::env::consts::OS, name, &cache_dir),
        sink,
        pidfile: state_dir.join("engine.pid"),
        oauth_pending: !credentials_path(&cache_dir).exists(),
    })
}

/// `librespot --version` must report the pinned version, so a stale or
/// hand-replaced binary fails loudly instead of on a changed flag.
pub fn check_version(path: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .output()
        .map_err(|_| "local engine not installed — re-run `herdr plugin install`".to_string())?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    text.contains(ENGINE_VERSION)
        .then_some(())
        .ok_or_else(|| "engine version mismatch — re-run `herdr plugin install`".to_string())
}

/// Audio players tried in order on Linux; each entry is the program plus
/// the raw-format args matching librespot's PCM output (S16 LE, 44.1 kHz,
/// stereo). The engine's stdout goes to the player's stdin.
const SINKS: &[(&str, &[&str])] = &[
    (
        "pw-play",
        &["--raw", "--rate=44100", "--channels=2", "--format=s16", "-"],
    ),
    (
        "paplay",
        &["--raw", "--rate=44100", "--channels=2", "--format=s16le"],
    ),
    ("aplay", &["-f", "S16_LE", "-c", "2", "-r", "44100"]),
];

/// First available audio player from `SINKS`, resolved to its full path
/// so spawn can't pick a different binary than the check accepted;
/// `resolve` is the PATH lookup (injected for tests). None means no
/// player is installed.
pub fn pick_sink(resolve: impl Fn(&str) -> Option<PathBuf>) -> Option<(PathBuf, Vec<String>)> {
    SINKS.iter().find_map(|(name, args)| {
        resolve(name).map(|path| (path, args.iter().map(|s| s.to_string()).collect()))
    })
}

/// PATH lookup used by `pick_sink` in production: the full path of the
/// first executable match.
pub fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
            is_executable(&full).then_some(full)
        })
    })
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(os: &str) -> Vec<String> {
        engine_argv(os, "Herdr Music", std::path::Path::new("/cache"))
    }

    #[test]
    fn argv_carries_name_oauth_and_cache() {
        for os in ["linux", "macos", "windows"] {
            let a = argv(os);
            let pos = |flag: &str| a.iter().position(|x| x == flag).unwrap();
            assert_eq!(a[pos("--name") + 1], "Herdr Music");
            assert_eq!(a[pos("--device-type") + 1], "computer");
            assert_eq!(a[pos("--oauth-port") + 1], OAUTH_PORT.to_string());
            assert_eq!(a[pos("--system-cache") + 1], "/cache");
            assert!(a.contains(&"--enable-oauth".to_string()));
            assert!(a.contains(&"--disable-audio-cache".to_string()));
            // Credentials must be cached — the engine's OAuth mints them.
            assert!(!a.contains(&"--disable-credential-cache".to_string()));
        }
    }

    #[test]
    fn argv_picks_backend_per_os() {
        let linux = argv("linux");
        assert!(linux.windows(2).any(|w| w == ["--backend", "pipe"]));
        assert!(linux.windows(2).any(|w| w == ["--device", "-"]));
        for os in ["macos", "windows"] {
            assert!(argv(os).windows(2).any(|w| w == ["--backend", "rodio"]));
        }
    }

    #[test]
    fn sink_prefers_pw_play_then_paplay_then_aplay() {
        let found = |n: &str| Some(PathBuf::from(format!("/usr/bin/{n}")));
        assert_eq!(
            pick_sink(|n| (n == "pw-play").then(|| found(n).unwrap()))
                .unwrap()
                .0,
            PathBuf::from("/usr/bin/pw-play")
        );
        assert_eq!(
            pick_sink(|n| (n == "paplay").then(|| found(n).unwrap()))
                .unwrap()
                .0,
            PathBuf::from("/usr/bin/paplay")
        );
        assert_eq!(
            pick_sink(|n| (n == "aplay").then(|| found(n).unwrap()))
                .unwrap()
                .0,
            PathBuf::from("/usr/bin/aplay")
        );
        assert!(pick_sink(|_| None).is_none());
    }
}
