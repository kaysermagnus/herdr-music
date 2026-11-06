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
        Requested::Tui => tui::run(Box::new(spotify::provider_from_config(&AppConfig::load())?)),
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
        Requested::Help => unreachable!("handled in run()"),
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
