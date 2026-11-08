use std::path::PathBuf;

const APP_DIR: &str = "herdr-music";
const CLIENT_ID_ENV: &str = "HERDR_MUSIC_SPOTIFY_CLIENT_ID";

/// Plugin config directory: `HERDR_PLUGIN_CONFIG_DIR` when running inside
/// Herdr, otherwise a platform config dir so `cargo run` works standalone.
pub fn config_dir() -> PathBuf {
    resolve_dir(
        std::env::var("HERDR_PLUGIN_CONFIG_DIR").ok(),
        dirs::config_dir(),
    )
}

/// Plugin state directory (tokens, durable state): `HERDR_PLUGIN_STATE_DIR`
/// inside Herdr, otherwise a platform state/data dir.
pub fn state_dir() -> PathBuf {
    resolve_dir(
        std::env::var("HERDR_PLUGIN_STATE_DIR").ok(),
        dirs::state_dir().or_else(dirs::data_local_dir),
    )
}

fn resolve_dir(env: Option<String>, fallback: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = env.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    fallback.unwrap_or_else(|| PathBuf::from(".")).join(APP_DIR)
}

/// Which music service adapter backs the TUI (`provider` in config.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Spotify,
    AirPlay,
}

impl ProviderKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "spotify" => Ok(Self::Spotify),
            "airplay" => Ok(Self::AirPlay),
            other => Err(format!(
                "unknown provider `{other}` in config.toml — expected \"spotify\" or \"airplay\""
            )),
        }
    }
}

const DEFAULT_AIRPLAY_NAME: &str = "Herdr Music";

#[derive(Debug)]
pub struct AppConfig {
    pub spotify_client_id: Option<String>,
    pub provider: ProviderKind,
    /// mDNS-advertised AirPlay receiver name.
    pub airplay_name: String,
}

impl AppConfig {
    /// Load `config.toml` inside the config dir plus env overrides.
    /// Client id resolution: env var first, then `spotify_client_id`.
    /// Unknown keys are ignored; an unknown `provider` value is an error.
    pub fn load() -> Result<Self, String> {
        let mut cfg = match std::fs::read_to_string(config_dir().join("config.toml")) {
            Ok(text) => Self::parse(&text)?,
            Err(_) => Self::parse("")?,
        };
        if let Ok(client_id) = std::env::var(CLIENT_ID_ENV) {
            if !client_id.is_empty() {
                cfg.spotify_client_id = Some(client_id);
            }
        }
        Ok(cfg)
    }

    /// Parse the hand-rolled subset of TOML we use: `key = "value"` lines,
    /// `#` comments. `Err` only for an unrecognized `provider` value.
    fn parse(toml_text: &str) -> Result<Self, String> {
        let provider = match value_of(toml_text, "provider") {
            Some(v) => ProviderKind::parse(&v)?,
            None => ProviderKind::Spotify,
        };
        Ok(Self {
            spotify_client_id: value_of(toml_text, "spotify_client_id"),
            provider,
            airplay_name: value_of(toml_text, "airplay_name")
                .unwrap_or_else(|| DEFAULT_AIRPLAY_NAME.to_string()),
        })
    }
}

/// First `key = "value"` line matching `key`; quotes stripped, empty → None.
fn value_of(toml_text: &str, wanted: &str) -> Option<String> {
    for line in toml_text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == wanted {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_dir_wins_over_fallback() {
        let resolved = resolve_dir(
            Some("/tmp/herdrcfg".into()),
            Some(PathBuf::from("/should/not/be/used")),
        );
        assert_eq!(resolved, PathBuf::from("/tmp/herdrcfg"));
    }

    #[test]
    fn fallback_joins_app_dir() {
        let resolved = resolve_dir(None, Some(PathBuf::from("/base")));
        assert_eq!(resolved, PathBuf::from("/base/herdr-music"));
    }

    #[test]
    fn empty_env_is_ignored() {
        let resolved = resolve_dir(Some("".into()), Some(PathBuf::from("/base")));
        assert_eq!(resolved, PathBuf::from("/base/herdr-music"));
    }

    #[test]
    fn parses_client_id_from_toml() {
        let text = "# comment\nspotify_client_id = \"abc123\"\nother = 1\n";
        assert_eq!(
            AppConfig::parse(text).unwrap().spotify_client_id,
            Some("abc123".into())
        );
    }

    #[test]
    fn missing_or_empty_client_id_is_none() {
        assert_eq!(
            AppConfig::parse("other = 1").unwrap().spotify_client_id,
            None
        );
        assert_eq!(
            AppConfig::parse("spotify_client_id = \"\"")
                .unwrap()
                .spotify_client_id,
            None
        );
    }

    #[test]
    fn provider_defaults_to_spotify() {
        let cfg = AppConfig::parse("other = 1").unwrap();
        assert_eq!(cfg.provider, ProviderKind::Spotify);
        let cfg = AppConfig::parse("provider = \"spotify\"").unwrap();
        assert_eq!(cfg.provider, ProviderKind::Spotify);
    }

    #[test]
    fn provider_airplay_parses() {
        let cfg = AppConfig::parse("provider = \"airplay\"").unwrap();
        assert_eq!(cfg.provider, ProviderKind::AirPlay);
    }

    #[test]
    fn unknown_provider_is_an_error() {
        let err = AppConfig::parse("provider = \"sonos\"").unwrap_err();
        assert!(err.contains("sonos"), "{err}");
        assert!(err.contains("provider"), "{err}");
    }

    #[test]
    fn airplay_name_defaults_and_overrides() {
        assert_eq!(
            AppConfig::parse("other = 1").unwrap().airplay_name,
            "Herdr Music"
        );
        assert_eq!(
            AppConfig::parse("airplay_name = \"Studio Mac\"")
                .unwrap()
                .airplay_name,
            "Studio Mac"
        );
    }
}
