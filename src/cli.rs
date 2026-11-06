use crate::app::Command;

pub const USAGE: &str = "herdr-music — TUI music controller

USAGE:
    herdr-music [tui]              Open the player TUI
    herdr-music status             Print the current track
    herdr-music ctl <command>      Run a transport command and exit
    herdr-music auth <command>     Manage provider login

CTL COMMANDS:
    play-pause play pause stop next previous forward rewind

AUTH COMMANDS:
    login [port]   Connect Spotify via OAuth (browser)
    status         Show connection state
    logout         Remove stored tokens";

#[derive(Debug, PartialEq, Eq)]
pub enum AuthCmd {
    Login(Option<u16>),
    Status,
    Logout,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Requested {
    Tui,
    Ctl(String),
    Auth(AuthCmd),
    Status,
    Help,
}

pub fn parse(args: &[String]) -> Result<Requested, String> {
    let err = || Err(format!("invalid arguments\n\n{USAGE}"));
    match args {
        [] => Ok(Requested::Tui),
        [cmd] => match cmd.as_str() {
            "tui" => Ok(Requested::Tui),
            "status" => Ok(Requested::Status),
            "-h" | "--help" | "help" => Ok(Requested::Help),
            _ => err(),
        },
        [cmd, sub] => match (cmd.as_str(), sub.as_str()) {
            ("ctl", name) if ctl_command(name).is_some() => Ok(Requested::Ctl(name.to_string())),
            ("ctl", _) => err(),
            ("auth", "login") => Ok(Requested::Auth(AuthCmd::Login(None))),
            ("auth", "status") => Ok(Requested::Auth(AuthCmd::Status)),
            ("auth", "logout") => Ok(Requested::Auth(AuthCmd::Logout)),
            _ => err(),
        },
        [cmd, sub, arg] => match (cmd.as_str(), sub.as_str()) {
            ("auth", "login") => arg
                .parse::<u16>()
                .map(|p| Requested::Auth(AuthCmd::Login(Some(p))))
                .map_err(|_| format!("invalid port `{arg}`\n\n{USAGE}")),
            _ => err(),
        },
        _ => err(),
    }
}

/// `ctl` name → controller command. `stop` maps to pause because Spotify's
/// API has no true stop.
pub fn ctl_command(name: &str) -> Option<Command> {
    Some(match name {
        "play-pause" | "toggle" => Command::Toggle,
        "play" => Command::Play,
        "pause" => Command::Pause,
        "stop" => Command::Stop,
        "next" => Command::Next,
        "previous" | "prev" => Command::Previous,
        "forward" => Command::SeekForward,
        "rewind" => Command::SeekBack,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_and_tui_open_the_player() {
        assert_eq!(parse(&args(&[])), Ok(Requested::Tui));
        assert_eq!(parse(&args(&["tui"])), Ok(Requested::Tui));
    }

    #[test]
    fn ctl_accepts_known_commands_only() {
        assert_eq!(
            parse(&args(&["ctl", "next"])),
            Ok(Requested::Ctl("next".into()))
        );
        assert!(parse(&args(&["ctl", "bogus"])).is_err());
        assert!(parse(&args(&["ctl"])).is_err());
    }

    #[test]
    fn auth_subcommands() {
        assert_eq!(
            parse(&args(&["auth", "login"])),
            Ok(Requested::Auth(AuthCmd::Login(None)))
        );
        assert_eq!(
            parse(&args(&["auth", "login", "9999"])),
            Ok(Requested::Auth(AuthCmd::Login(Some(9999))))
        );
        assert!(parse(&args(&["auth", "login", "abc"])).is_err());
        assert!(parse(&args(&["auth", "bogus"])).is_err());
    }

    #[test]
    fn ctl_names_map_to_commands() {
        assert_eq!(ctl_command("play-pause"), Some(Command::Toggle));
        assert_eq!(ctl_command("stop"), Some(Command::Stop));
        assert_eq!(ctl_command("forward"), Some(Command::SeekForward));
        assert_eq!(ctl_command("rewind"), Some(Command::SeekBack));
        assert_eq!(ctl_command("volume"), None);
    }
}
