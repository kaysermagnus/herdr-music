use anyhow::{anyhow, Result};
use herdr_music::app::Controller;
use herdr_music::cli::{self, AuthCmd, Requested};
use herdr_music::config::{self, AppConfig};
use herdr_music::provider::MusicProvider;
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
        Requested::Tui => {
            let cfg = AppConfig::load();
            let provider = spotify::provider_from_config(&cfg)?;
            tui::run(Box::new(provider), cfg.local_device())
        }
        Requested::Status => status(),
        Requested::Ctl(name) => ctl(&name),
        Requested::Auth(AuthCmd::Login(port)) => auth_login(port),
        Requested::Auth(AuthCmd::Status) => {
            match spotify::connection_status(&config::state_dir())? {
                true => println!("spotify: connected"),
                false => println!("spotify: not connected — run `herdr-music auth login`"),
            }
            Ok(())
        }
        Requested::Auth(AuthCmd::Logout) => {
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

fn provider() -> Result<spotify::SpotifyProvider> {
    Ok(spotify::provider_from_config(&AppConfig::load())?)
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
    let client_id = AppConfig::load().spotify_client_id.ok_or_else(|| {
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
}
