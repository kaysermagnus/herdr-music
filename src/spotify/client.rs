use crate::provider::{self, Playback, ProviderError, Track};
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
    /// Spotify always sends this; default to an hour so a missing field
    /// doesn't make the token instantly stale.
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
}

fn default_expires_in() -> u64 {
    3600
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

/// `GET /me/player/devices` payload: the Connect device list.
#[derive(Debug, Deserialize)]
pub struct DevicesResponse {
    #[serde(default)]
    devices: Vec<DeviceEntry>,
}

#[derive(Debug, Deserialize)]
struct DeviceEntry {
    id: String,
    name: String,
    #[serde(default)]
    is_active: bool,
}

pub fn devices_from(r: DevicesResponse) -> Vec<provider::Device> {
    r.devices
        .into_iter()
        .map(|d| provider::Device {
            id: d.id,
            name: d.name,
            is_active: d.is_active,
        })
        .collect()
}

/// Transfer body for `PUT /me/player`: move playback, keep it playing.
pub fn transfer_body(device_id: &str) -> serde_json::Value {
    serde_json::json!({"device_ids": [device_id], "play": true})
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
    fn missing_expires_in_defaults_to_one_hour() {
        let r: TokenResponse = serde_json::from_str(r#"{"access_token":"x"}"#).unwrap();
        assert_eq!(r.expires_in, 3600);
    }

    #[test]
    fn devices_response_maps_to_devices() {
        let r: DevicesResponse = serde_json::from_str(
            r#"{"devices": [
                {"id": "d1", "name": "Herdr Music", "is_active": true, "type": "Computer"},
                {"id": "d2", "name": "Kitchen", "is_active": false}
            ]}"#,
        )
        .unwrap();
        let devices = devices_from(r);
        assert_eq!(
            devices,
            vec![
                provider::Device {
                    id: "d1".into(),
                    name: "Herdr Music".into(),
                    is_active: true,
                },
                provider::Device {
                    id: "d2".into(),
                    name: "Kitchen".into(),
                    is_active: false,
                },
            ]
        );
        // An empty/absent list is fine too.
        let empty: DevicesResponse = serde_json::from_str("{}").unwrap();
        assert!(devices_from(empty).is_empty());
    }

    #[test]
    fn transfer_body_names_device_and_plays() {
        assert_eq!(
            transfer_body("d1"),
            serde_json::json!({"device_ids": ["d1"], "play": true})
        );
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
