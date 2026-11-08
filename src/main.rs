use anyhow::{anyhow, Result};
use herdr_music::app::Controller;
use herdr_music::cli::{self, AuthCmd, Requested};
use herdr_music::config::{self, AppConfig, ProviderKind};
use herdr_music::provider::{self, MusicProvider};
use herdr_music::spotify;
use herdr_music::tui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    match cli::parse(args) {
        Err(msg) => {
            eprintln!("{msg}");
            2
        }
        Ok(Requested::Help) => {
            println!("{}", cli::USAGE);
            0
        }
        Ok(Requested::Open) => open_pane(),
        Ok(req) => match dispatch(req) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e:#}");
                1
            }
        },
    }
}

fn dispatch(req: Requested) -> Result<()> {
    match req {
        Requested::Tui => match AppConfig::load() {
            Ok(cfg) => {
                let provider = provider::from_config(&cfg)?;
                tui::run(provider)
            }
            // A config error lands on the TUI status line — the pane is
            // the one place a plugin can show it to the user.
            Err(msg) => tui::run(Box::new(provider::ErrorProvider::new(msg))),
        },
        Requested::Status => status(),
        Requested::Ctl(name) => ctl(&name),
        Requested::Auth(AuthCmd::Login(port)) => auth_login(port),
        Requested::Auth(AuthCmd::Status) => {
            // Auth stays Spotify-only under every provider — it manages
            // the Spotify tokens, not a pane session.
            AppConfig::load().map_err(|e| anyhow!(e))?;
            match spotify::connection_status(&config::state_dir())? {
                true => println!("spotify: connected"),
                false => println!("spotify: not connected — run `herdr-music auth login`"),
            }
            Ok(())
        }
        Requested::Auth(AuthCmd::Logout) => {
            AppConfig::load().map_err(|e| anyhow!(e))?;
            spotify::logout(&config::state_dir())?;
            println!("spotify: disconnected");
            Ok(())
        }
        Requested::Open => Ok(()),
        Requested::Help => unreachable!("handled in run()"),
    }
}

/// The `open` action: ask Herdr to open the player pane, propagating its
/// exit status so the action reports failure honestly.
fn open_pane() -> i32 {
    open_pane_with(std::env::var("HERDR_BIN_PATH").ok(), |cmd| {
        cmd.status().map(|status| status.code().unwrap_or(1))
    })
}

/// `open` split from its environment for tests: `herdr` is `HERDR_BIN_PATH`
/// (`None` when unset) and `run` executes the built command.
fn open_pane_with(
    herdr: Option<String>,
    run: impl Fn(&mut std::process::Command) -> std::io::Result<i32>,
) -> i32 {
    let Some(herdr) = herdr.filter(|path| !path.is_empty()) else {
        eprintln!("HERDR_BIN_PATH is not set — `open` only works inside Herdr");
        return 1;
    };
    let (program, args) = cli::open_pane_command(&herdr);
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    match run(&mut cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("failed to launch `{herdr}`: {e}");
            1
        }
    }
}

/// `ctl`/`status` run in a separate process and can only reach Spotify.
/// The AirPlay receiver lives inside the pane's TUI process, so under
/// `provider = "airplay"` those commands explain that instead of
/// silently talking to Spotify. `auth` is unaffected: it manages the
/// Spotify token file, which exists under either provider.
fn headless_config() -> Result<AppConfig> {
    let cfg = AppConfig::load().map_err(|e| anyhow!(e))?;
    if cfg.provider == ProviderKind::AirPlay {
        anyhow::bail!(
            "this command is not supported under provider airplay — \
             the AirPlay receiver runs inside the player pane"
        );
    }
    Ok(cfg)
}

fn provider() -> Result<spotify::SpotifyProvider> {
    Ok(spotify::provider_from_config(&headless_config()?)?)
}

fn status() -> Result<()> {
    match provider()?.playback()? {
        Some(pb) => {
            let state = if pb.is_playing { "playing" } else { "paused" };
            println!(
                "{} — {} · {} [{}]",
                pb.track.title,
                pb.track.artists.join(", "),
                pb.track.album,
                state
            );
        }
        None => println!("nothing playing"),
    }
    Ok(())
}

fn ctl(name: &str) -> Result<()> {
    let cmd = cli::ctl_command(name).ok_or_else(|| anyhow!("unknown ctl command `{name}`"))?;
    let provider = provider()?;
    let mut controller = Controller::new();
    controller.handle(cmd, &provider);
    match controller.status() {
        Some(msg) => Err(anyhow!(msg.to_string())),
        None => {
            println!("{name}: ok");
            Ok(())
        }
    }
}

fn auth_login(port: Option<u16>) -> Result<()> {
    let client_id = AppConfig::load()
        .map_err(|e| anyhow!(e))?
        .spotify_client_id
        .ok_or_else(|| {
            anyhow!(
                "spotify client id not configured — set HERDR_MUSIC_SPOTIFY_CLIENT_ID \
             or add spotify_client_id to {}",
                config::config_dir().join("config.toml").display()
            )
        })?;
    spotify::login(
        &client_id,
        &config::state_dir(),
        port.unwrap_or(spotify::auth::DEFAULT_REDIRECT_PORT),
    )?;
    println!("spotify: connected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pane_fails_without_herdr_bin_path() {
        let no_spawn = |_: &mut std::process::Command| -> std::io::Result<i32> {
            unreachable!("must not spawn")
        };
        assert_eq!(open_pane_with(None, no_spawn), 1);
        assert_eq!(open_pane_with(Some(String::new()), no_spawn), 1);
    }

    #[test]
    fn open_pane_propagates_herdr_exit_status() {
        assert_eq!(open_pane_with(Some("/bin/herdr".into()), |_| Ok(0)), 0);
        assert_eq!(open_pane_with(Some("/bin/herdr".into()), |_| Ok(7)), 7);
    }

    #[test]
    fn open_pane_reports_spawn_failure() {
        let code = open_pane_with(Some("/bin/herdr".into()), |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no herdr",
            ))
        });
        assert_eq!(code, 1);
    }

    // Config-dir env var + temp file fixtures for headless dispatch tests.
    // Tests mutate process env, so they serialize on this lock.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct AirplayConfig {
        dir: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl AirplayConfig {
        fn new(name: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap();
            let dir =
                std::env::temp_dir().join(format!("herdr-music-{name}-{}", std::process::id()));
            let state = dir.join("state");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(dir.join("config.toml"), "provider = \"airplay\"\n").unwrap();
            std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);
            std::env::set_var("HERDR_PLUGIN_STATE_DIR", &state);
            Self { dir, _guard: guard }
        }
    }

    impl Drop for AirplayConfig {
        fn drop(&mut self) {
            std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR");
            std::env::remove_var("HERDR_PLUGIN_STATE_DIR");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn headless_commands_fail_under_airplay_provider() {
        let _cfg = AirplayConfig::new("headless");
        for req in [Requested::Status, Requested::Ctl("next".into())] {
            let name = format!("{req:?}");
            let err = dispatch(req).unwrap_err().to_string();
            assert!(
                err.contains("not supported under provider airplay"),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn auth_stays_spotify_only_under_airplay_provider() {
        // `auth` manages the Spotify token file, not a pane session, so
        // the airplay guard must not block it. With no tokens stored it
        // reports "not connected" and exits 0.
        let _cfg = AirplayConfig::new("auth");
        dispatch(Requested::Auth(AuthCmd::Status)).unwrap();
        assert_eq!(run(&["auth".into(), "status".into()]), 0);
    }

    #[test]
    fn run_status_exits_1_under_airplay_provider() {
        let _cfg = AirplayConfig::new("run");
        assert_eq!(run(&["status".into()]), 1);
        assert_eq!(run(&["ctl".into(), "play-pause".into()]), 1);
    }

    #[test]
    fn unknown_provider_value_is_a_load_error() {
        let guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("herdr-music-badcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "provider = \"sonos\"\n").unwrap();
        std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);
        let err = dispatch(Requested::Status).unwrap_err().to_string();
        assert!(err.contains("sonos"), "{err}");
        std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        drop(guard);
    }
}
