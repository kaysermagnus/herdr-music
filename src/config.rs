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

pub struct AppConfig {
    pub spotify_client_id: Option<String>,
    pub local_playback: bool,
    pub local_device_name: String,
}

const DEFAULT_LOCAL_DEVICE_NAME: &str = "Herdr Music";

impl AppConfig {
    /// Client id resolution: env var first, then `spotify_client_id` in
    /// `config.toml` inside the config dir. `local_playback` and
    /// `local_device_name` come from the same file.
    pub fn load() -> Self {
        let mut spotify_client_id = std::env::var(CLIENT_ID_ENV).ok().filter(|s| !s.is_empty());
        let mut local_playback = false;
        let mut local_device_name = DEFAULT_LOCAL_DEVICE_NAME.to_string();
        if let Ok(text) = std::fs::read_to_string(config_dir().join("config.toml")) {
            if spotify_client_id.is_none() {
                spotify_client_id = parse_client_id(&text);
            }
            let (lp, name) = parse_local(&text);
            local_playback = lp.unwrap_or(false);
            if let Some(name) = name {
                local_device_name = name;
            }
        }
        Self {
            spotify_client_id,
            local_playback,
            local_device_name,
        }
    }

    /// The name the local engine registers, when local playback is on.
    pub fn local_device(&self) -> Option<String> {
        self.local_playback.then(|| self.local_device_name.clone())
    }
}

fn parse_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (k, v) = line.split_once('=')?;
    (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\''))
}

fn parse_local(toml_text: &str) -> (Option<bool>, Option<String>) {
    let mut playback = None;
    let mut name = None;
    for line in toml_text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(v) = parse_value(line, "local_playback") {
            playback = Some(v == "true");
        }
        if let Some(v) = parse_value(line, "local_device_name") {
            if !v.is_empty() {
                name = Some(v.to_string());
            }
        }
    }
    (playback, name)
}

fn parse_client_id(toml_text: &str) -> Option<String> {
    for line in toml_text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "spotify_client_id" {
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
        assert_eq!(parse_client_id(text), Some("abc123".into()));
    }

    #[test]
    fn missing_or_empty_client_id_is_none() {
        assert_eq!(parse_client_id("other = 1"), None);
        assert_eq!(parse_client_id("spotify_client_id = \"\""), None);
    }

    #[test]
    fn local_playback_keys_parse_with_defaults() {
        assert_eq!(parse_local("other = 1"), (None, None));
        assert_eq!(parse_local("local_playback = false"), (Some(false), None));
        assert_eq!(
            parse_local("local_playback = true\nlocal_device_name = \"Desk\""),
            (Some(true), Some("Desk".into()))
        );
        // Unknown values and comments don't count.
        assert_eq!(
            parse_local("# local_playback = false\nlocal_device_name = \"\""),
            (None, None)
        );
    }
}
