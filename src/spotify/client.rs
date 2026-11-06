use crate::provider::{Playback, ProviderError, Track};
use serde::Deserialize;
use std::time::Duration;

/// API base URLs. Fields are injectable so tests can point the adapter at a
/// stub server on localhost — the adapter's internal seam.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub accounts: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: "https://api.spotify.com/v1".into(),
            accounts: "https://accounts.spotify.com".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct PlayerResponse {
    #[serde(default)]
    is_playing: bool,
    #[serde(default)]
    progress_ms: Option<u64>,
    #[serde(default)]
    device: Option<Device>,
    #[serde(default)]
    item: Option<Item>,
}

#[derive(Debug, Deserialize)]
struct Device {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Item {
    name: String,
    duration_ms: u64,
    album: Album,
    artists: Vec<Artist>,
}

#[derive(Debug, Deserialize)]
struct Album {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Artist {
    name: String,
}

/// Map the `/me/player` payload onto our domain type. `item` is null when
/// nothing is playing or a non-track (ad, podcast episode) occupies the
/// player — both read as "nothing playing".
pub fn playback_from(r: PlayerResponse) -> Option<Playback> {
    r.item.map(|item| Playback {
        track: Track {
            title: item.name,
            artists: item.artists.into_iter().map(|a| a.name).collect(),
            album: item.album.name,
            duration: Duration::from_millis(item.duration_ms),
        },
        progress: Duration::from_millis(r.progress_ms.unwrap_or(0)),
        is_playing: r.is_playing,
        device: r.device.map(|d| d.name),
    })
}

/// Turn an HTTP status into a typed error. 2xx is handled by the caller.
pub fn error_for_status(status: u16, body: &str) -> ProviderError {
    match status {
        401 => ProviderError::NotAuthenticated,
        403 => ProviderError::PremiumRequired,
        404 => ProviderError::NoActiveDevice,
        code => ProviderError::Api(format!("spotify error {code}: {}", truncate(body))),
    }
}

fn truncate(body: &str) -> String {
    let body = body.trim();
    if body.len() > 200 {
        format!("{}…", &body[..200])
    } else {
        body.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYER_JSON: &str = r#"{
        "is_playing": true,
        "progress_ms": 61234,
        "device": {"name": "Cesar's Mac"},
        "item": {
            "name": "The Song",
            "duration_ms": 201500,
            "album": {"name": "The Album"},
            "artists": [{"name": "A1"}, {"name": "A2"}]
        }
    }"#;

    #[test]
    fn maps_player_response_to_playback() {
        let r: PlayerResponse = serde_json::from_str(PLAYER_JSON).unwrap();
        let pb = playback_from(r).unwrap();
        assert_eq!(pb.track.title, "The Song");
        assert_eq!(pb.track.artists, vec!["A1", "A2"]);
        assert_eq!(pb.track.album, "The Album");
        assert_eq!(pb.track.duration, Duration::from_millis(201500));
        assert_eq!(pb.progress, Duration::from_millis(61234));
        assert!(pb.is_playing);
        assert_eq!(pb.device.as_deref(), Some("Cesar's Mac"));
    }

    #[test]
    fn null_item_is_nothing_playing() {
        let r: PlayerResponse =
            serde_json::from_str(r#"{"is_playing": false, "item": null}"#).unwrap();
        assert!(playback_from(r).is_none());
    }

    #[test]
    fn status_maps_to_typed_errors() {
        assert_eq!(error_for_status(401, ""), ProviderError::NotAuthenticated);
        assert_eq!(error_for_status(403, ""), ProviderError::PremiumRequired);
        assert_eq!(error_for_status(404, ""), ProviderError::NoActiveDevice);
        assert!(matches!(
            error_for_status(500, "boom"),
            ProviderError::Api(_)
        ));
    }
}
